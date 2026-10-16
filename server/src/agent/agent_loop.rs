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
use super::registry::REGISTRY;
use super::tool::{DynTool, ToolContext, ToolResult};
use crate::llm::{ChatCompletionResponse, FunctionDef, LlmClient, StreamEvent, Usage};
use crate::models::{Artifact, ChatAttachment, ChatMessage};

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
    Done {
        summary: String,
        artifacts: Vec<Artifact>,
        cancelled: bool,
    },
    Error {
        message: String,
    },
}

/// Agent 配置
#[derive(Clone)]
pub struct AgentConfig {
    pub max_turns: usize,
    pub system_prompt: String,
    pub allowed_tools: Option<Vec<String>>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 8,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            allowed_tools: None,
        }
    }
}

const DEFAULT_SYSTEM_PROMPT: &str = r#"你是一个智能办公 Agent。你可以调用工具来帮助用户完成办公任务。

## 工作原则
1. 先理解用户意图，再选择合适的工具
2. 一次只调用必要的工具，不要过度调用
3. 工具返回结果后，总结关键信息给用户
4. 如果用户需求模糊，先询问澄清后再行动
5. 不要编造工具不存在的能力

## 输出规范
- 有工具需要调用时，返回 tool_calls
- 没有工具需要调用时，返回简洁的自然语言回复
- 不要在文本中模拟工具调用结果"#;

const OFFICE_AGENT_PROMPT: &str = r#"你是一个智能办公 Agent，可以帮助用户生成 PPT、Word 文档、Markdown 文档、表格、流程图、图片结果和视频结果。

## 可用工具
- ppt_plan: 规划 PPT 大纲（只读，不产生最终产物）
- ppt_generate: 生成完整 PPT 项目（含视觉设计）
- doc_generate: 生成结构化 Word 文档（报告/计划/总结/文章/PRD）
- md_generate: 生成 Markdown 文档（知识库/README/说明文档/纪要/调研整理）
- sheet_generate: 生成结构化表格（可导出 Excel）
- chart_generate: 生成可直接在右侧渲染的 ECharts 图表，适合普通对话中的趋势、对比、占比、排名、漏斗和指标可视化
- drawio_generate: 生成 draw.io 可编辑图表（流程图/架构图等）
- image_prompt: 基于 Agnes Image 2.1 Flash 生成图片结果，返回可直接预览的图片链接
- video_generate: 基于 Agnes Video V2.5 生成视频结果，返回可直接预览的 mp4 视频链接。支持三种模式：text（文生视频）、keyframe（首尾帧控制）、reference（多模态参考）。系统根据图片数量自动选择最优模式：0图→text，1-2图→keyframe，3+图→reference。支持从会话历史产物中自动提取图片。
- video_storyboard: 视频分镜规划工具（只读）。输入视频主题和可选参考图片，AI 自动拆分为多个镜头，每个镜头含独立的提示词、生成模式、时长和首尾帧分配。适用于复杂视频需求（多场景宣传片、故事短片）。规划完成后，逐镜头调用 video_generate 生成视频片段
- web_search: 联网搜索公开网页资料，适合查找最新信息、官网说明、新闻和政策

## 意图优先级与时序规则（必须遵守）
- 当用户说"先做X""帮我写/想/构思/规划X""先出个方案/提示词/大纲"时，当前意图是文本/文档生成，不是直接执行最终产物
  - "先帮我写提示词""写提示词""出提示词" → 用 md_generate 生成提示词文档，或直接回复文本
  - "帮我构思一个方案""先规划一下" → 用 md_generate 或 doc_generate
  - "帮我写脚本""写个脚本" → 用 md_generate 或 doc_generate
  - "帮我设计""先出个方案" → 用 md_generate 或 doc_generate
- 当用户说"先做X，再做Y""先X然后Y"时，必须分步执行，先完成X再考虑Y，不要跳到Y
- 当用户只是表达"想做什么"（愿望/设想），而非"现在做什么"（指令）时，应先帮助用户规划或出方案，而非直接生成
- "写提示词""出提示词""帮我写prompt" → 文本输出意图，不触发图片/视频生成工具
- "动画"在用户说"想做一个动画片"时，如果前面有"先写/先构思/帮我规划/帮我写"等修饰，应识别为文本/文档意图，不调用 video_generate

## 意图识别规则
- PPT/演示文稿/幻灯片/presentation/汇报材料/做个PPT → 先调 ppt_plan 再调 ppt_generate
- Word/文档/报告/PRD/方案/docx → doc_generate
- Markdown/md/README/知识库/说明文档/操作手册/会议纪要/调研整理 → md_generate
- excel/xlsx/表格/数据分析/排期/预算/数据指标 → sheet_generate
- 图表/可视化/趋势图/柱状图/折线图/饼图/占比/排名/漏斗/仪表盘，且用户不要求生成 Excel 或 PPT → chart_generate
- draw.io/流程图/架构图/泳道图/拓扑图/ER图 → drawio_generate
- 只有当用户明确要“生成图片/做海报/做封面/logo/配图/主视觉/插画/出图/改图/换风格/基于参考图生成”时，才调用 image_prompt；如本轮上传了图片且用户要求改图、生成同风格图片、换背景、做海报，使用 image_to_image
- 只有当用户明确要“生成视频/做视频/短片/短视频/宣传片/动画/视频广告/片头/转场动画/让图片动起来/图生视频/关键帧动画”时，才调用 video_generate；如本轮上传图片并要求动起来，系统自动选择 keyframe 模式；多图过渡使用 keyframe（首尾帧）；3+ 张图使用 reference 模式
- 当用户说"用刚才那张图""用上面的图片做视频""基于之前的图片生成视频"等上下文引用时，系统会自动从会话历史产物中提取图片 URL，用户不需要重新上传
- 当用户需求复杂（多场景/多镜头/故事线/发布会/宣传片/完整短片）时，先调用 video_storyboard 规划分镜，再逐镜头调用 video_generate 生成。简单视频需求直接调 video_generate，不需要分镜
- 如果用户上传了图片，并在问“这是什么”“帮我识别”“提取文字/OCR”“解释图片”“分析截图”“描述图里内容”等理解类问题，不要调用 image_prompt，优先直接结合视觉输入回答
- 用户需要“最新”“官网”“新闻”“政策”“联网查询”“检索资料”“搜索一下”等外部信息时 → web_search
- 当用户上传了 md/txt 附件时，要优先把附件正文视作本轮输入上下文，再决定是否继续调用工具
- 当用户上传了图片附件时，如当前模型支持视觉输入，要直接结合图片内容进行识别、总结与生成
- 用户说"做个XX"但未明确类型时，根据内容判断最合适的产物形式
- 用户明确要求多个交付物时（例如“PPT + 流程图 + 文档”），要拆解成多个子任务，按顺序调用所有对应工具，不要只生成其中一种

## 工作原则
1. PPT 任务：先调用 ppt_plan 规划大纲，再调用 ppt_generate 生成幻灯片（两步缺一不可）
2. 复杂视频任务：先调用 video_storyboard 规划分镜，再逐镜头调用 video_generate（每镜头一次调用）
3. 其他任务：直接调用对应工具
3. 普通对话中只要出现明确的数据对比、趋势、占比、排名或漏斗诉求，即使用户没有要求 PPT/Excel，也可以调用 chart_generate 生成右侧图表增强体验
4. 综合任务：优先遵循用户显式指定的文件类型，可在同一轮中顺序调用多个工具，常见组合如 `ppt_plan -> ppt_generate -> drawio_generate -> doc_generate` 或 `web_search -> md_generate`
5. 当用户同时要多个文件时，优先完整交付多个文件，不要把结果合并成一句普通文本
6. 工具总数以完成任务为准，尽量控制在 8 个以内；PPT 必须占用 2 个工具，复杂视频分镜+生成可能需要 4-8 个工具
7. 用户需求缺少少量信息时，优先做合理默认假设并继续生成，例如默认受众、篇幅、风格、表格字段；只有缺失信息会显著影响结果质量时才提问
8. 生成内容必须接近可直接交付的质量，不要只输出空泛提纲、模板占位符或“请补充内容”
9. 需要引用外部资料时，优先先调用 web_search，再基于搜索结果继续生成文档、PPT 或其他产物
10. 工具执行完毕后，简洁总结结果给用户，并说明已经生成了哪些文件，以及每个产物适合怎样使用
11. 如果调用了 web_search，回答中要基于搜索结果组织信息，不要假装亲自访问了不存在的页面
12. 如果用户上传的是图片附件，要优先尝试直接结合图像理解用户需求；如果当前模型不支持视觉或识别结果不可靠，再明确说明限制，并引导用户补充图片中的文字或关键内容
13. 对图片类请求先判断是“识图/读图”还是“生成图片/改图”。识图问题直接回答，生成或编辑图片才调用 image_prompt；有参考图片时优先传入 image_to_image
14. 对视频类请求优先调用 video_generate，并给出适合场景的镜头感、风格和成片方向；有参考图片时优先做图生视频，多张图做过渡或关键帧动画
15. 不要在文本中模拟工具调用结果
额外要求：
- 如果用户说“综合”“一起生成”“并且再来一个”“同时给我”，默认考虑多文件交付
- 如果用户既要可视化说明又要汇报材料，优先同时生成 draw.io 和 PPT
- 如果用户要方案/汇报材料并希望可下载，优先生成 document、markdown 或 sheet 作为正式文件产物
- 如果内容更适合知识沉淀、教程说明、README 或调研整理，优先生成 markdown 产物
- 常见业务场景包括：产品方案、运营复盘、销售汇报、技术设计、培训课件、项目实施；要主动贴近这些场景组织产物
- 对不同产物的质量预期：
  - PPT：要有清晰叙事节奏、页面目标、适合演示的标题和要点密度
  - Word：要有完整章节、论证、表格和正式措辞
  - Markdown：要便于阅读与沉淀，结构清楚，示例充分
  - Excel：字段要真实可用，行列设计要支持实际分析或执行
  - 图表：要选择适合的图表类型，数据要有业务含义，可直接在右侧动态渲染
  - draw.io：节点层次和关系要清楚，布局整齐，适合继续编辑
  - 图片结果：要有可直接预览的图像链接，同时保留风格化提示词，方便继续优化
  - 视频结果：要有可直接预览的视频链接，并说明视频时长、尺寸、生成模式和适用场景"#;

/// 第一轮发给 LLM 的完整请求（系统提示词 + 历史 + 用户消息，以及暴露的工具）
pub struct InitialRequest {
    pub conversation: Vec<ChatMessage>,
    pub function_defs: Vec<FunctionDef>,
}

/// 组装第一轮请求（agent 循环与离线评测共用）
pub async fn build_initial_request(
    history: Vec<ChatMessage>,
    user_message: &str,
    user_attachments: &[crate::models::ChatAttachment],
    config: &AgentConfig,
    client: &crate::llm::LlmClient,
) -> InitialRequest {
    let system_prompt = if config.system_prompt.is_empty() {
        OFFICE_AGENT_PROMPT.to_string()
    } else {
        // system_prompt 作为意图上下文补充，拼接到 OFFICE_AGENT_PROMPT 后面
        format!("{}\n{}", OFFICE_AGENT_PROMPT, config.system_prompt)
    };

    // 获取工具定义
    let allowed_tool_names = config.allowed_tools.clone();
    let mut function_defs: Vec<FunctionDef> = if let Some(ref allowed) = allowed_tool_names {
        REGISTRY
            .to_function_defs()
            .await
            .into_iter()
            .filter(|d| allowed.contains(&d.function.name))
            .collect()
    } else {
        REGISTRY.to_function_defs().await
    };
    // 有图片附件时，LLM 可以直接结合视觉输入回答，不需要额外工具
    if user_attachments.iter().any(|item| item.kind == "image") && allowed_tool_names.is_none() {
        function_defs.clear();
    }

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

/// 运行 Agent 循环，通过 channel 推送事件。`cancel` 被触发后尽快停止（已生成的产物照常返回）
pub async fn run_agent_loop(
    history: Vec<ChatMessage>,
    user_message: String,
    user_attachments: Vec<ChatAttachment>,
    ctx: ToolContext,
    config: AgentConfig,
    client: std::sync::Arc<LlmClient>,
    cancel: CancellationToken,
) -> mpsc::UnboundedReceiver<AgentEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let max_turns = config.max_turns;
        let InitialRequest {
            mut conversation,
            function_defs,
        } = build_initial_request(history, &user_message, &user_attachments, &config, &client).await;
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

            let mut calls = Vec::with_capacity(tool_calls.len());
            for (index, tc) in tool_calls.into_iter().enumerate() {
                calls.push(PlannedCall {
                    call_id: call_id(turn, index),
                    tool: REGISTRY.get(&tc.function.name).await,
                    input: serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::json!({})),
                    llm_call_id: tc.id,
                    name: tc.function.name,
                });
            }

            let timeout = Duration::from_millis(crate::config::config().llm_tool_timeout_ms);
            for outcome in execute_calls(calls, &ctx, &tx, &cancel, timeout).await {
                let result = outcome.result;
                let tool_content = serde_json::to_string(&serde_json::json!({
                    "success": result.success,
                    "data": result.data,
                    "error": result.error,
                    "observation": result.observation,
                }))
                .unwrap_or_default()
                .chars()
                .take(4000)
                .collect::<String>();
                conversation.push(ChatMessage {
                    role: "tool".to_string(),
                    content: tool_content,
                    tool_calls: None,
                    tool_call_id: Some(outcome.llm_call_id),
                });
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
                });
            }
            TurnOutcome::Cancelled => finish_cancelled(&tx, transcript, all_artifacts),
            TurnOutcome::Failed(e) => {
                warn!("[AgentLoop] summary generation failed: {e}");
                let _ = tx.send(AgentEvent::Done {
                    summary: format!("已完成 {max_turns} 轮工具调用，但总结生成失败: {e}"),
                    artifacts: all_artifacts,
                    cancelled: false,
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
            ToolResult::err(format!("工具 \"{}\" 未注册。可用工具: {available}", call.name))
        }
        Some(tool) => {
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
            };
            let _ = tx.send(AgentEvent::Artifact {
                artifact: artifact.clone(),
            });
            artifacts.push(artifact);
        }
    }

    CallOutcome {
        llm_call_id: call.llm_call_id,
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
