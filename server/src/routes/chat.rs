use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::post;
use axum::{Json, Router};
use futures::stream::Stream;
use std::collections::HashSet;
use std::convert::Infallible;
use std::time::Duration;

use crate::agent::agent_loop::AgentStart;
use crate::agent::plan::{AgentMode, Decision};
use crate::agent::{run_agent_loop, AgentConfig, AgentEvent};
use crate::auth::middleware::AuthUser;
use crate::db::{agent_run_repo, artifact_repo, session_repo};
use crate::error::AppError;
use crate::models::{Artifact, ChatAttachment, ChatMessage, ChatRequest};
use crate::state;

pub fn router() -> Router {
    Router::new()
        .route("/api/chat/stream", post(chat_stream))
        .route("/api/chat/cancel", post(chat_cancel))
}

#[derive(serde::Deserialize)]
struct CancelRequest {
    session_id: String,
}

/// 停止某个会话正在运行的 Agent
async fn chat_cancel(
    user: AuthUser,
    Json(req): Json<CancelRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let cancelled = crate::tasks::running_for_session(&req.session_id, &user.0.id)
        .is_some_and(|task_id| crate::tasks::cancel(&task_id, &user.0.id));
    Ok(Json(serde_json::json!({ "cancelled": cancelled })))
}

fn looks_like_markdown_document(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed.starts_with('#')
        || trimmed.contains("\n## ")
        || trimmed.contains("\n- ")
        || trimmed.contains("\n1. ")
        || trimmed.contains("```")
        || trimmed.contains("\n> ")
        || trimmed.contains("\n|")
}

fn build_summary_markdown_artifact(summary: &str, tool_kind: Option<&str>) -> Artifact {
    let now = chrono::Utc::now().to_rfc3339();
    Artifact {
        id: uuid::Uuid::new_v4().to_string(),
        kind: "markdown".into(),
        tool_kind: tool_kind.unwrap_or("doc").to_string(),
        title: "对话整理结果".into(),
        status: "ready".into(),
        content: serde_json::json!({
            "type": "markdown",
            "markdown": summary,
            "source": "assistant_summary",
        }),
        version: 1,
        created_at: now.clone(),
        updated_at: now,
        change_summary: None,
    }
}

fn format_attachment_context(attachments: &[ChatAttachment]) -> String {
    if attachments.is_empty() {
        return String::new();
    }

    let mut image_attachment_count = 0usize;
    let sections = attachments
        .iter()
        .enumerate()
        .filter_map(|(index, attachment)| {
            if attachment.kind == "text" {
                let text = attachment
                    .text_content
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(12_000)
                    .collect::<String>();

                format!(
                    "附件 {}（文本）\n- 文件名：{}\n- MIME：{}\n- 大小：{} 字节\n- 正文开始\n{}\n- 正文结束",
                    index + 1,
                    attachment.name,
                    attachment.mime_type,
                    attachment.size,
                    text
                )
                .into()
            } else {
                image_attachment_count += 1;
                let has_inline_image = attachment
                    .data_url
                    .as_deref()
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false);

                if has_inline_image {
                    let ocr_section = crate::image_ocr::extract_text_from_attachment(attachment)
                        .ok()
                        .flatten()
                        .map(|text| text.chars().take(1_200).collect::<String>())
                        .filter(|text| !text.trim().is_empty())
                        .map(|text| format!(
                            "附件 {}（图片）补充 OCR\n- 文件名：{}\n- OCR 提示开始\n{}\n- OCR 提示结束",
                            index + 1,
                            attachment.name,
                            text
                        ));

                    ocr_section.or_else(|| {
                        Some(format!(
                            "附件 {}（图片）\n- 文件名：{}\n- MIME：{}\n- 大小：{} 字节\n- 说明：图片内容已作为视觉输入随本轮消息一并发送，请直接观察图片回答用户问题。",
                            index + 1,
                            attachment.name,
                            attachment.mime_type,
                            attachment.size,
                        ))
                    })
                } else {
                    let ocr_text = crate::image_ocr::extract_text_from_attachment(attachment)
                        .ok()
                        .flatten()
                        .map(|text| text.chars().take(8_000).collect::<String>());
                    let ocr_section = ocr_text
                        .map(|text| format!("\n- OCR 提取文字开始\n{}\n- OCR 提取文字结束", text))
                        .unwrap_or_default();

                    Some(
                        format!(
                            "附件 {}（图片）\n- 文件名：{}\n- MIME：{}\n- 大小：{} 字节\n- 说明：当前仅收到图片附件元信息，尚未附带可供模型识别的图片内容；如需精确识别，请补充图片中的文字说明。",
                            index + 1,
                            attachment.name,
                            attachment.mime_type,
                            attachment.size,
                        ) + &ocr_section
                    )
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let image_note = if image_attachment_count > 0 {
        format!(
            "用户本次还上传了 {} 张图片；服务端会把带 data URL 的图片作为视觉输入发送给模型，请优先直接结合图像内容回答。",
            image_attachment_count
        )
    } else {
        String::new()
    };

    format!(
        "用户本次还上传了 {} 个附件，请将它们视作本轮对话输入的一部分，并优先结合附件内容回答。若用户这轮提问使用“这是什么”“这张图”“这里写了什么”“图里内容”等指代性表达，默认就是在询问这些附件，尤其是图片内容。{}\n\n{}",
        attachments.len(),
        if image_note.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", image_note)
        },
        sections
    )
}

pub fn build_user_message(req: &ChatRequest) -> String {
    let base = req.message.trim();
    let attachment_context = req
        .attachments
        .as_deref()
        .map(format_attachment_context)
        .unwrap_or_default();

    match (base.is_empty(), attachment_context.is_empty()) {
        (false, true) => base.to_string(),
        (true, false) => attachment_context,
        (false, false) => format!("{base}\n\n{attachment_context}"),
        (true, true) => String::new(),
    }
}

fn allowed_tools_for_kind(tool_kind: Option<&str>) -> Option<Vec<String>> {
    match tool_kind {
        Some("image") => Some(vec!["image_prompt".to_string()]),
        Some("video") => Some(vec![
            "video_generate".to_string(),
            "video_storyboard".to_string(),
            "video_batch_generate".to_string(),
        ]),
        _ => None,
    }
}

fn merge_session_artifacts(existing: Vec<Artifact>, current_turn: Vec<Artifact>) -> Vec<Artifact> {
    let mut merged = existing;

    for artifact in current_turn {
        if let Some(index) = merged.iter().position(|item| item.id == artifact.id) {
            merged[index] = artifact;
        } else {
            merged.insert(0, artifact);
        }
    }

    merged
}


/// 一次请求的工具路由结果：决定本轮暴露给 LLM 的工具和追加的系统提示词
pub struct Routing {
    pub resolved_tool_kind: Option<String>,
    pub mode: AgentMode,
    /// 追加到 OFFICE_AGENT_PROMPT 之后的内容（空串表示不追加）
    pub system_prompt: String,
    pub allowed_tools: Option<Vec<String>>,
}

/// 路由决策（chat_stream 与离线评测共用，保证评测的就是线上逻辑）。
/// 不再做关键词意图识别：工具选择交给模型，只有用户在界面上显式选择的
/// 图片/视频模式和工作模式（Craft/Plan/Ask）会约束可用工具。
pub fn resolve_routing(req: &ChatRequest) -> Routing {
    let mode = req.mode.unwrap_or_default();
    Routing {
        resolved_tool_kind: req.tool_kind.clone(),
        mode,
        system_prompt: mode.prompt().to_string(),
        allowed_tools: allowed_tools_for_kind(req.tool_kind.as_deref()),
    }
}

async fn chat_stream(
    user: AuthUser,
    Json(req): Json<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let started = start_chat_task(&user.0.id, req, crate::tasks::Origin::Chat).await?;
    task_sse(&started.task_id, &user.0.id, 0)
}

/// 订阅任务事件的 SSE：先回放 after 之后的历史事件，再推送实时事件，直到 done / error
pub fn task_sse(
    task_id: &str,
    owner_id: &str,
    after: u64,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let (backlog, mut rx, finished) = crate::tasks::subscribe(task_id, owner_id, after)
        .ok_or_else(|| AppError::NotFound("任务不存在或已结束".into()))?;
    let task_id = task_id.to_string();
    let to_sse = |e: &crate::tasks::TaskEvent| {
        Ok::<_, Infallible>(Event::default().id(e.seq.to_string()).event(&e.event).data(e.data.to_string()))
    };
    let stream = async_stream::stream! {
        let mut last = after;
        let mut ended = finished;
        for e in &backlog {
            last = e.seq;
            ended |= e.is_terminal();
            yield to_sse(e);
        }
        while !ended {
            match rx.recv().await {
                Ok(e) => {
                    if e.seq <= last {
                        continue;
                    }
                    last = e.seq;
                    ended = e.is_terminal();
                    yield to_sse(&e);
                }
                // 消费太慢被广播丢弃：从回放缓冲补齐
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    for e in crate::tasks::events_after(&task_id, last) {
                        last = e.seq;
                        ended |= e.is_terminal();
                        yield to_sse(&e);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// 发起的任务
pub struct StartedTask {
    pub task_id: String,
    pub session_id: String,
}

/// 发起一次 Agent 运行（对话、定时任务、IM 共用）：准备好上下文后在后台执行，立即返回任务 id。
/// 运行与 HTTP 连接无关，进度通过 tasks 模块订阅。
pub async fn start_chat_task(
    owner_id: &str,
    req: ChatRequest,
    origin: crate::tasks::Origin,
) -> Result<StartedTask, AppError> {
    let pool = state::db_pool();

    let Routing {
        resolved_tool_kind,
        mode,
        system_prompt,
        allowed_tools,
    } = resolve_routing(&req);
    if req.resume.is_some() && req.session_id.is_none() {
        return Err(AppError::BadRequest("确认计划需要指定会话".into()));
    }

    let client = std::sync::Arc::new(crate::llm::LlmClient::for_user(
        &owner_id,
        req.model.as_deref(),
    ).await);

    // 创建或获取会话
    let session = if let Some(ref sid) = req.session_id {
        session_repo::find_by_id(&pool, sid).await?.ok_or(AppError::NotFound("会话不存在".into()))?
    } else {
        let title_source = if req.message.trim().is_empty() {
            req.attachments
                .as_deref()
                .and_then(|items| items.first())
                .map(|item| format!("围绕附件：{}", item.name))
                .unwrap_or_else(|| "新的办公对话".to_string())
        } else {
            req.message.clone()
        };
        let title: String = title_source.chars().take(30).collect();
        session_repo::create(
            &pool,
            &owner_id,
            req.project_id.as_deref(),
            resolved_tool_kind.as_deref(),
            &title,
        ).await?
    };

    let session_id = session.id.clone();

    // 加载历史消息
    let history = session_repo::get_messages(&pool, &session_id, 50).await.unwrap_or_default();
    // 读取失败必须报错：若当作空列表继续，本轮保存时会覆盖掉会话里已有的产物
    let existing_artifacts = session_repo::get_artifacts(&pool, &session_id).await?;

    if let Some(attachments) = req.attachments.as_deref() {
        crate::file_store::save_chat_attachments_to_files(&pool, &owner_id, attachments).await;
    }

    // 计划确认：校验并锁定那次等待中的运行（重复提交会被拒绝）
    let resumed = match req.resume.as_ref() {
        Some(decision) => {
            let run = agent_run_repo::find(&pool, &decision.run_id)
                .await?
                .ok_or(AppError::NotFound("计划不存在或已过期".into()))?;
            if run.owner_id != owner_id || run.session_id != session_id {
                return Err(AppError::Forbidden);
            }
            let status = match decision.decision {
                Decision::Approve => "approved",
                Decision::Revise => "revised",
                Decision::Reject => "rejected",
            };
            if !agent_run_repo::resolve(&pool, &run.id, status).await? {
                return Err(AppError::BadRequest("这个计划已经处理过了".into()));
            }
            Some((run, decision.clone()))
        }
        None => None,
    };

    // 保存用户消息
    let user_msg = ChatMessage {
        role: "user".into(),
        content: resumed
            .as_ref()
            .map(|(_, decision)| decision.user_message())
            .unwrap_or_else(|| req.message.clone()),
        tool_calls: None,
        tool_call_id: None,
    };
    let _ = session_repo::add_message(&pool, &session_id, &user_msg).await;

    // 登记后台任务；进度写入任务的回放缓冲，任何时候都可以订阅
    let title: String = user_msg.content.chars().take(40).collect();
    let (sink, cancel) = crate::tasks::create(&pool, owner_id, &session_id, origin, &title, &req).await?;
    let task_id = sink.task_id.clone();

    // clone 必要数据给 agent 任务
    let user_id = owner_id.to_string();
    let project_id = req.project_id.clone();
    let user_message = build_user_message(&req);

    // 工作区概览 + 本轮 @ 引用的文件（Ask 模式也需要：只读工具可用）
    let workspace_note = {
        let overview = crate::workspace::overview(&pool, &owner_id).await.unwrap_or_else(|e| {
            tracing::warn!("[Workspace] 生成概览失败: {e:?}");
            String::new()
        });
        let refs = match req.file_refs.as_deref().filter(|ids| !ids.is_empty()) {
            Some(ids) => match crate::workspace::Tree::load(&pool, &owner_id).await {
                Ok(tree) => crate::workspace::referenced_files_note(&tree, ids),
                Err(_) => String::new(),
            },
            None => String::new(),
        };
        [overview, refs].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n")
    };

    // 当前时间（用户时区）：理解“明天”“下周五”、创建定时任务都需要
    let now_note = {
        let tz: chrono_tz::Tz = req
            .timezone
            .as_deref()
            .and_then(|t| t.parse().ok())
            .unwrap_or(chrono_tz::Asia::Shanghai);
        let now = chrono::Utc::now().with_timezone(&tz);
        let weekday = ["一", "二", "三", "四", "五", "六", "日"][now.format("%u").to_string().parse::<usize>().unwrap_or(1) - 1];
        format!("## 当前时间\n{}（星期{weekday}，时区 {tz}）", now.format("%Y-%m-%d %H:%M"))
    };

    // 技能：清单（名称 + 简介）+ 用户显式选择的技能正文；显式技能声明了本系统的工具时缩小工具范围
    let all_skills = crate::skills::load_all(&pool, &owner_id).await.unwrap_or_else(|e| {
        tracing::warn!("[Skills] 加载失败: {e:?}");
        Vec::new()
    });
    let selected_names: Vec<String> = req.skills.clone().unwrap_or_default();
    let skill_note = [crate::skills::listing(&all_skills), crate::skills::preload(&all_skills, &selected_names)]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let allowed_tools = {
        let selected: Vec<&crate::skills::Skill> = all_skills
            .iter()
            .filter(|s| s.enabled && selected_names.contains(&s.meta.name))
            .collect();
        let known: Vec<String> = crate::agent::registry::REGISTRY.list().await.iter().map(|t| t.name().to_string()).collect();
        crate::skills::narrow_tools(&selected, &known, allowed_tools)
    };

    let (start, agent_config) = match resumed {
        Some((run, decision)) => {
            let mut conversation = run.conversation;
            conversation.push(ChatMessage {
                role: "tool".into(),
                content: decision.tool_result(&run.plan),
                tool_calls: None,
                tool_call_id: Some(run.plan_call_id.clone()),
            });
            for call_id in &run.other_call_ids {
                conversation.push(ChatMessage {
                    role: "tool".into(),
                    content: r#"{"success":false,"error":"等待计划确认，未执行"}"#.into(),
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                });
            }
            (
                AgentStart::Resume {
                    conversation,
                    plan_steps: decision.effective_steps(&run.plan),
                },
                AgentConfig {
                    max_turns: 10,
                    system_prompt: String::new(),
                    allowed_tools: run.allowed_tools,
                    mode: decision.next_mode(),
                },
            )
        }
        None => (
            AgentStart::Fresh {
                history,
                user_message,
            },
            AgentConfig {
                max_turns: 10,
                // 附上会话产物清单，模型才能把"第三页""那份周报"对应到具体产物并局部修改
                system_prompt: [system_prompt, now_note, skill_note, crate::agent::artifact_ops::manifest(&existing_artifacts), workspace_note]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                allowed_tools: allowed_tools.clone(),
                mode,
            },
        ),
    };

    // 创建 tool context 的 emit 回调
    let emit_sink = sink.clone();
    let emit = move |event: &str, data: serde_json::Value| emit_sink.emit(event, data);

    let ctx = crate::agent::tool::ToolContext::new(
        session_id.clone(),
        user_id.clone(),
        project_id.clone(),
        req.model.clone(),
        req.attachments.clone().unwrap_or_default(),
        emit,
    )
    .with_tool_config(req.tool_config.clone().unwrap_or(serde_json::json!({})))
    .with_prior_artifacts(existing_artifacts.clone())
    .with_timezone(req.timezone.clone())
    .with_im_target(req.im_target.clone());

    let session_id_for_save = session_id.clone();
    let pool_for_save = pool.clone();
    let user_id_for_save = user_id.clone();
    let existing_artifacts_for_save = existing_artifacts.clone();
    sink.emit(
        "run_start",
        serde_json::json!({ "session_id": session_id, "run_id": task_id, "task_id": task_id }),
    );
    if let Some(attachments) = req.attachments.as_ref() {
        ctx.send(
            "state_update",
            serde_json::json!({
                "phase": "running",
                "step": "接收附件",
                "detail": format!("已接收 {} 个附件（支持 md / txt / csv / json / docx / xlsx / pptx / pdf / 图片；图片会优先尝试视觉识别，若视觉输入失败则退化为 OCR/文本辅助）", attachments.len()),
                "attachment_count": attachments.len(),
                "at": chrono::Utc::now().to_rfc3339(),
            }),
        );
    }

    let requested_tool_kind = resolved_tool_kind.clone();
    let attachments = req.attachments.clone().unwrap_or_default();
    let started = StartedTask { task_id: task_id.clone(), session_id: session_id.clone() };

    tokio::spawn(async move {
        // 排队等待运行名额（每个用户同时运行的任务数有上限）
        let _permit = crate::tasks::acquire_slot(&pool_for_save, &sink, &user_id_for_save).await;
        let mut event_rx = run_agent_loop(start, attachments, ctx, agent_config, client.clone(), cancel).await;
        let mut final_status = "failed";
        let mut final_summary: Option<String> = None;
        let mut final_error: Option<String> = Some("运行意外结束".to_string());
        let mut total_usage = crate::llm::Usage::default();
        let mut collected_artifacts: Vec<crate::models::Artifact> = Vec::new();
        let mut saved_artifact_ids = HashSet::new();

        while let Some(event) = event_rx.recv().await {
            let sse_event = match &event {
                AgentEvent::TextDelta { text } => Some(
                    ("message", serde_json::json!({
                        "text": text,
                        "session_id": session_id_for_save,
                    })),
                ),
                AgentEvent::ToolStart { call_id, tool } => Some(
                    ("tool_start", serde_json::json!({
                        "call_id": call_id,
                        "tool": tool,
                    })),
                ),
                AgentEvent::ToolCall { call_id, tool, input } => Some(
                    ("tool_call", serde_json::json!({
                        "call_id": call_id,
                        "tool": tool,
                        "input": input,
                    })),
                ),
                AgentEvent::ToolResult { call_id, tool, success, result, error, duration_ms } => Some(
                    ("tool_result", serde_json::json!({
                        "call_id": call_id,
                        "tool": tool,
                        "success": success,
                        "result": result,
                        "error": error,
                        "duration_ms": duration_ms,
                    })),
                ),
                AgentEvent::PlanProposed { plan, call_id, other_call_ids, conversation } => {
                    match agent_run_repo::create(
                        &pool_for_save,
                        &session_id_for_save,
                        &user_id_for_save,
                        plan,
                        conversation,
                        call_id,
                        other_call_ids,
                        allowed_tools.as_deref(),
                    )
                    .await
                    {
                        Ok(plan_run_id) => Some(("plan_proposed", serde_json::json!({
                            "run_id": plan_run_id,
                            "plan": plan,
                            "session_id": session_id_for_save,
                        }))),
                        Err(e) => {
                            tracing::error!("[Plan] 保存待确认计划失败: {e:?}");
                            Some(("error", serde_json::json!({
                                "message": "计划保存失败，请重试",
                            })))
                        }
                    }
                }
                AgentEvent::PlanProgress { step_id, success } => Some(
                    ("plan_progress", serde_json::json!({
                        "step_id": step_id,
                        "success": success,
                    })),
                ),
                AgentEvent::Usage { usage } => {
                    total_usage.add(*usage);
                    Some(("usage", serde_json::json!({
                        "prompt_tokens": total_usage.prompt_tokens,
                        "completion_tokens": total_usage.completion_tokens,
                    })))
                }
                AgentEvent::Artifact { artifact } => {
                    // 记录版本历史（AI 修订的版本号若被占用会顺延）
                    let mut artifact = artifact.clone();
                    if let Err(e) = artifact_repo::assign_next_version(&pool_for_save, &mut artifact).await {
                        tracing::warn!("[Artifact] 分配版本号失败: {e:?}");
                    }
                    if let Err(e) = artifact_repo::record_version(
                        &pool_for_save,
                        &session_id_for_save,
                        &artifact,
                        artifact_repo::AUTHOR_AGENT,
                    )
                    .await
                    {
                        tracing::warn!("[Artifact] 记录版本失败: {e:?}");
                    }
                    let artifact = &artifact;
                    collected_artifacts.push(artifact.clone());
                    if saved_artifact_ids.insert(format!("{}@{}", artifact.id, artifact.version)) {
                        crate::file_store::save_artifact_to_files(&pool_for_save, &user_id_for_save, artifact, crate::file_store::SaveTarget::Generated).await;
                    }
                    let session_artifacts = merge_session_artifacts(
                        existing_artifacts_for_save.clone(),
                        collected_artifacts.clone(),
                    );
                    let _ = session_repo::save_artifacts(
                        &pool_for_save,
                        &session_id_for_save,
                        &session_artifacts,
                    ).await;
                    Some(("artifact_update", serde_json::json!({
                        "artifact": artifact,
                        "artifacts": session_artifacts,
                        "session_id": session_id_for_save,
                        "tool_kind": artifact.tool_kind,
                    })))
                }
                // 文本已通过 TextDelta 推送，这里只用于持久化
                AgentEvent::Message { .. } => None,
                AgentEvent::TurnEnd { turn } => Some(
                    ("state_update", serde_json::json!({
                        "phase": "running",
                        "step": format!("第 {turn} 轮完成"),
                        "detail": format!("已完成 {turn} 轮工具调用"),
                        "at": chrono::Utc::now().to_rfc3339(),
                    })),
                ),
                AgentEvent::Done { summary, artifacts, cancelled, awaiting_approval } => {
                    final_status = if *cancelled {
                        "cancelled"
                    } else if *awaiting_approval {
                        "awaiting_approval"
                    } else {
                        "succeeded"
                    };
                    final_summary = Some(summary.clone());
                    final_error = None;
                    if collected_artifacts.is_empty() {
                        collected_artifacts = artifacts.clone();
                    }
                    if collected_artifacts.is_empty()
                        && requested_tool_kind.as_deref() == Some("doc")
                        && looks_like_markdown_document(&summary)
                    {
                        collected_artifacts.push(build_summary_markdown_artifact(&summary, requested_tool_kind.as_deref()));
                    }
                    let session_artifacts = merge_session_artifacts(
                        existing_artifacts_for_save.clone(),
                        collected_artifacts.clone(),
                    );
                    for artifact in &collected_artifacts {
                        if saved_artifact_ids.insert(format!("{}@{}", artifact.id, artifact.version)) {
                            crate::file_store::save_artifact_to_files(&pool_for_save, &user_id_for_save, artifact, crate::file_store::SaveTarget::Generated).await;
                        }
                    }
                    let _ = session_repo::update_summary(
                        &pool_for_save,
                        &session_id_for_save,
                        &summary.chars().take(240).collect::<String>(),
                    ).await;
                    // 本轮没有新产物时不改写会话产物，避免用过期快照覆盖
                    if !collected_artifacts.is_empty() {
                        let _ = session_repo::save_artifacts(
                            &pool_for_save,
                            &session_id_for_save,
                            &session_artifacts,
                        ).await;
                    }
                    Some(("done", serde_json::json!({
                        "session_id": session_id_for_save,
                        "summary": summary,
                        "artifacts": session_artifacts,
                        "new_artifacts": collected_artifacts,
                        "cancelled": cancelled,
                        "awaiting_approval": awaiting_approval,
                        "usage": total_usage,
                    })))
                }
                AgentEvent::Error { message } => {
                    final_status = "failed";
                    final_error = Some(message.clone());
                    Some(
                    ("error", serde_json::json!({
                        "message": message,
                    })),
                )}
            };
            if let Some((name, data)) = sse_event {
                sink.emit(name, data);
            }

            // 保存 assistant 消息
            if let AgentEvent::Message { content } = &event {
                let assistant_msg = ChatMessage {
                    role: "assistant".into(),
                    content: content.clone(),
                    tool_calls: None,
                    tool_call_id: None,
                };
                let _ =
                    session_repo::add_message(&pool_for_save, &session_id_for_save, &assistant_msg).await;
            }
        }
        // 运行结束：没有收到 done / error（例如 panic）时补发一个 error，订阅者不会一直等待
        if final_status == "failed" && final_summary.is_none() {
            sink.emit("error", serde_json::json!({ "message": final_error.clone().unwrap_or_default() }));
        }
        let unwatched = sink.watchers() == 0;
        crate::tasks::finish(&pool_for_save, &sink, final_status, final_summary.as_deref(), final_error.as_deref()).await;
        // 没有页面在看时（关掉了页面、定时任务、IM），完成后发通知
        if unwatched && final_status != "cancelled" {
            let (title, content) = match final_status {
                "succeeded" => (format!("任务完成：{title}"), final_summary.clone().unwrap_or_default()),
                "awaiting_approval" => (format!("计划待确认：{title}"), "Agent 提交了执行计划，请打开对话确认".to_string()),
                _ => (format!("任务失败：{title}"), final_error.clone().unwrap_or_default()),
            };
            let content: String = content.chars().take(200).collect();
            if let Err(e) = crate::db::notification_repo::create(
                &pool_for_save,
                &user_id_for_save,
                "task",
                &title,
                &content,
                Some(&format!("/?s={session_id_for_save}")),
            )
            .await
            {
                tracing::warn!("[Task] 写入通知失败: {e:?}");
            }
        }
    });

    Ok(started)
}
