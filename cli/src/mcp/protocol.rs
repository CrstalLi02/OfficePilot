//! JSON-RPC 2.0 与 MCP 方法：initialize / tools / resources / ping

use base64::Engine;
use serde_json::{json, Value};

use super::tools::{self, Tool};
use super::Workspace;
use crate::guides::TOPICS;

pub const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "officepilot 在本机创建、读取、修改、转换真实的 Word / Excel / PPT 文件，只改写涉及的部分，其余排版逐字节保留。\n\
工作方式：\n\
1. 改已有文件：先 read 看结构（块序号 / 单元格 / 页码与形状 id），再用 docs_apply / sheet_apply / slides_apply 提交一批操作，可先 dry_run 预览；不要凭猜测写序号。\n\
2. 新建 Word 用 create_docx（Markdown），Excel 用 create_xlsx（统计写公式，不要写死结果）。\n\
3. 做 PPT：deck_start（风格表 + 大纲）→ 每页 deck_page（检查不通过就按提示修改重交）→ deck_build → slides_audit；不满意的页 deck_replace。\n\
4. 不熟悉的操作先读资源 officepilot://guide/<主题> 或调用 guide。\n\
5. 出错时 error 说明原因、hint 给出修正方法，文件保持原样。";

pub struct Server {
    pub ws: Workspace,
    tools: Vec<Tool>,
    pub protocol: String,
}

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("md") => "text/markdown",
        Some("html") => "text/html",
        Some("csv") => "text/csv",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// 内嵌回传的文件大小上限
const INLINE_MAX: u64 = 1024 * 1024;

impl Server {
    pub fn new(ws: Workspace) -> Self {
        let local = ws.remote.is_none();
        let tools = tools::all().into_iter().filter(|t| local || !t.local_only).collect();
        Self { ws, tools, protocol: VERSIONS[0].into() }
    }

    /// 处理一条消息；通知（没有 id）返回 None。数组按批处理。
    pub fn handle(&mut self, msg: Value) -> Option<Value> {
        if let Value::Array(batch) = msg {
            let out: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(m)).collect();
            return if out.is_empty() { None } else { Some(Value::Array(out)) };
        }
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let Some(id) = id else {
            // 通知（initialized、cancelled 等）不需要回复
            return None;
        };
        if method.is_empty() {
            // 客户端发来的响应（本服务不发请求），忽略
            return None;
        }
        Some(match method {
            "initialize" => {
                let wanted = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(VERSIONS[0]);
                self.protocol = if VERSIONS.contains(&wanted) { wanted.to_string() } else { VERSIONS[0].to_string() };
                ok(&id, json!({
                    "protocolVersion": self.protocol,
                    "capabilities": { "tools": { "listChanged": false }, "resources": { "listChanged": false } },
                    "serverInfo": { "name": "officepilot", "title": "OfficePilot", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => ok(&id, json!({})),
            "tools/list" => ok(&id, json!({ "tools": self.tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.schema)(),
                "annotations": { "readOnlyHint": t.read_only, "destructiveHint": t.destructive, "idempotentHint": t.read_only, "openWorldHint": false },
            })).collect::<Vec<_>>() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match self.tools.iter().find(|t| t.name == name) {
                    None => err(&id, -32602, &format!("没有工具 {name}")),
                    Some(tool) => ok(&id, self.call(tool, &args)),
                }
            }
            "resources/list" => ok(&id, json!({ "resources": TOPICS.iter().map(|(name, _)| json!({
                "uri": format!("officepilot://guide/{name}"),
                "name": format!("guide-{name}"),
                "title": format!("officepilot 说明：{name}"),
                "mimeType": "text/markdown",
            })).collect::<Vec<_>>() })),
            "resources/templates/list" => ok(&id, json!({ "resourceTemplates": [] })),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
                match uri.strip_prefix("officepilot://guide/").and_then(|t| TOPICS.iter().find(|(n, _)| *n == t)) {
                    Some((_, text)) => ok(&id, json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }] })),
                    None => err(&id, -32002, &format!("没有资源 {uri}")),
                }
            }
            "prompts/list" => ok(&id, json!({ "prompts": [] })),
            "logging/setLevel" => ok(&id, json!({})),
            other => err(&id, -32601, &format!("不支持的方法 {other}")),
        })
    }

    fn call(&self, tool: &Tool, args: &Value) -> Value {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (tool.run)(&self.ws, args)));
        let result = match result {
            Ok(r) => r,
            Err(_) => Err(crate::Fail::new("内部错误（已记录），文件未改动")),
        };
        match result {
            Ok(done) => {
                let mut data = done.data;
                if let Some(obj) = data.as_object_mut() {
                    obj.insert("ok".into(), json!(!done.failed));
                }
                let mut content = vec![json!({ "type": "text", "text": done.summary })];
                self.attach_outputs(&mut data, &mut content);
                json!({ "content": content, "structuredContent": data, "isError": done.failed })
            }
            Err(fail) => {
                let text = match &fail.hint {
                    Some(h) => format!("{}\n提示：{h}", fail.message),
                    None => fail.message.clone(),
                };
                json!({ "content": [{ "type": "text", "text": text }], "structuredContent": { "ok": false, "error": fail.message, "hint": fail.hint }, "isError": true })
            }
        }
    }

    /// HTTP 模式：写出的文件给下载地址，小文件同时内嵌返回
    fn attach_outputs(&self, data: &mut Value, content: &mut Vec<Value>) {
        if self.ws.remote.is_none() {
            return;
        }
        let mut paths: Vec<String> = Vec::new();
        if let Some(p) = data.get("path").and_then(Value::as_str) {
            paths.push(p.to_string());
        }
        if let Some(list) = data.get("paths").and_then(Value::as_array) {
            paths.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
        }
        let mut urls = Vec::new();
        for p in paths {
            let path = std::path::Path::new(&p);
            if !path.is_file() {
                continue;
            }
            let Some(url) = self.ws.url_of(path) else { continue };
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if size <= INLINE_MAX {
                if let Ok(bytes) = std::fs::read(path) {
                    content.push(json!({ "type": "resource", "resource": { "uri": url, "mimeType": mime_of(&p), "blob": base64::engine::general_purpose::STANDARD.encode(bytes) } }));
                }
            } else {
                content.push(json!({ "type": "resource_link", "uri": url, "name": path.file_name().and_then(|n| n.to_str()).unwrap_or(""), "mimeType": mime_of(&p), "size": size }));
            }
            urls.push(url);
        }
        if let Some(obj) = data.as_object_mut() {
            if urls.len() == 1 {
                obj.insert("output_url".into(), json!(urls[0]));
            } else if !urls.is_empty() {
                obj.insert("output_urls".into(), json!(urls));
            }
        }
    }
}
