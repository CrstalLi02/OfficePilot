//! 精简的 MCP 客户端：initialize → tools/list → tools/call。
//!
//! - Streamable HTTP：每个请求一次 POST，响应可能是 JSON，也可能是 SSE 流；会话 id 放在 `Mcp-Session-Id` 头；
//! - 旧版 SSE：GET 建立事件流，第一条 `endpoint` 事件给出 POST 地址，响应从事件流返回；
//! - stdio：启动子进程（不经过 shell，环境变量只带 PATH 等必要项和用户配置的），按行收发 JSON。
//! 服务端主动发来的请求只回应 ping，其余返回“不支持”。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use super::McpServer;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// stderr 保留的行数（进程退出时用于报错）
const STDERR_TAIL: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("会话已过期")]
    SessionExpired,
    #[error("连接已断开：{0}")]
    Closed(String),
    #[error("{0}")]
    Transport(String),
    #[error("超时（{0} 秒）")]
    Timeout(u64),
    #[error("服务返回错误（{code}）：{message}")]
    Rpc { code: i64, message: String },
}

impl McpError {
    /// 重新连接后可以安全重试（请求还没送达服务端）
    pub fn retryable(&self) -> bool {
        matches!(self, McpError::SessionExpired | McpError::Closed(_))
    }
}

type Result<T> = std::result::Result<T, McpError>;

fn transport(e: impl std::fmt::Display) -> McpError {
    McpError::Transport(e.to_string())
}

/// 一个工具的描述
#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

impl ToolInfo {
    fn from_value(v: &Value) -> Option<Self> {
        let name = v.get("name")?.as_str()?.to_string();
        let annotations = v.get("annotations").cloned().unwrap_or_default();
        Some(Self {
            title: v.get("title").or_else(|| annotations.get("title")).and_then(Value::as_str).map(str::to_string),
            description: v.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
            input_schema: v.get("inputSchema").cloned().unwrap_or_else(|| json!({ "type": "object" })),
            read_only: annotations.get("readOnlyHint").and_then(Value::as_bool).unwrap_or(false),
            name,
        })
    }
}

// ───────────────────────── SSE 解析 ─────────────────────────

/// 增量解析 text/event-stream：返回 (event, data)
#[derive(Default)]
pub struct SseParser {
    buf: String,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &str) -> Vec<(String, String)> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    let event = if self.event.is_empty() { "message".to_string() } else { std::mem::take(&mut self.event) };
                    out.push((event, self.data.join("\n")));
                }
                self.data.clear();
                self.event.clear();
            } else if let Some(v) = line.strip_prefix("data:") {
                self.data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = v.trim().to_string();
            }
        }
        out
    }
}

// ───────────────────────── 连接 ─────────────────────────

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// 流式连接（SSE / stdio）的发送端
enum Sender {
    Sse { client: reqwest::Client, post_url: String, headers: HeaderMap },
    Stdio { stdin: tokio::sync::Mutex<tokio::process::ChildStdin> },
}

impl Sender {
    async fn send(&self, message: &Value) -> Result<()> {
        match self {
            Sender::Sse { client, post_url, headers } => {
                let resp = client
                    .post(post_url)
                    .headers(headers.clone())
                    .json(message)
                    .send()
                    .await
                    .map_err(|e| McpError::Closed(e.to_string()))?;
                if !resp.status().is_success() {
                    return Err(transport(format!("HTTP {}", resp.status().as_u16())));
                }
                Ok(())
            }
            Sender::Stdio { stdin } => {
                let mut line = serde_json::to_vec(message).map_err(transport)?;
                line.push(b'\n');
                let mut stdin = stdin.lock().await;
                stdin.write_all(&line).await.map_err(|e| McpError::Closed(e.to_string()))?;
                stdin.flush().await.map_err(|e| McpError::Closed(e.to_string()))
            }
        }
    }
}

struct StreamConn {
    sender: Arc<Sender>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    stderr: Arc<Mutex<Vec<String>>>,
    tasks: Vec<tokio::task::AbortHandle>,
    _child: Option<tokio::process::Child>,
}

impl Drop for StreamConn {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

struct HttpConn {
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    session_id: Mutex<Option<String>>,
}

enum Conn {
    Http(HttpConn),
    Stream(StreamConn),
}

/// 收到一条消息：响应交给等待者；服务端请求回应 ping
async fn dispatch(message: Value, pending: &Pending, sender: &Sender) {
    let id = message.get("id").and_then(Value::as_u64);
    if message.get("method").is_some() {
        // 服务端发来的请求（带 id）需要回应；通知忽略
        if let Some(id_value) = message.get("id").cloned() {
            let reply = if message.get("method").and_then(Value::as_str) == Some("ping") {
                json!({ "jsonrpc": "2.0", "id": id_value, "result": {} })
            } else {
                json!({ "jsonrpc": "2.0", "id": id_value, "error": { "code": -32601, "message": "client does not support this request" } })
            };
            let _ = sender.send(&reply).await;
        }
        return;
    }
    if let Some(id) = id {
        if let Some(tx) = pending.lock().unwrap().remove(&id) {
            let _ = tx.send(message);
        }
    }
}

fn header_map(headers: &std::collections::BTreeMap<String, String>) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for (k, v) in headers {
        let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| transport(format!("请求头名称不合法：{k}")))?;
        let value = HeaderValue::from_str(v).map_err(|_| transport(format!("请求头「{k}」的值不合法")))?;
        map.insert(name, value);
    }
    Ok(map)
}

fn http_client() -> Result<reqwest::Client> {
    // 不跟随重定向：避免被重定向到内网地址
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(transport)
}

async fn connect_sse(url: &str, headers: HeaderMap) -> Result<StreamConn> {
    let client = http_client()?;
    let resp = client
        .get(url)
        .headers(headers.clone())
        .header(ACCEPT, "text/event-stream")
        .send()
        .await
        .map_err(|e| transport(format!("连接失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(transport(format!("连接失败：HTTP {}", resp.status().as_u16())));
    }
    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::default();
    let mut early = Vec::new();
    let post_url = loop {
        let chunk = tokio::time::timeout(CONNECT_TIMEOUT, stream.next())
            .await
            .map_err(|_| McpError::Timeout(CONNECT_TIMEOUT.as_secs()))?
            .ok_or_else(|| transport("服务没有返回 endpoint 事件"))?
            .map_err(transport)?;
        let mut found = None;
        for (event, data) in parser.push(&String::from_utf8_lossy(&chunk)) {
            if event == "endpoint" && found.is_none() {
                found = Some(data);
            } else {
                early.push(data);
            }
        }
        if let Some(endpoint) = found {
            let base = reqwest::Url::parse(url).map_err(transport)?;
            let joined = base.join(endpoint.trim()).map_err(|_| transport("endpoint 地址无效"))?;
            // POST 地址必须与 SSE 地址同源，防止被引导到其他主机
            if joined.origin() != base.origin() {
                return Err(transport("endpoint 与服务地址不同源，已拒绝"));
            }
            break joined.to_string();
        }
    };
    let sender = Arc::new(Sender::Sse { client, post_url, headers });
    let pending: Pending = Arc::default();
    let alive = Arc::new(AtomicBool::new(true));
    let task = {
        let (sender, pending, alive) = (sender.clone(), pending.clone(), alive.clone());
        tokio::spawn(async move {
            for data in early {
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    dispatch(v, &pending, &sender).await;
                }
            }
            while let Some(Ok(chunk)) = stream.next().await {
                for (_, data) in parser.push(&String::from_utf8_lossy(&chunk)) {
                    if let Ok(v) = serde_json::from_str::<Value>(&data) {
                        dispatch(v, &pending, &sender).await;
                    }
                }
            }
            alive.store(false, Ordering::SeqCst);
            pending.lock().unwrap().clear();
        })
    };
    Ok(StreamConn { sender, pending, alive, stderr: Arc::default(), tasks: vec![task.abort_handle()], _child: None })
}

async fn connect_stdio(server: &McpServer) -> Result<StreamConn> {
    let command = server.command.as_deref().unwrap_or_default();
    let dir = std::path::Path::new(&crate::config::config().data_dir).join("mcp").join(&server.owner_id);
    let _ = std::fs::create_dir_all(&dir);
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(&server.args)
        .current_dir(&dir)
        // 不继承服务端环境变量（里面有模型密钥等），只带运行命令必需的几项
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for key in ["PATH", "HOME", "LANG", "TMPDIR", "SystemRoot", "USERPROFILE", "APPDATA"] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    cmd.envs(&server.secrets.env);
    let mut child = cmd.spawn().map_err(|e| transport(format!("启动命令失败（{command}）：{e}")))?;
    let stdin = child.stdin.take().ok_or_else(|| transport("无法写入子进程"))?;
    let stdout = child.stdout.take().ok_or_else(|| transport("无法读取子进程"))?;
    let stderr_pipe = child.stderr.take();

    let sender = Arc::new(Sender::Stdio { stdin: tokio::sync::Mutex::new(stdin) });
    let pending: Pending = Arc::default();
    let alive = Arc::new(AtomicBool::new(true));
    let stderr: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut tasks = Vec::new();
    {
        let (sender, pending, alive) = (sender.clone(), pending.clone(), alive.clone());
        tasks.push(
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                        dispatch(v, &pending, &sender).await;
                    }
                }
                alive.store(false, Ordering::SeqCst);
                pending.lock().unwrap().clear();
            })
            .abort_handle(),
        );
    }
    if let Some(pipe) = stderr_pipe {
        let stderr = stderr.clone();
        tasks.push(
            tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut tail = stderr.lock().unwrap();
                    tail.push(line.chars().take(300).collect());
                    if tail.len() > STDERR_TAIL {
                        tail.remove(0);
                    }
                }
            })
            .abort_handle(),
        );
    }
    Ok(StreamConn { sender, pending, alive, stderr, tasks, _child: Some(child) })
}

// ───────────────────────── 会话 ─────────────────────────

pub struct Session {
    conn: Conn,
    next_id: AtomicU64,
    protocol: Mutex<String>,
    pub server_name: String,
}

fn rpc_result(message: Value) -> Result<Value> {
    if let Some(err) = message.get("error") {
        return Err(McpError::Rpc {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: err.get("message").and_then(Value::as_str).unwrap_or("未知错误").to_string(),
        });
    }
    Ok(message.get("result").cloned().unwrap_or(Value::Null))
}

impl Session {
    /// 建立连接并完成 initialize 握手
    pub async fn connect(server: &McpServer) -> Result<Session> {
        let conn = match server.transport.as_str() {
            "http" | "sse" => {
                let url = server.url.clone().unwrap_or_default();
                super::check_url(&url).await.map_err(transport)?;
                let headers = header_map(&server.secrets.headers)?;
                if server.transport == "sse" {
                    Conn::Stream(connect_sse(&url, headers).await?)
                } else {
                    Conn::Http(HttpConn { client: http_client()?, url, headers, session_id: Mutex::new(None) })
                }
            }
            "stdio" => {
                if !super::stdio_enabled() {
                    return Err(transport("服务端没有开启本地命令（stdio）插件"));
                }
                Conn::Stream(connect_stdio(server).await?)
            }
            other => return Err(transport(format!("不支持的传输方式：{other}"))),
        };
        let mut session = Session { conn, next_id: AtomicU64::new(1), protocol: Mutex::new(PROTOCOL_VERSION.into()), server_name: String::new() };
        let init = session
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "WaLiOffice", "version": env!("CARGO_PKG_VERSION") },
                }),
                CONNECT_TIMEOUT,
            )
            .await?;
        if let Some(v) = init.get("protocolVersion").and_then(Value::as_str) {
            *session.protocol.lock().unwrap() = v.to_string();
        }
        session.server_name = init.pointer("/serverInfo/name").and_then(Value::as_str).unwrap_or_default().to_string();
        session.notify("notifications/initialized", json!({})).await?;
        Ok(session)
    }

    pub fn alive(&self) -> bool {
        match &self.conn {
            Conn::Http(_) => true,
            Conn::Stream(s) => s.alive.load(Ordering::SeqCst),
        }
    }

    fn closed_error(&self) -> McpError {
        match &self.conn {
            Conn::Stream(s) => {
                let tail = s.stderr.lock().unwrap().join("\n");
                McpError::Closed(if tail.is_empty() { "服务已退出".into() } else { format!("服务已退出：{tail}") })
            }
            Conn::Http(_) => McpError::Closed("连接已关闭".into()),
        }
    }

    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let fut = async {
            match &self.conn {
                Conn::Http(http) => self.http_request(http, id, &message, method == "initialize").await,
                Conn::Stream(stream) => {
                    if !stream.alive.load(Ordering::SeqCst) {
                        return Err(self.closed_error());
                    }
                    let (tx, rx) = oneshot::channel();
                    stream.pending.lock().unwrap().insert(id, tx);
                    if let Err(e) = stream.sender.send(&message).await {
                        stream.pending.lock().unwrap().remove(&id);
                        return Err(e);
                    }
                    rx.await.map_err(|_| self.closed_error())
                }
            }
        };
        match tokio::time::timeout(timeout, fut).await {
            Ok(result) => rpc_result(result?),
            Err(_) => {
                if let Conn::Stream(stream) = &self.conn {
                    stream.pending.lock().unwrap().remove(&id);
                }
                Err(McpError::Timeout(timeout.as_secs()))
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        match &self.conn {
            Conn::Http(http) => {
                let resp = self.http_post(http, &message).await?;
                if !resp.status().is_success() {
                    return Err(transport(format!("HTTP {}", resp.status().as_u16())));
                }
                Ok(())
            }
            Conn::Stream(stream) => stream.sender.send(&message).await,
        }
    }

    async fn http_post(&self, http: &HttpConn, message: &Value) -> Result<reqwest::Response> {
        let mut req = http
            .client
            .post(&http.url)
            .headers(http.headers.clone())
            .header(ACCEPT, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .header("MCP-Protocol-Version", self.protocol.lock().unwrap().clone());
        if let Some(sid) = http.session_id.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", sid);
        }
        req.json(message).send().await.map_err(|e| transport(format!("请求失败：{e}")))
    }

    async fn http_request(&self, http: &HttpConn, id: u64, message: &Value, initialize: bool) -> Result<Value> {
        let resp = self.http_post(http, message).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND && http.session_id.lock().unwrap().is_some() {
            return Err(McpError::SessionExpired);
        }
        if !status.is_success() {
            let body: String = resp.text().await.unwrap_or_default().chars().take(300).collect();
            return Err(transport(format!("HTTP {}：{body}", status.as_u16())));
        }
        if initialize {
            if let Some(sid) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
                *http.session_id.lock().unwrap() = Some(sid.to_string());
            }
        }
        let is_sse = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let matches = |v: &Value| v.get("id").and_then(Value::as_u64) == Some(id) && v.get("method").is_none();
        if !is_sse {
            let body: Value = resp.json().await.map_err(|e| transport(format!("响应不是 JSON：{e}")))?;
            return match body {
                Value::Array(items) => items.into_iter().find(matches).ok_or_else(|| transport("响应里没有对应的结果")),
                v => Ok(v),
            };
        }
        // SSE 响应：一直读到对应 id 的结果
        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::default();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| McpError::Closed(e.to_string()))?;
            for (_, data) in parser.push(&String::from_utf8_lossy(&chunk)) {
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    if matches(&v) {
                        return Ok(v);
                    }
                }
            }
        }
        Err(transport("响应流结束前没有收到结果"))
    }

    /// 列出全部工具（处理分页）
    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.request("tools/list", params, LIST_TIMEOUT).await?;
            if let Some(items) = result.get("tools").and_then(Value::as_array) {
                tools.extend(items.iter().filter_map(ToolInfo::from_value));
            }
            cursor = result.get("nextCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(&self, name: &str, arguments: Value, timeout: Duration) -> Result<Value> {
        self.request("tools/call", json!({ "name": name, "arguments": arguments }), timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_split_chunks() {
        let mut p = SseParser::default();
        assert!(p.push("event: endpoint\ndata: /messages?sid=1").is_empty());
        assert_eq!(p.push("\n\n"), vec![("endpoint".to_string(), "/messages?sid=1".to_string())]);
        let events = p.push("data: {\"a\":\r\ndata: 1}\r\n\r\n: comment\n\ndata: x\n\n");
        assert_eq!(events, vec![("message".to_string(), "{\"a\":\n1}".to_string()), ("message".to_string(), "x".to_string())]);
    }

    #[test]
    fn tool_info_reads_annotations() {
        let t = ToolInfo::from_value(&json!({
            "name": "get_weather", "description": "查天气",
            "inputSchema": { "type": "object", "properties": { "city": { "type": "string" } } },
            "annotations": { "readOnlyHint": true, "title": "天气" }
        }))
        .unwrap();
        assert!(t.read_only);
        assert_eq!(t.title.as_deref(), Some("天气"));
        let w = ToolInfo::from_value(&json!({ "name": "send" })).unwrap();
        assert!(!w.read_only);
        assert!(ToolInfo::from_value(&json!({ "description": "no name" })).is_none());
    }

    #[test]
    fn rpc_errors() {
        assert_eq!(rpc_result(json!({ "id": 1, "result": { "ok": true } })).unwrap(), json!({ "ok": true }));
        match rpc_result(json!({ "id": 1, "error": { "code": -32602, "message": "bad" } })) {
            Err(McpError::Rpc { code, message }) => assert_eq!((code, message.as_str()), (-32602, "bad")),
            other => panic!("{other:?}"),
        }
    }
}
