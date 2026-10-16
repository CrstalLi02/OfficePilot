//! 技能管理：列表、查看、启用 / 停用、导入（zip / SKILL.md）、删除、从对话生成技能草稿

use axum::extract::{Multipart, Path};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::db::session_repo;
use crate::error::AppError;
use crate::skills::{self, Source};
use crate::state;

pub fn router() -> Router {
    Router::new()
        .route("/api/skills", get(list).post(create_from_markdown))
        .route("/api/skills/import", post(import_upload))
        .route("/api/skills/draft", post(draft_from_session))
        .route("/api/skills/:name", get(detail).delete(remove))
        .route("/api/skills/:name/enabled", post(set_enabled))
}

fn summary(s: &skills::Skill) -> Value {
    json!({
        "name": s.meta.name,
        "description": s.meta.description,
        "source": s.source,
        "enabled": s.enabled,
        "category": s.meta.category(),
        "icon": s.meta.icon(),
        "user_invocable": s.meta.user_invocable,
        "disable_model_invocation": s.meta.disable_model_invocation,
        "files": s.files,
    })
}

async fn list(user: AuthUser) -> Result<Json<Value>, AppError> {
    let all = skills::load_all(&state::db_pool(), &user.0.id).await?;
    Ok(Json(json!({ "skills": all.iter().map(summary).collect::<Vec<_>>() })))
}

async fn detail(user: AuthUser, Path(name): Path<String>) -> Result<Json<Value>, AppError> {
    let skill = skills::find(&state::db_pool(), &user.0.id, &name)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("技能 {name} 不存在")))?;
    let source_md = skills::read_file(&user.0.id, &skill, "SKILL.md").unwrap_or_default();
    let mut value = summary(&skill);
    value["skill_md"] = json!(source_md);
    Ok(Json(value))
}

#[derive(Deserialize)]
struct EnabledPayload {
    enabled: bool,
}

async fn set_enabled(user: AuthUser, Path(name): Path<String>, Json(payload): Json<EnabledPayload>) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    if skills::find(&pool, &user.0.id, &name).await?.is_none() {
        return Err(AppError::NotFound(format!("技能 {name} 不存在")));
    }
    skills::set_enabled(&pool, &user.0.id, &name, payload.enabled).await?;
    Ok(Json(json!({ "name": name, "enabled": payload.enabled })))
}

#[derive(Deserialize)]
struct MarkdownPayload {
    skill_md: String,
}

/// 直接用 SKILL.md 文本创建（或覆盖同名的用户技能）
async fn create_from_markdown(user: AuthUser, Json(payload): Json<MarkdownPayload>) -> Result<Json<Value>, AppError> {
    let meta = skills::import_markdown(&user.0.id, &payload.skill_md).map_err(AppError::BadRequest)?;
    Ok(Json(json!({ "name": meta.name, "description": meta.description })))
}

/// 上传 .zip（含 SKILL.md 与资源文件）或单个 SKILL.md
async fn import_upload(user: AuthUser, mut multipart: Multipart) -> Result<Json<Value>, AppError> {
    let field = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
        .ok_or_else(|| AppError::BadRequest("没有上传文件".into()))?;
    let filename = field.file_name().unwrap_or("").to_lowercase();
    let bytes = field.bytes().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let meta = if filename.ends_with(".zip") || bytes.starts_with(b"PK\x03\x04") {
        skills::import_zip(&user.0.id, &bytes)
    } else {
        skills::import_markdown(&user.0.id, &String::from_utf8_lossy(&bytes))
    }
    .map_err(AppError::BadRequest)?;
    Ok(Json(json!({ "name": meta.name, "description": meta.description })))
}

async fn remove(user: AuthUser, Path(name): Path<String>) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    match skills::find(&pool, &user.0.id, &name).await? {
        Some(s) if s.source == Source::User => {
            skills::delete_user_skill(&user.0.id, &name)?;
            Ok(Json(json!({ "deleted": name })))
        }
        Some(_) => Err(AppError::BadRequest("内置技能不能删除，可以停用".into())),
        None => Err(AppError::NotFound(format!("技能 {name} 不存在"))),
    }
}

#[derive(Deserialize)]
struct DraftPayload {
    session_id: String,
    #[serde(default)]
    hint: Option<String>,
}

const DRAFT_PROMPT: &str = r#"你是技能编写助手。根据下面这段对话中用户完成任务的做法，提炼成一个可复用的 Agent 技能，输出完整的 SKILL.md（只输出文件内容，不要代码块包裹）：
---
name: 小写字母、数字、连字符组成的英文名称
description: 一句话说明这个技能做什么、什么时候使用（中文，100 字以内）
metadata: {"walioffice": {"category": "写作 / 数据 / 产品 / 研究 / 演示 / 求职 / 其他 中选一个"}}
---

# 标题
## 何时使用
## 流程（编号步骤，写明每步用哪个工具）
## 质量标准
要求：只总结对话里真实出现的做法和用户偏好（格式、语气、篇幅、必须包含的内容），不要编造；去掉一次性的具体数据。"#;

/// 从对话生成技能草稿（不保存，返回给用户预览修改）
async fn draft_from_session(user: AuthUser, Json(payload): Json<DraftPayload>) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let session = session_repo::find_by_id(&pool, &payload.session_id)
        .await?
        .ok_or_else(|| AppError::NotFound("会话不存在".into()))?;
    if session.owner_id != user.0.id {
        return Err(AppError::Forbidden);
    }
    let messages = session_repo::get_messages(&pool, &payload.session_id, 40).await?;
    if messages.is_empty() {
        return Err(AppError::BadRequest("这个会话还没有内容".into()));
    }
    let transcript: String = messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .map(|m| format!("[{}] {}", if m.role == "user" { "用户" } else { "助手" }, m.content.chars().take(1500).collect::<String>()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let hint = payload.hint.as_deref().map(|h| format!("\n用户的补充要求：{h}")).unwrap_or_default();
    let client = crate::llm::LlmClient::for_user(&user.0.id, None).await;
    let chat = |role: &str, content: String| crate::models::ChatMessage { role: role.into(), content, tool_calls: None, tool_call_id: None };
    let resp = client
        .chat(&[chat("system", DRAFT_PROMPT.to_string()), chat("user", format!("对话记录：\n{transcript}{hint}"))], None)
        .await
        .map_err(|e| AppError::Llm(e.to_string()))?;
    let draft = resp
        .choices
        .first()
        .and_then(|c| c.message.content.clone())
        .unwrap_or_default()
        .trim()
        .trim_start_matches("```markdown")
        .trim_start_matches("```md")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string();
    // 草稿不合法时也返回，交给用户修改；附上校验结果
    let validation = skills::parse_skill_md(&draft).err();
    Ok(Json(json!({ "skill_md": draft, "error": validation })))
}
