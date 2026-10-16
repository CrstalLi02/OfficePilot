//! 流式响应解析：把 OpenAI 兼容的 SSE chunk 实时转发给调用方，并拼装成完整的
//! `ChatCompletionResponse`，让 agent 循环按非流式的方式处理结果。

use std::collections::{BTreeMap, HashMap};

use anyhow::{anyhow, Result};
use eventsource_stream::Eventsource;
use futures::StreamExt;

use super::types::*;

/// 流式过程中推送给调用方的事件
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// 文本增量
    Delta(String),
    /// 某个工具调用的名字已经确定（参数可能还在生成）
    ToolCallStarted { index: usize, name: String },
}

/// 流式回调。调用方在这里实时转发增量（例如推送 SSE）；以 `&mut StreamSink` 形式传递
pub type StreamSink<'s> = dyn FnMut(StreamEvent) + Send + 's;

#[derive(Debug, Default)]
struct PartialCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

/// 按 index 累加流式 tool_call 分片
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    calls: BTreeMap<usize, PartialCall>,
    /// 部分服务商所有调用都用 index 0、只靠 id 区分；遇到新 id 时重映射到新的槽位
    alias: HashMap<usize, usize>,
}

impl ToolCallAccumulator {
    /// 累加一个分片；若这个分片让某个调用第一次有了名字，返回 (槽位, 名字)
    pub fn push(&mut self, chunk: &StreamToolCall) -> Option<(usize, String)> {
        let mut slot = *self.alias.get(&chunk.index).unwrap_or(&chunk.index);
        let incoming_id = chunk.id.as_deref().filter(|id| !id.is_empty());

        if let (Some(id), Some(existing)) = (incoming_id, self.calls.get(&slot)) {
            if existing.id.as_deref().is_some_and(|old| old != id) && !existing.name.is_empty() {
                slot = self.calls.keys().next_back().map_or(0, |k| k + 1);
                self.alias.insert(chunk.index, slot);
            }
        }

        let entry = self.calls.entry(slot).or_default();
        let was_named = !entry.name.is_empty();
        if let Some(id) = incoming_id {
            entry.id = Some(id.to_string());
        }
        if let Some(function) = &chunk.function {
            if let Some(name) = function.name.as_deref().filter(|n| !n.is_empty()) {
                if entry.name.is_empty() {
                    entry.name = name.to_string();
                } else if entry.name != name {
                    entry.name.push_str(name);
                }
            }
            if let Some(args) = &function.arguments {
                entry.arguments.push_str(args);
            }
        }

        (!was_named && !entry.name.is_empty()).then(|| (slot, entry.name.clone()))
    }

    pub fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .filter(|(_, call)| !call.name.is_empty())
            .map(|(index, call)| ToolCall {
                id: call.id.unwrap_or_else(|| format!("call_{index}")),
                call_type: "function".to_string(),
                function: ToolFunction {
                    name: call.name,
                    arguments: if call.arguments.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        call.arguments
                    },
                },
            })
            .collect()
    }
}

/// 逐块读取 SSE 响应，实时回调，并在结束时返回拼装好的完整响应
pub async fn read_stream_response(
    resp: reqwest::Response,
    model: &str,
    sink: &mut StreamSink<'_>,
) -> Result<ChatCompletionResponse> {
    let mut stream = resp.bytes_stream().eventsource();
    let mut content = String::new();
    let mut tool_calls = ToolCallAccumulator::default();
    let mut finish_reason = None;
    let mut usage = None;

    while let Some(event) = stream.next().await {
        let event = event.map_err(|e| anyhow!("LLM 流读取失败: {e}"))?;
        let data = event.data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        match serde_json::from_str::<StreamChunk>(data) {
            Ok(chunk) => {
                if chunk.usage.is_some() {
                    usage = chunk.usage;
                }
                for choice in chunk.choices {
                    if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
                        content.push_str(&text);
                        sink(StreamEvent::Delta(text));
                    }
                    for tc in choice.delta.tool_calls.iter().flatten() {
                        if let Some((index, name)) = tool_calls.push(tc) {
                            sink(StreamEvent::ToolCallStarted { index, name });
                        }
                    }
                    if choice.finish_reason.is_some() {
                        finish_reason = choice.finish_reason;
                    }
                }
            }
            Err(parse_err) => {
                // 部分服务商会在流中返回 {"error": {...}}
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(err) = value.get("error") {
                        return Err(anyhow!("LLM 流返回错误: {err}"));
                    }
                }
                tracing::debug!("跳过无法解析的 SSE chunk: {parse_err}");
            }
        }
    }

    let tool_calls = tool_calls.finish();
    Ok(ChatCompletionResponse {
        model: model.to_string(),
        usage,
        choices: vec![Choice {
            finish_reason,
            message: ResponseMessage {
                role: "assistant".to_string(),
                content: (!content.is_empty()).then_some(content),
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            },
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(index: usize, id: Option<&str>, name: Option<&str>, args: Option<&str>) -> StreamToolCall {
        StreamToolCall {
            index,
            id: id.map(str::to_string),
            function: Some(StreamToolFunction {
                name: name.map(str::to_string),
                arguments: args.map(str::to_string),
            }),
        }
    }

    #[test]
    fn accumulates_interleaved_calls_by_index() {
        let mut acc = ToolCallAccumulator::default();
        assert_eq!(
            acc.push(&chunk(0, Some("a"), Some("doc_generate"), Some(""))),
            Some((0, "doc_generate".to_string()))
        );
        assert_eq!(
            acc.push(&chunk(1, Some("b"), Some("drawio_generate"), None)),
            Some((1, "drawio_generate".to_string()))
        );
        assert_eq!(acc.push(&chunk(0, None, None, Some("{\"topic\":"))), None);
        assert_eq!(acc.push(&chunk(1, None, None, Some("{}"))), None);
        assert_eq!(acc.push(&chunk(0, None, None, Some("\"x\"}"))), None);

        let calls = acc.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[0].function.arguments, "{\"topic\":\"x\"}");
        assert_eq!(calls[1].function.name, "drawio_generate");
        assert_eq!(calls[1].function.arguments, "{}");
    }

    #[test]
    fn remaps_calls_that_reuse_index_zero_with_new_ids() {
        let mut acc = ToolCallAccumulator::default();
        acc.push(&chunk(0, Some("a"), Some("web_search"), Some("{\"q\":1}")));
        assert_eq!(
            acc.push(&chunk(0, Some("b"), Some("chart_generate"), Some("{\"t\":"))),
            Some((1, "chart_generate".to_string()))
        );
        acc.push(&chunk(0, None, None, Some("2}")));

        let calls = acc.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.arguments, "{\"q\":1}");
        assert_eq!(calls[1].id, "b");
        assert_eq!(calls[1].function.arguments, "{\"t\":2}");
    }

    #[test]
    fn missing_id_and_empty_args_get_defaults_and_nameless_calls_are_dropped() {
        let mut acc = ToolCallAccumulator::default();
        acc.push(&chunk(0, None, Some("md_generate"), None));
        acc.push(&chunk(3, None, None, Some("{}")));
        let calls = acc.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_0");
        assert_eq!(calls[0].function.arguments, "{}");
    }

    #[test]
    fn repeated_full_name_is_not_duplicated() {
        let mut acc = ToolCallAccumulator::default();
        acc.push(&chunk(0, Some("a"), Some("ppt_plan"), None));
        assert_eq!(acc.push(&chunk(0, Some("a"), Some("ppt_plan"), Some("{}"))), None);
        assert_eq!(acc.finish()[0].function.name, "ppt_plan");
    }
}
