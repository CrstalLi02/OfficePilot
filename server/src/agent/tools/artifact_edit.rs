use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::artifact_ops::{apply_ops, edit_view, is_editable, op_guide};
use crate::agent::tool::{ArtifactRevision, OfficeTool, ToolContext, ToolResult};
use crate::db::project_repo;
use crate::llm::LlmClient;
use crate::models::{Artifact, ChatMessage, Slide};

/// 局部修改已有产物：只重写被要求改动的部分，产物 id 不变、版本号 +1
pub struct ArtifactEditTool;

const EDIT_SYSTEM_PROMPT: &str = r#"你是办公产物编辑器。根据用户的修改要求，对给定产物做最小必要的修改：只改要求涉及的部分，其余内容保持原样。
只返回一个 JSON 对象，不要输出其他文字：
{"ops":[...操作...],"summary":"一句话说明改了什么，例如：第 3 页改为柱状图对比"}
编号都指下面视图里显示的原始编号，多个操作的先后顺序由系统处理。"#;

fn chat_message(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content,
        tool_calls: None,
        tool_call_id: None,
    }
}

fn find_artifact<'a>(artifacts: &'a [Artifact], key: &str) -> Option<&'a Artifact> {
    let key = key.trim();
    artifacts
        .iter()
        .find(|a| a.id == key)
        .or_else(|| artifacts.iter().find(|a| a.title == key))
}

/// PPT 的幻灯片同时存在项目文件里（预览、导出都读它），编辑后同步写回；history_title 记入项目历史
pub(crate) fn sync_ppt_project(content: &Value, title: &str, history_title: &str) -> Result<(), String> {
    let Some(project_id) = content.get("project_id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(mut project) = project_repo::load_ppt_project(project_id).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    project.slides = serde_json::from_value::<Vec<Slide>>(content["slides"].clone()).map_err(|e| e.to_string())?;
    project.title = title.to_string();
    project.updated_at = chrono::Utc::now().to_rfc3339();
    project.history.get_or_insert_with(Vec::new).push(json!({
        "type": "edit",
        "title": history_title,
        "created_at": project.updated_at,
    }));
    project_repo::save_ppt_project(&project).map_err(|e| e.to_string())
}

#[async_trait]
impl OfficeTool for ArtifactEditTool {
    fn name(&self) -> &str {
        "artifact_edit"
    }

    fn description(&self) -> &str {
        "局部修改会话里已有的产物（Markdown、Word 文档、PPT、表格、图表、draw.io）：改某一章或某一页、增删内容、改单元格、换图表类型、调整措辞等。只重写被要求改动的部分，产物保留原 id 并生成新版本，可回看和恢复旧版本。用户说“把第 3 页改成…”“这份文档加一节…”“表格里预算改成…”时用它，不要重新生成整份产物。图片和视频不支持，需要时用对应的生成工具参考原图重新生成。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "artifact_id": { "type": "string", "description": "要修改的产物 id（见系统提示中的产物清单）" },
                "instruction": { "type": "string", "description": "具体修改要求，尽量写清楚位置和目标，例如“第 3 页改为三列对比，突出成本下降”" }
            },
            "required": ["artifact_id", "instruction"]
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["artifact_id"].as_str().unwrap_or("");
        let instruction = input["instruction"].as_str().unwrap_or("").trim();
        if instruction.is_empty() {
            return ToolResult::err("缺少修改要求 instruction");
        }
        let artifacts = ctx.artifacts();
        let Some(base) = find_artifact(&artifacts, key) else {
            let available = artifacts
                .iter()
                .map(|a| format!("{}（{}《{}》）", a.id, a.kind, a.title))
                .collect::<Vec<_>>()
                .join("；");
            return ToolResult::err(format!(
                "找不到产物 {key}。当前会话的产物：{}",
                if available.is_empty() { "无" } else { &available }
            ));
        };
        if !is_editable(&base.kind) {
            return ToolResult::err(format!(
                "{} 类型的产物不支持局部编辑，请用对应的生成工具参考原产物重新生成",
                base.kind
            ));
        }

        ctx.send(
            "state_update",
            json!({
                "phase": "running",
                "step": "局部编辑",
                "detail": format!("正在定位《{}》中要修改的部分", base.title),
                "at": chrono::Utc::now().to_rfc3339(),
            }),
        );

        let client = LlmClient::for_user(&ctx.user_id, ctx.preferred_model.as_deref()).await;
        let mut messages = vec![
            chat_message("system", format!("{EDIT_SYSTEM_PROMPT}\n\n{}", op_guide(&base.kind))),
            chat_message(
                "user",
                format!("修改要求：{instruction}\n\n当前产物（{}，v{}）：\n{}", base.kind, base.version, edit_view(base)),
            ),
        ];

        // 解析或应用失败时，把错误交回模型修正一次
        let mut last_error = String::new();
        for attempt in 0..2 {
            let reply = match client.chat(&messages, None).await {
                Ok(resp) => resp
                    .choices
                    .first()
                    .and_then(|c| c.message.content.clone())
                    .unwrap_or_default(),
                Err(e) => return ToolResult::err(format!("编辑模型调用失败: {e}")),
            };
            let outcome = LlmClient::extract_json(&reply)
                .map_err(|e| e.to_string())
                .and_then(|value| {
                    let ops = value["ops"].as_array().cloned().unwrap_or_default();
                    let summary = value["summary"].as_str().unwrap_or("").trim().to_string();
                    apply_ops(base, &ops).map(|edited| (edited, summary, ops.len()))
                });
            match outcome {
                Ok((edited, summary, op_count)) => {
                    let summary = if summary.is_empty() {
                        instruction.chars().take(40).collect()
                    } else {
                        summary
                    };
                    if base.kind == "ppt" {
                        if let Err(e) = sync_ppt_project(&edited.content, &edited.title, &format!("AI 修改：{summary}")) {
                            tracing::warn!("[ArtifactEdit] 同步 PPT 项目失败: {e}");
                        }
                    }
                    let observation = format!(
                        "已修改《{}》（{} 个操作，v{} → v{}）：{summary}",
                        edited.title,
                        op_count,
                        base.version,
                        base.version + 1
                    );
                    return ToolResult::revised(
                        observation,
                        ArtifactRevision {
                            base: base.clone(),
                            title: edited.title,
                            content: edited.content,
                            change_summary: summary,
                        },
                    );
                }
                Err(e) => {
                    tracing::warn!("[ArtifactEdit] 第 {} 次修改无法应用: {e}", attempt + 1);
                    last_error = e.clone();
                    messages.push(chat_message("assistant", reply));
                    messages.push(chat_message(
                        "user",
                        format!("上面的修改无法应用：{e}。请修正后重新返回完整的 JSON（只返回 JSON）。"),
                    ));
                }
            }
        }
        ToolResult::err(format!("修改未能应用：{last_error}"))
    }
}
