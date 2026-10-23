//! 用编辑器套件打开工作区文件（桌面版）。网页 / Docker 部署只返回「需要桌面版」。

use axum::extract::Path;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::db::file_repo;
use crate::editor;
use crate::error::AppError;
use crate::state;

pub fn router() -> Router {
    Router::new()
        .route("/api/editor", get(status))
        .route("/api/files/:id/open-in-editor", post(open_in_editor))
        .route("/api/artifacts/:artifact_id/open-in-editor", post(open_artifact_in_editor))
        .route("/api/editor/ai-sync", get(ai_sync_status).post(set_ai_sync))
}

fn ai_sync_json() -> Value {
    let paths = crate::suite_ai::paths();
    let state = paths.as_ref().map(|(_, s)| crate::suite_ai::load_state(s)).unwrap_or_default();
    json!({
        "available": editor::enabled() && editor::suite_app().is_some(),
        "enabled": state.enabled,
        "profile_name": state.profile_name,
        "model": state.model,
        "settings_file": paths.map(|(f, _)| f.to_string_lossy().into_owned()),
    })
}

/// 编辑器套件是否共用 OfficePilot 的模型
async fn ai_sync_status(_user: AuthUser) -> Result<Json<Value>, AppError> {
    Ok(Json(ai_sync_json()))
}

#[derive(Deserialize)]
struct AiSyncPayload {
    enabled: bool,
}

/// 开启 / 关闭共用（只在桌面版、由用户点击时执行：会改写编辑器套件的设置文件）
async fn set_ai_sync(user: AuthUser, Json(payload): Json<AiSyncPayload>) -> Result<Json<Value>, AppError> {
    if !editor::enabled() {
        return Err(AppError::BadRequest("共用模型配置需要桌面版".into()));
    }
    let (suite_file, state_file) = crate::suite_ai::paths().ok_or_else(|| AppError::BadRequest("找不到用户主目录".into()))?;
    let pool = state::db_pool();
    if payload.enabled {
        crate::suite_ai::enable(&pool, &user.0.id, &suite_file, &state_file).await.map_err(AppError::BadRequest)?;
        tracing::info!("[SuiteAI] 编辑器套件开始共用 OfficePilot 的模型");
    } else {
        crate::suite_ai::disable(&suite_file, &state_file).await.map_err(AppError::BadRequest)?;
        tracing::info!("[SuiteAI] 编辑器套件不再共用 OfficePilot 的模型");
    }
    Ok(Json(ai_sync_json()))
}

/// 能导出成可编辑文件的产物
const EDITABLE_ARTIFACTS: &[&str] = &["document", "markdown", "sheet", "ppt"];

/// 编辑器是否可用、在哪里，以及正在用编辑器打开的文件
async fn status(user: AuthUser) -> Result<Json<Value>, AppError> {
    let enabled = editor::enabled();
    let app = if enabled { editor::suite_app() } else { None };
    let open = if enabled { editor::list_open(&editor::open_root(), &user.0.id).await } else { Vec::new() };
    Ok(Json(json!({
        "enabled": enabled,
        "available": app.is_some(),
        "app": app.map(|p| p.to_string_lossy().into_owned()),
        "supported": editor::SUPPORTED,
        "open": open,
        // 智能助手能用的编辑器套件工具
        "agent_tools": if crate::suite_cli::available() { vec!["转换格式", "填写模板", "文档体检", "渲染页面", "读取编辑器选区", "在编辑器中打开"] } else { vec![] },
    })))
}

fn check_available() -> Result<std::path::PathBuf, AppError> {
    if !editor::enabled() {
        return Err(AppError::BadRequest("用编辑器打开需要桌面版".into()));
    }
    editor::suite_app().ok_or_else(|| AppError::BadRequest("没有找到编辑器套件（OfficePilot Suite），请先运行 scripts/build-suite.sh 构建".into()))
}

async fn open_file(app: &std::path::Path, owner_id: &str, file: &file_repo::FileRow) -> Result<std::path::PathBuf, AppError> {
    if !editor::supported(&file.name) {
        return Err(AppError::BadRequest(format!("编辑器不支持 {}，可以用默认应用打开", file.name)));
    }
    let working = editor::prepare_copy(&editor::open_root(), owner_id, file).await?;
    editor::launch(app, &working).await?;
    tracing::info!("[Editor] 用编辑器打开 {}", file.name);
    Ok(working)
}

async fn open_in_editor(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let app = check_available()?;
    let pool = state::db_pool();
    let file = file_repo::get_file(&pool, &user.0.id, &id).await?.ok_or_else(|| AppError::NotFound("文件不存在".into()))?;
    let working = open_file(&app, &user.0.id, &file).await?;
    Ok(Json(json!({ "opened": true, "path": working })))
}

#[derive(Deserialize)]
struct ArtifactPayload {
    session_id: String,
}

/// 会话产物：先导出到工作区根目录（可撤销），再用编辑器打开导出的文件
async fn open_artifact_in_editor(user: AuthUser, Path(artifact_id): Path<String>, Json(payload): Json<ArtifactPayload>) -> Result<Json<Value>, AppError> {
    let app = check_available()?;
    let pool = state::db_pool();
    let artifact = super::artifact::load_owned(&pool, &user, &payload.session_id, &artifact_id).await?;
    if !EDITABLE_ARTIFACTS.contains(&artifact.kind.as_str()) {
        return Err(AppError::BadRequest("这类产物不能用编辑器打开".into()));
    }
    let (path, op_id) = crate::workspace::export_artifact(&pool, &user.0.id, &artifact, "/", None, Some(&payload.session_id)).await?;
    let tree = crate::workspace::Tree::load(&pool, &user.0.id).await?;
    let file = tree.find_file(&path).map_err(AppError::BadRequest)?.cloned().ok_or_else(|| AppError::NotFound("导出的文件没有找到".into()))?;
    let working = open_file(&app, &user.0.id, &file).await?;
    Ok(Json(json!({ "opened": true, "path": working, "file_path": path, "file_id": file.id, "op_id": op_id })))
}
