use std::ops::Range;
use std::time::{Duration, Instant};

use futures::StreamExt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::context::{compact_context, DEFAULT_CONTEXT_CONFIG};
use super::plan::{AgentMode, Plan, PlanStep, PlanTracker, PROPOSE_PLAN};
use super::registry::REGISTRY;
use super::tool::{DynTool, ToolContext, ToolResult};
use crate::llm::{ChatCompletionResponse, FunctionDef, LlmClient, StreamEvent, Usage};
use crate::models::{Artifact, ChatAttachment, ChatMessage};

/// 写回给模型的单个工具结果最大字符数（读文件类工具需要较大的空间）
const MAX_TOOL_RESULT_CHARS: usize = 16_000;
/// 工具结果中 data 字段超过这个长度时不回传给模型
const MAX_TOOL_DATA_CHARS: usize = 2_000;

/// 同一并发组内最多同时执行的工具数
static MAX_PARALLEL_TOOLS: Lazy<usize> = Lazy::new(|| {
    std::env::var("AGENT_MAX_PARALLEL_TOOLS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n: &usize| *n > 0)
        .unwrap_or(3)
});

// NOTE: direct_media_tool / clean_direct_topic / direct_media_input 已移除。
// 原实现会在 allowed_tools 只有一个工具时跳过 LLM 推理直接调用工具，
// 导致意图误判（如"先帮我写提示词"被直接送到 video_generate）。
// 现在所有请求都走标准 ReAct 循环，由 LLM 决定是否调用工具。

/// Agent 事件（通过 channel 向上层推送）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    /// 模型输出的文本增量（含工具调用前的说明文字）
    TextDelta {
        text: String,
    },
    /// 模型决定调用某个工具（参数可能还在生成）
    ToolStart {
        call_id: String,
        tool: String,
    },
    /// 参数已完整，开始执行
    ToolCall {
        call_id: String,
        tool: String,
        input: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        tool: String,
        success: bool,
        result: serde_json::Value,
        error: Option<String>,
        duration_ms: u64,
    },
    Artifact {
        artifact: Artifact,
    },
    /// 本次回复的完整文本（所有轮次拼接），用于持久化；增量已通过 TextDelta 推送过
    Message {
        content: String,
    },
    TurnEnd {
        turn: usize,
    },
    Usage {
        usage: Usage,
    },
    /// Plan 模式下模型提交了计划；上层保存 conversation 以便确认后恢复
    PlanProposed {
        plan: Plan,
        /// propose_plan 的 tool_call id
        call_id: String,
        /// 同一轮里其他（未执行的）tool_call id
        other_call_ids: Vec<String>,
        conversation: Vec<ChatMessage>,
    },
    /// 已确认计划中某一步完成或失败
    PlanProgress {
        step_id: String,
        success: bool,
    },
    Done {
        summary: String,
        artifacts: Vec<Artifact>,
        cancelled: bool,
        /// 计划已提交，等待用户确认
        awaiting_approval: bool,
    },
    Error {
        message: String,
    },
}

/// Agent 配置
#[derive(Clone)]
pub struct AgentConfig {
    pub max_turns: usize,
    /// 追加到 OFFICE_AGENT_PROMPT 之后的内容（模式说明等）
    pub system_prompt: String,
    /// 用户在界面上显式限定的工具（如图片/视频模式）
    pub allowed_tools: Option<Vec<String>>,
    pub mode: AgentMode,
    /// 本次运行额外可用的工具（用户接入的 MCP 插件）
    pub extra_tools: Vec<DynTool>,
}

/// 工具范围匹配：精确名称，或以 * 结尾的前缀（如 mcp__amap__*）
fn scope_allows(allowed: &[String], name: &str) -> bool {
    allowed.iter().any(|a| match a.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => a == name,
    })
}

/// 按名称找工具：先找本次运行的插件工具，再找内置工具
async fn lookup_tool(config: &AgentConfig, name: &str) -> Option<DynTool> {
    if let Some(tool) = config.extra_tools.iter().find(|t| t.name() == name) {
        return Some(tool.clone());
    }
    REGISTRY.get(name).await
}

const OFFICE_AGENT_PROMPT: &str = r#"你是 WaLiOffice 的智能办公 Agent，帮助用户完成文档、演示、表格、图表、流程图、图片、视频和资料检索等办公任务。每个工具的适用场景写在它的描述里，按描述选择工具。

## 工作方式
1. 先判断用户这一轮真正要什么：要成品就调用工具交付；只是提问、讨论、要思路，就直接用文字回答。
2. 用户说"先写/先构思/先出方案/写提示词/写脚本"时，这一轮只交付文字或文档，不要直接生成最终的图片、视频或 PPT。
3. "先做 X 再做 Y"按顺序执行；一次要多个交付物时每个都要交付，彼此独立的工具可以在同一轮并行调用。
4. 信息不全时做合理假设（受众、篇幅、风格、字段）并在总结里说明；只有缺失信息会显著影响结果时才提问。
5. 需要最新或外部信息（新闻、政策、官网、实时数据）时先搜索，再基于搜索结果组织内容；不要声称访问过没有访问的页面。
6. 用户提供了数据文件（Excel、CSV 等）时，先 data_profile 了解结构，再用 data_query 计算；报告、PPT 和图表中的数字必须来自查询结果，不要估算或编造。
7. 用户说"刚才那张图""上面的文档""第三页"时，结合对话历史和已有产物理解指代。
8. 用户上传图片并问"这是什么、提取文字、分析截图"时，直接结合图片回答，不要调用生成工具。
9. 不要在文本中模拟工具调用或编造工具结果。
10. 用户要求执行一个操作（创建、提交、发送、修改外部系统里的数据）时，每次都必须实际调用对应的工具。对话历史里只有之前的文字回复、不包含工具调用，历史上的“已创建 / 已发送”不代表这次已经执行；没有调用工具就绝不能声称已完成，也不能编造编号或结果。

## 交付质量
- 产物要接近可直接使用：完整的章节与论证、真实可用的字段和数据、清晰的叙事，不要只给提纲或占位符。
- PPT 叙事有节奏、要点密度适合演示；Word 措辞正式；表格字段支持实际分析或执行；图表类型匹配数据且有业务含义；流程图层次清楚、便于继续编辑。

## 收尾
工具执行完后简洁总结：生成了哪些产物、各自适合怎么用、做了哪些假设。"#;

/// 第一轮发给 LLM 的完整请求（系统提示词 + 历史 + 用户消息，以及暴露的工具）
pub struct InitialRequest {
    pub conversation: Vec<ChatMessage>,
    pub function_defs: Vec<FunctionDef>,
}

/// 按工作模式与界面限定筛选暴露给模型的工具
pub async fn function_defs_for(config: &AgentConfig) -> Vec<FunctionDef> {
    let mut defs = Vec::new();
    let mut all = REGISTRY.list().await;
    all.extend(config.extra_tools.iter().cloned());
    for tool in all {
        let name = tool.name();
        let in_mode = config.mode.allows(name, tool.is_read_only());
        let in_scope = name == PROPOSE_PLAN
            || config
                .allowed_tools
                .as_ref()
                .map_or(true, |allowed| scope_allows(allowed, name));
        if in_mode && in_scope {
            defs.push(FunctionDef {
                def_type: "function".to_string(),
                function: crate::llm::FunctionSpec {
                    name: name.to_string(),
                    description: tool.description().to_string(),
                    parameters: tool.parameters(),
                },
            });
        }
    }
    defs
}

/// 组装第一轮请求（agent 循环与离线评测共用）
pub async fn build_initial_request(
    history: Vec<ChatMessage>,
    user_message: &str,
    config: &AgentConfig,
    client: &crate::llm::LlmClient,
) -> InitialRequest {
    let system_prompt = if config.system_prompt.is_empty() {
        OFFICE_AGENT_PROMPT.to_string()
    } else {
        format!("{}\n\n{}", OFFICE_AGENT_PROMPT, config.system_prompt)
    };
    let function_defs = function_defs_for(config).await;

    // 上下文压缩
    let compacted = compact_context(history, &DEFAULT_CONTEXT_CONFIG, client).await;

    // 组装消息
    let mut conversation: Vec<ChatMessage> = vec![ChatMessage {
        role: "system".to_string(),
        content: system_prompt,
        tool_calls: None,
        tool_call_id: None,
    }];
    // 保留最近 8 条历史
    let hist_start = compacted.len().saturating_sub(8);
    conversation.extend(compacted[hist_start..].to_vec());
    conversation.push(ChatMessage {
        role: "user".to_string(),
        content: user_message.to_string(),
        tool_calls: None,
        tool_call_id: None,
    });

    InitialRequest {
        conversation,
        function_defs,
    }
}

/// Agent 运行的起点
pub enum AgentStart {
    /// 新的一轮用户消息
    Fresh {
        history: Vec<ChatMessage>,
        user_message: String,
    },
    /// 恢复一次等待计划确认的运行：conversation 已包含对 propose_plan 的工具结果
    Resume {
        conversation: Vec<ChatMessage>,
        /// 已确认的计划步骤，用于推送执行进度
        plan_steps: Vec<PlanStep>,
    },
}

/// 运行 Agent 循环，通过 channel 推送事件。`cancel` 被触发后尽快停止（已生成的产物照常返回）
pub async fn run_agent_loop(
    start: AgentStart,
    user_attachments: Vec<ChatAttachment>,
    ctx: ToolContext,
    config: AgentConfig,
    client: std::sync::Arc<LlmClient>,
    cancel: CancellationToken,
) -> mpsc::UnboundedReceiver<AgentEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let max_turns = config.max_turns;
        let (mut conversation, function_defs, mut tracker) = match start {
            AgentStart::Fresh {
                history,
                user_message,
            } => {
                let InitialRequest {
                    conversation,
                    function_defs,
                } = build_initial_request(history, &user_message, &config, &client)
                    .await;
                (conversation, function_defs, PlanTracker::default())
            }
            AgentStart::Resume {
                conversation,
                plan_steps,
            } => (
                conversation,
                function_defs_for(&config).await,
                PlanTracker::new(plan_steps),
            ),
        };
        let attachments = (!user_attachments.is_empty()).then_some(user_attachments.as_slice());

        let mut all_artifacts: Vec<Artifact> = Vec::new();
        let mut transcript = String::new();

        // 所有请求都走标准 ReAct 循环，由 LLM 决定是否调用工具
        for turn in 0..max_turns {
            let tools = (!function_defs.is_empty()).then_some(function_defs.as_slice());
            let response = match stream_turn(&client, &conversation, tools, attachments, turn, &transcript, &tx, &cancel).await {
                TurnOutcome::Response(resp) => resp,
                TurnOutcome::Cancelled => {
                    finish_cancelled(&tx, transcript, all_artifacts);
                    return;
                }
                TurnOutcome::Failed(e) => {
                    let _ = tx.send(AgentEvent::Error {
                        message: format!("LLM 调用失败: {e}"),
                    });
                    return;
                }
            };
            if let Some(usage) = response.usage {
                let _ = tx.send(AgentEvent::Usage { usage });
            }

            let Some(choice) = response.choices.into_iter().next() else {
                let _ = tx.send(AgentEvent::Error {
                    message: "LLM 返回空响应".to_string(),
                });
                return;
            };
            let content = choice.message.content.unwrap_or_default();
            append_transcript(&mut transcript, &content);
            let tool_calls = choice.message.tool_calls.unwrap_or_default();

            // 没有工具调用 → 本轮文本就是最终回复
            if tool_calls.is_empty() {
                let summary = if content.trim().is_empty() {
                    let fallback = "我已完成你的请求。".to_string();
                    let _ = tx.send(AgentEvent::TextDelta {
                        text: separator(&transcript) + &fallback,
                    });
                    append_transcript(&mut transcript, &fallback);
                    fallback
                } else {
                    content
                };
                let _ = tx.send(AgentEvent::Message { content: transcript });
                let _ = tx.send(AgentEvent::Done {
                    summary,
                    artifacts: all_artifacts,
                    cancelled: false,
                    awaiting_approval: false,
                });
                return;
            }

            // 将 assistant 消息（含 tool_calls）加入上下文
            conversation.push(ChatMessage {
                role: "assistant".to_string(),
                content,
                tool_calls: Some(
                    tool_calls
                        .iter()
                        .map(|tc| {
                            serde_json::json!({
                                "id": tc.id,
                                "type": tc.call_type,
                                "function": {
                                    "name": tc.function.name,
                                    "arguments": tc.function.arguments,
                                }
                            })
                        })
                        .collect(),
                ),
                tool_call_id: None,
            });

            // Plan 模式：模型提交了计划 → 暂停运行，等待用户确认
            if let Some(plan_call) = tool_calls.iter().find(|tc| tc.function.name == PROPOSE_PLAN) {
                match Plan::from_arguments(&plan_call.function.arguments) {
                    Ok(plan) => {
                        append_transcript(&mut transcript, &plan.to_markdown());
                        let _ = tx.send(AgentEvent::PlanProposed {
                            plan: plan.clone(),
                            call_id: plan_call.id.clone(),
                            other_call_ids: tool_calls
                                .iter()
                                .filter(|tc| tc.id != plan_call.id)
                                .map(|tc| tc.id.clone())
                                .collect(),
                            conversation: conversation.clone(),
                        });
                        let _ = tx.send(AgentEvent::Message { content: transcript });
                        let _ = tx.send(AgentEvent::Done {
                            summary: plan.goal,
                            artifacts: all_artifacts,
                            cancelled: false,
                            awaiting_approval: true,
                        });
                        return;
                    }
                    Err(error) => {
                        // 计划格式有误：把错误交回模型重试
                        for tc in &tool_calls {
                            let content = if tc.id == plan_call.id {
                                format!("{{\"success\":false,\"error\":\"{error}\"}}")
                            } else {
                                "{\"success\":false,\"error\":\"需先提交有效计划\"}".to_string()
                            };
                            conversation.push(ChatMessage {
                                role: "tool".to_string(),
                                content,
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                            });
                        }
                        let _ = tx.send(AgentEvent::TurnEnd { turn: turn + 1 });
                        continue;
                    }
                }
            }

            let mut calls = Vec::with_capacity(tool_calls.len());
            for (index, tc) in tool_calls.into_iter().enumerate() {
                // 只能调用本轮暴露给模型的工具（模式 / 范围之外的即使存在也不执行）
                let exposed = function_defs.iter().any(|d| d.function.name == tc.function.name);
                calls.push(PlannedCall {
                    call_id: call_id(turn, index),
                    tool: if exposed { lookup_tool(&config, &tc.function.name).await } else { None },
                    input: serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::json!({})),
                    llm_call_id: tc.id,
                    name: tc.function.name,
                });
            }

            let timeout = Duration::from_millis(crate::config::config().llm_tool_timeout_ms);
            for outcome in execute_calls(calls, &ctx, &tx, &cancel, timeout).await {
                let result = outcome.result;
                // 告诉模型产物 id，后续步骤（批量生成、局部编辑）可以引用
                let produced: Vec<serde_json::Value> = outcome
                    .artifacts
                    .iter()
                    .map(|a| serde_json::json!({ "id": a.id, "kind": a.kind, "title": a.title, "version": a.version }))
                    .collect();
                // data 主要给前端用；体积大时不回传给模型，避免和 observation 重复占用上下文
                let data = result.data.as_ref().filter(|d| d.to_string().len() <= MAX_TOOL_DATA_CHARS);
                let tool_content = serde_json::to_string(&serde_json::json!({
                    "success": result.success,
                    "data": data,
                    "error": result.error,
                    "observation": result.observation,
                    "artifacts": produced,
                }))
                .unwrap_or_default()
                .chars()
                .take(MAX_TOOL_RESULT_CHARS)
                .collect::<String>();
                conversation.push(ChatMessage {
                    role: "tool".to_string(),
                    content: tool_content,
                    tool_calls: None,
                    tool_call_id: Some(outcome.llm_call_id),
                });
                if let Some(step_id) = tracker.record(&outcome.name, result.success) {
                    let _ = tx.send(AgentEvent::PlanProgress {
                        step_id,
                        success: result.success,
                    });
                }
                all_artifacts.extend(outcome.artifacts);
            }

            if cancel.is_cancelled() {
                finish_cancelled(&tx, transcript, all_artifacts);
                return;
            }
            let _ = tx.send(AgentEvent::TurnEnd { turn: turn + 1 });
        }

        // 超过最大轮次，用无工具调用生成总结
        info!("[AgentLoop] max turns ({max_turns}) reached, generating summary");
        conversation.push(ChatMessage {
            role: "system".to_string(),
            content: "你已经完成了多轮工具调用。请根据以上工具执行结果，给用户一个简洁的总结回复。不要调用任何工具。".to_string(),
            tool_calls: None,
            tool_call_id: None,
        });

        match stream_turn(&client, &conversation, None, attachments, max_turns, &transcript, &tx, &cancel).await {
            TurnOutcome::Response(resp) => {
                let content = resp
                    .choices
                    .into_iter()
                    .next()
                    .and_then(|c| c.message.content)
                    .unwrap_or_else(|| "任务已完成。".to_string());
                append_transcript(&mut transcript, &content);
                let _ = tx.send(AgentEvent::Message { content: transcript });
                let _ = tx.send(AgentEvent::Done {
                    summary: content,
                    artifacts: all_artifacts,
                    cancelled: false,
                    awaiting_approval: false,
                });
            }
            TurnOutcome::Cancelled => finish_cancelled(&tx, transcript, all_artifacts),
            TurnOutcome::Failed(e) => {
                warn!("[AgentLoop] summary generation failed: {e}");
                let _ = tx.send(AgentEvent::Done {
                    summary: format!("已完成 {max_turns} 轮工具调用，但总结生成失败: {e}"),
                    artifacts: all_artifacts,
                    cancelled: false,
                    awaiting_approval: false,
                });
            }
        }
    });

    rx
}

enum TurnOutcome {
    Response(ChatCompletionResponse),
    Cancelled,
    Failed(anyhow::Error),
}

/// 前端用来关联同一个工具调用各事件的 id
fn call_id(turn: usize, index: usize) -> String {
    format!("{turn}-{index}")
}

/// 新一轮文本与之前的文本之间的分隔
fn separator(transcript: &str) -> String {
    if transcript.is_empty() {
        String::new()
    } else {
        "\n\n".to_string()
    }
}

fn append_transcript(transcript: &mut String, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    transcript.push_str(&separator(transcript));
    transcript.push_str(text);
}

fn finish_cancelled(
    tx: &mpsc::UnboundedSender<AgentEvent>,
    transcript: String,
    artifacts: Vec<Artifact>,
) {
    info!("[AgentLoop] cancelled by user");
    if !transcript.trim().is_empty() {
        let _ = tx.send(AgentEvent::Message { content: transcript });
    }
    let _ = tx.send(AgentEvent::Done {
        summary: "已停止生成".to_string(),
        artifacts,
        cancelled: true,
        awaiting_approval: false,
    });
}

/// 流式调用一轮 LLM，增量实时转发为 TextDelta / ToolStart
#[allow(clippy::too_many_arguments)]
async fn stream_turn(
    client: &LlmClient,
    conversation: &[ChatMessage],
    tools: Option<&[FunctionDef]>,
    attachments: Option<&[ChatAttachment]>,
    turn: usize,
    transcript: &str,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    cancel: &CancellationToken,
) -> TurnOutcome {
    let sink_tx = tx.clone();
    let mut pending_separator = separator(transcript);
    let mut sink = move |event: StreamEvent| {
        let event = match event {
            StreamEvent::Delta(text) => AgentEvent::TextDelta {
                text: std::mem::take(&mut pending_separator) + &text,
            },
            StreamEvent::ToolCallStarted { index, name } => AgentEvent::ToolStart {
                call_id: call_id(turn, index),
                tool: name,
            },
        };
        let _ = sink_tx.send(event);
    };

    tokio::select! {
        _ = cancel.cancelled() => TurnOutcome::Cancelled,
        result = client.chat_stream_with_attachments(conversation, tools, attachments, &mut sink) => {
            match result {
                Ok(resp) => TurnOutcome::Response(resp),
                Err(e) => TurnOutcome::Failed(e),
            }
        }
    }
}

struct PlannedCall {
    call_id: String,
    llm_call_id: String,
    name: String,
    tool: Option<DynTool>,
    input: serde_json::Value,
}

struct CallOutcome {
    llm_call_id: String,
    name: String,
    result: ToolResult,
    artifacts: Vec<Artifact>,
}

/// 把同一轮的调用切成执行段：连续的可并行调用组成一个并发段，不可并行的调用单独成段（屏障）
pub(crate) fn plan_segments(parallel_safe: &[bool]) -> Vec<Range<usize>> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (i, safe) in parallel_safe.iter().enumerate() {
        if !safe {
            if start < i {
                segments.push(start..i);
            }
            segments.push(i..i + 1);
            start = i + 1;
        }
    }
    if start < parallel_safe.len() {
        segments.push(start..parallel_safe.len());
    }
    segments
}

/// 按段执行工具调用；段内并发（上限 MAX_PARALLEL_TOOLS），结果按原调用顺序返回
async fn execute_calls(
    calls: Vec<PlannedCall>,
    ctx: &ToolContext,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    cancel: &CancellationToken,
    timeout: Duration,
) -> Vec<CallOutcome> {
    let flags: Vec<bool> = calls
        .iter()
        .map(|c| c.tool.as_ref().map_or(true, |t| t.parallel_safe()))
        .collect();
    let segments = plan_segments(&flags);

    let mut calls = calls.into_iter();
    let mut outcomes = Vec::new();
    for segment in segments {
        let batch: Vec<PlannedCall> = calls.by_ref().take(segment.len()).collect();
        if cancel.is_cancelled() {
            outcomes.extend(batch.into_iter().map(|call| CallOutcome {
                llm_call_id: call.llm_call_id,
                name: call.name,
                result: ToolResult::err("用户已停止生成，工具未执行"),
                artifacts: vec![],
            }));
            continue;
        }
        let results: Vec<CallOutcome> = futures::stream::iter(
            batch
                .into_iter()
                .map(|call| run_one(call, ctx, tx, cancel, timeout)),
        )
        .buffered(*MAX_PARALLEL_TOOLS)
        .collect()
        .await;
        outcomes.extend(results);
    }
    outcomes
}

/// 执行单个工具：独立任务运行（panic 隔离），带超时与取消；完成后立即推送结果和产物
async fn run_one(
    call: PlannedCall,
    ctx: &ToolContext,
    tx: &mpsc::UnboundedSender<AgentEvent>,
    cancel: &CancellationToken,
    timeout: Duration,
) -> CallOutcome {
    let _ = tx.send(AgentEvent::ToolCall {
        call_id: call.call_id.clone(),
        tool: call.name.clone(),
        input: call.input.clone(),
    });
    info!("[ToolLog] {} → {} ({})", ctx.session_id, call.name, call.call_id);

    let started = Instant::now();
    let result = match call.tool {
        None => {
            let available = REGISTRY
                .list()
                .await
                .iter()
                .map(|t| t.name().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            ToolResult::err(format!("工具 \"{}\" 不可用（不存在，或当前模式 / 范围不开放）。可用工具: {available}", call.name))
        }
        Some(tool) => 'run: {
            // 需要确认的工具先等用户处理（不计入工具超时）
            if let Some(spec) = tool.approval(&call.input) {
                let outcome = super::approval::request(&ctx.for_call(&call.call_id), &call.call_id, &call.name, &spec, &call.input, cancel).await;
                if let super::approval::Outcome::Denied(reason) = outcome {
                    break 'run ToolResult::err(reason);
                }
            }
            let call_ctx = ctx.for_call(&call.call_id);
            let input = call.input.clone();
            let mut handle = tokio::spawn(async move { tool.call(input, &call_ctx).await });
            let abort = handle.abort_handle();
            tokio::select! {
                _ = cancel.cancelled() => {
                    abort.abort();
                    ToolResult::err("用户已停止生成，工具已中断")
                }
                joined = tokio::time::timeout(timeout, &mut handle) => match joined {
                    Ok(Ok(result)) => result,
                    Ok(Err(e)) => {
                        warn!("[AgentLoop] tool {} panicked: {e}", call.name);
                        ToolResult::err(format!("工具执行异常: {e}"))
                    }
                    Err(_) => {
                        abort.abort();
                        ToolResult::err(format!("工具执行超时（{} 秒）", timeout.as_secs()))
                    }
                }
            }
        }
    };

    let _ = tx.send(AgentEvent::ToolResult {
        call_id: call.call_id.clone(),
        tool: call.name.clone(),
        success: result.success,
        result: result.data.clone().unwrap_or(serde_json::Value::Null),
        error: result.error.clone(),
        duration_ms: started.elapsed().as_millis() as u64,
    });

    let mut artifacts = Vec::new();
    if result.success {
        for art in result.artifacts.iter().flatten() {
            let now = chrono::Utc::now().to_rfc3339();
            let artifact = Artifact {
                id: Uuid::new_v4().to_string(),
                kind: art.kind.clone(),
                tool_kind: map_artifact_to_tool_kind(&art.kind),
                title: art.title.clone(),
                status: "ready".to_string(),
                content: art.content.clone(),
                version: 1,
                created_at: now.clone(),
                updated_at: now,
                change_summary: None,
            };
            ctx.upsert_artifact(artifact.clone());
            let _ = tx.send(AgentEvent::Artifact {
                artifact: artifact.clone(),
            });
            artifacts.push(artifact);
        }
        if let Some(revision) = result.revision.clone() {
            // 修订已有产物：保留 id，版本号 +1（落库时若与已有版本冲突会再顺延）
            let mut artifact = revision.base;
            artifact.version += 1;
            artifact.title = revision.title;
            artifact.content = revision.content;
            artifact.change_summary = Some(revision.change_summary);
            artifact.status = "ready".to_string();
            artifact.updated_at = chrono::Utc::now().to_rfc3339();
            ctx.upsert_artifact(artifact.clone());
            let _ = tx.send(AgentEvent::Artifact {
                artifact: artifact.clone(),
            });
            artifacts.push(artifact);
        }
    }

    CallOutcome {
        llm_call_id: call.llm_call_id,
        name: call.name,
        result,
        artifacts,
    }
}

fn map_artifact_to_tool_kind(kind: &str) -> String {
    match kind {
        "document" => "doc",
        "search" => "general",
        "chart" => "general",
        "ppt" => "ppt",
        "drawio" => "drawio",
        "sheet" => "excel",
        "image" => "image",
        "video" => "video",
        "code" => "code",
        _ => "general",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tool::OfficeTool;
    use std::sync::Arc;

    /// 测试用工具：睡眠指定毫秒后返回；可设置为 panic 或不可并行
    struct SleepTool {
        name: &'static str,
        ms: u64,
        parallel: bool,
        panic: bool,
    }

    #[async_trait::async_trait]
    impl OfficeTool for SleepTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "test"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn parallel_safe(&self) -> bool {
            self.parallel
        }
        async fn call(&self, _input: serde_json::Value, ctx: &ToolContext) -> ToolResult {
            ctx.send("state_update", serde_json::json!({ "step": self.name }));
            tokio::time::sleep(Duration::from_millis(self.ms)).await;
            if self.panic {
                panic!("boom");
            }
            ToolResult::ok(format!("{} done", self.name), vec![])
        }
    }

    fn planned(index: usize, tool: SleepTool) -> PlannedCall {
        PlannedCall {
            call_id: call_id(0, index),
            llm_call_id: format!("llm-{index}"),
            name: tool.name.to_string(),
            tool: Some(Arc::new(tool)),
            input: serde_json::json!({}),
        }
    }

    fn sleep(name: &'static str, ms: u64) -> SleepTool {
        SleepTool { name, ms, parallel: true, panic: false }
    }

    fn test_ctx(events: Arc<std::sync::Mutex<Vec<serde_json::Value>>>) -> ToolContext {
        ToolContext::new("s".into(), "u".into(), None, None, vec![], move |_, data| {
            events.lock().unwrap().push(data);
        })
    }

    async fn run(
        calls: Vec<PlannedCall>,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> (Vec<CallOutcome>, Duration, Vec<serde_json::Value>) {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let ctx = test_ctx(events.clone());
        let (tx, _rx) = mpsc::unbounded_channel();
        let started = Instant::now();
        let outcomes = execute_calls(calls, &ctx, &tx, cancel, timeout).await;
        let emitted = events.lock().unwrap().clone();
        (outcomes, started.elapsed(), emitted)
    }

    #[tokio::test]
    async fn parallel_calls_overlap_and_keep_call_order() {
        let calls = vec![planned(0, sleep("a", 300)), planned(1, sleep("b", 100)), planned(2, sleep("c", 200))];
        let (outcomes, elapsed, emitted) = run(calls, &CancellationToken::new(), Duration::from_secs(5)).await;
        assert!(elapsed < Duration::from_millis(550), "并发执行应接近最慢的那个: {elapsed:?}");
        let ids: Vec<_> = outcomes.iter().map(|o| o.llm_call_id.as_str()).collect();
        assert_eq!(ids, ["llm-0", "llm-1", "llm-2"]);
        assert!(outcomes.iter().all(|o| o.result.success));
        // 工具推送的进度事件自动带上 call_id
        let mut tagged: Vec<_> = emitted.iter().map(|e| e["call_id"].as_str().unwrap().to_string()).collect();
        tagged.sort();
        assert_eq!(tagged, ["0-0", "0-1", "0-2"]);
    }

    #[tokio::test]
    async fn barrier_tool_runs_alone() {
        let barrier = SleepTool { name: "plan", ms: 200, parallel: false, panic: false };
        let calls = vec![planned(0, barrier), planned(1, sleep("a", 200)), planned(2, sleep("b", 200))];
        let (outcomes, elapsed, _) = run(calls, &CancellationToken::new(), Duration::from_secs(5)).await;
        assert!(elapsed >= Duration::from_millis(400), "屏障后再并发: {elapsed:?}");
        assert!(elapsed < Duration::from_millis(600), "{elapsed:?}");
        assert_eq!(outcomes.len(), 3);
    }

    #[tokio::test]
    async fn panic_and_timeout_are_isolated() {
        let panicking = SleepTool { name: "bad", ms: 10, parallel: true, panic: true };
        let calls = vec![planned(0, panicking), planned(1, sleep("slow", 5_000)), planned(2, sleep("ok", 10))];
        let (outcomes, elapsed, _) = run(calls, &CancellationToken::new(), Duration::from_millis(200)).await;
        assert!(elapsed < Duration::from_secs(1));
        assert!(!outcomes[0].result.success);
        assert!(outcomes[0].result.observation.contains("异常"));
        assert!(!outcomes[1].result.success);
        assert!(outcomes[1].result.observation.contains("超时"));
        assert!(outcomes[2].result.success);
    }

    #[tokio::test]
    async fn cancel_interrupts_running_and_skips_pending_segments() {
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            trigger.cancel();
        });
        let barrier = SleepTool { name: "plan", ms: 10, parallel: false, panic: false };
        let calls = vec![planned(0, sleep("long", 5_000)), planned(1, barrier)];
        let (outcomes, elapsed, _) = run(calls, &cancel, Duration::from_secs(10)).await;
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        assert!(outcomes.iter().all(|o| !o.result.success));
        assert!(outcomes[0].result.observation.contains("中断"));
        assert!(outcomes[1].result.observation.contains("未执行"));
    }

    #[test]
    fn segments_group_parallel_calls_and_isolate_barriers() {
        assert_eq!(plan_segments(&[]), Vec::<Range<usize>>::new());
        assert_eq!(plan_segments(&[true, true, true]), vec![0..3]);
        assert_eq!(plan_segments(&[false, true]), vec![0..1, 1..2]);
        assert_eq!(
            plan_segments(&[true, false, false, true, true]),
            vec![0..1, 1..2, 2..3, 3..5]
        );
    }

    #[test]
    fn transcript_joins_turns_with_blank_line() {
        let mut t = String::new();
        append_transcript(&mut t, "我先规划大纲。");
        append_transcript(&mut t, "  ");
        append_transcript(&mut t, "已生成 PPT。");
        assert_eq!(t, "我先规划大纲。\n\n已生成 PPT。");
        assert_eq!(separator(""), "");
    }
}
