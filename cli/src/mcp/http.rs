//! HTTP 传输：Streamable HTTP（POST /mcp）与旧版 SSE（GET /sse + POST /messages），
//! 以及文件仓库（PUT /files/<文件名> 上传，GET /files/… 下载）。
//!
//! 安全：默认只监听本机；监听其它地址必须设置令牌；每个会话只能访问自己的临时目录和上传的文件；
//! 带 Origin 的请求只接受本机来源（防 DNS 重绑定）；不会去下载外部链接。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server as HttpServer, StatusCode};

use super::protocol::Server;
use super::{allowed_roots_from_env, canonical, urlencoding_decode, Remote, Workspace};

pub struct Options {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
    pub root: PathBuf,
    pub public_url: Option<String>,
}

/// 单个上传文件的大小上限
const MAX_FILE: u64 = 50 * 1024 * 1024;
/// 单条 JSON-RPC 请求的大小上限
const MAX_JSON: u64 = 8 * 1024 * 1024;
/// 会话空闲多久后清理
const IDLE: Duration = Duration::from_secs(3600);

struct Session {
    server: Server,
    last: Instant,
    /// 旧版 SSE：回复从这条流推回去
    sse: Option<Sender<String>>,
}

struct State {
    token: Option<String>,
    store: PathBuf,
    base_url: String,
    loopback: bool,
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

fn header(req: &Request, name: &str) -> Option<String> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

fn h(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
}

fn json_response(status: u16, body: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_data(serde_json::to_vec(body).unwrap_or_default()).with_status_code(StatusCode(status)).with_header(h("Content-Type", "application/json; charset=utf-8"))
}

fn error(status: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(status, &json!({ "error": message }))
}

/// 常数时间比较，避免按耗时猜令牌
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() { "file".into() } else { cleaned }
}

impl State {
    fn workspace(&self, session: &str) -> std::io::Result<Workspace> {
        let dir = self.store.join("sessions").join(session);
        std::fs::create_dir_all(&dir)?;
        let mut allowed = vec![canonical(&dir), canonical(&self.store.join("uploads"))];
        allowed.extend(allowed_roots_from_env());
        Ok(Workspace { root: canonical(&dir), allowed, remote: Some(Remote { base_url: self.base_url.clone(), store: canonical(&self.store), bucket: session.to_string() }) })
    }

    fn new_session(&self, sse: Option<Sender<String>>) -> std::io::Result<String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let ws = self.workspace(&id)?;
        self.sessions.lock().unwrap().insert(id.clone(), Arc::new(Mutex::new(Session { server: Server::new(ws), last: Instant::now(), sse })));
        Ok(id)
    }

    fn drop_session(&self, id: &str) {
        if self.sessions.lock().unwrap().remove(id).is_some() {
            let _ = std::fs::remove_dir_all(self.store.join("sessions").join(id));
        }
    }

    fn cleanup(&self) {
        let stale: Vec<String> = self.sessions.lock().unwrap().iter().filter(|(_, s)| s.try_lock().map(|s| s.last.elapsed() > IDLE).unwrap_or(false)).map(|(k, _)| k.clone()).collect();
        for id in stale {
            self.drop_session(&id);
        }
        // 超过空闲时间的上传文件
        if let Ok(entries) = std::fs::read_dir(self.store.join("uploads")) {
            for e in entries.flatten() {
                let old = e.metadata().and_then(|m| m.modified()).map(|t| t.elapsed().unwrap_or_default() > IDLE).unwrap_or(false);
                if old {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }
}

fn read_body(req: &mut Request, limit: u64) -> Result<Vec<u8>, &'static str> {
    if req.body_length().map(|l| l as u64 > limit).unwrap_or(false) {
        return Err("内容太大");
    }
    let mut body = Vec::new();
    req.as_reader().take(limit + 1).read_to_end(&mut body).map_err(|_| "读取请求失败")?;
    if body.len() as u64 > limit {
        return Err("内容太大");
    }
    Ok(body)
}

fn query_param(url: &str, key: &str) -> Option<String> {
    url.split_once('?')?.1.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| urlencoding_decode(v))
    })
}

fn handle(state: Arc<State>, mut req: Request) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();
    let method = req.method().clone();

    // 浏览器来源检查：只接受本机页面（防 DNS 重绑定）
    if let Some(origin) = header(&req, "Origin") {
        let host = origin.split("://").nth(1).unwrap_or("").split(['/', ':']).next().unwrap_or("");
        if state.loopback && !is_loopback(host) {
            let _ = req.respond(error(403, "不接受来自其它网站的请求"));
            return;
        }
    }
    if path != "/" {
        if let Some(token) = &state.token {
            let got = header(&req, "Authorization").unwrap_or_default();
            if !same(got.strip_prefix("Bearer ").unwrap_or(""), token) {
                let _ = req.respond(error(401, "缺少或错误的令牌（Authorization: Bearer …）").with_header(h("WWW-Authenticate", "Bearer")));
                return;
            }
        }
    }

    let response = match (method, path.as_str()) {
        (Method::Get, "/") => json_response(200, &json!({ "name": "officepilot", "version": env!("CARGO_PKG_VERSION"), "mcp": format!("{}/mcp", state.base_url), "upload": format!("PUT {}/files/<文件名>", state.base_url) })),
        (Method::Post, "/mcp") => {
            let body = match read_body(&mut req, MAX_JSON) {
                Ok(b) => b,
                Err(e) => return drop(req.respond(error(413, e))),
            };
            let msg: Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => return drop(req.respond(json_response(400, &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("JSON 解析失败：{e}") } })))),
            };
            let is_init = match &msg {
                Value::Array(a) => a.iter().any(|m| m["method"] == "initialize"),
                m => m["method"] == "initialize",
            };
            let sid = if is_init {
                match state.new_session(None) {
                    Ok(id) => id,
                    Err(e) => return drop(req.respond(error(500, &e.to_string()))),
                }
            } else {
                match header(&req, "Mcp-Session-Id") {
                    Some(id) if state.sessions.lock().unwrap().contains_key(&id) => id,
                    Some(_) => return drop(req.respond(error(404, "会话不存在或已过期，请重新 initialize"))),
                    None => return drop(req.respond(error(400, "缺少 Mcp-Session-Id，请先 initialize"))),
                }
            };
            // 同一会话的请求串行处理，不同会话互不阻塞
            let session = state.sessions.lock().unwrap().get(&sid).cloned();
            let Some(session) = session else { return drop(req.respond(error(404, "会话不存在或已过期，请重新 initialize"))) };
            let reply = {
                let mut s = session.lock().unwrap();
                s.last = Instant::now();
                s.server.handle(msg)
            };
            match reply {
                Some(r) => json_response(200, &r).with_header(h("Mcp-Session-Id", &sid)),
                None => Response::from_data(Vec::new()).with_status_code(StatusCode(202)).with_header(h("Mcp-Session-Id", &sid)),
            }
        }
        (Method::Delete, "/mcp") => {
            if let Some(id) = header(&req, "Mcp-Session-Id") {
                state.drop_session(&id);
            }
            Response::from_data(Vec::new()).with_status_code(StatusCode(200))
        }
        (Method::Get, "/mcp") => error(405, "本服务不提供服务端推送流，请用 POST"),
        (Method::Get, "/sse") => {
            let (tx, rx) = mpsc::channel();
            let id = match state.new_session(Some(tx.clone())) {
                Ok(id) => id,
                Err(e) => return drop(req.respond(error(500, &e.to_string()))),
            };
            // 直接写原始连接并逐条刷新（tiny_http 的分块响应会缓冲小事件）
            let mut w = req.into_writer();
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
            let mut send = |text: &str| w.write_all(text.as_bytes()).and_then(|_| w.flush()).is_ok();
            if send(head) && send(&format!("event: endpoint\ndata: /messages?sessionId={id}\n\n")) {
                loop {
                    let ok = match rx.recv_timeout(Duration::from_secs(20)) {
                        Ok(event) => send(&event),
                        Err(mpsc::RecvTimeoutError::Timeout) => send(": ping\n\n"),
                        Err(mpsc::RecvTimeoutError::Disconnected) => false,
                    };
                    if !ok {
                        break;
                    }
                }
            }
            // 客户端断开后清理会话
            drop(tx);
            state.drop_session(&id);
            return;
        }
        (Method::Post, "/messages") => {
            let Some(id) = query_param(&url, "sessionId") else { return drop(req.respond(error(400, "缺少 sessionId"))) };
            let body = match read_body(&mut req, MAX_JSON) {
                Ok(b) => b,
                Err(e) => return drop(req.respond(error(413, e))),
            };
            let Ok(msg) = serde_json::from_slice::<Value>(&body) else { return drop(req.respond(error(400, "JSON 解析失败"))) };
            let session = state.sessions.lock().unwrap().get(&id).cloned();
            let Some(session) = session else { return drop(req.respond(error(404, "会话不存在"))) };
            let mut s = session.lock().unwrap();
            s.last = Instant::now();
            if let Some(reply) = s.server.handle(msg) {
                if let Some(tx) = &s.sse {
                    let _ = tx.send(format!("event: message\ndata: {}\n\n", serde_json::to_string(&reply).unwrap_or_default()));
                }
            }
            Response::from_data(b"Accepted".to_vec()).with_status_code(StatusCode(202))
        }
        (Method::Put | Method::Post, p) if p.starts_with("/files/") => {
            let name = sanitize(&urlencoding_decode(&p["/files/".len()..]));
            let body = match read_body(&mut req, MAX_FILE) {
                Ok(b) => b,
                Err(e) => return drop(req.respond(error(413, &format!("{e}（上限 {} MB）", MAX_FILE / 1024 / 1024)))),
            };
            let id = uuid::Uuid::new_v4().simple().to_string();
            let dir = state.store.join("uploads").join(&id);
            if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join(&name), &body)) {
                return drop(req.respond(error(500, &e.to_string())));
            }
            let url = format!("{}/files/uploads/{id}/{}", state.base_url, super::urlencoding_encode(&name));
            json_response(201, &json!({ "url": url, "name": name, "size": body.len() }))
        }
        (Method::Get, p) if p.starts_with("/files/") => {
            let rel = urlencoding_decode(&p["/files/".len()..]);
            let full = canonical(&state.store.join(&rel));
            let store = canonical(&state.store);
            if !full.starts_with(&store) || !full.is_file() || rel.contains("..") {
                error(404, "文件不存在")
            } else {
                match std::fs::File::open(&full) {
                    Ok(f) => {
                        let name = full.file_name().and_then(|n| n.to_str()).unwrap_or("file");
                        let resp = Response::from_file(f)
                            .with_header(h("Content-Type", "application/octet-stream"))
                            .with_header(h("Content-Disposition", &format!("attachment; filename*=UTF-8''{}", super::urlencoding_encode(name))));
                        return drop(req.respond(resp));
                    }
                    Err(_) => error(404, "文件不存在"),
                }
            }
        }
        _ => error(404, "没有这个地址：POST /mcp、GET /sse、PUT /files/<文件名>、GET /files/…"),
    };
    let _ = req.respond(response);
}

fn default_store(port: u16) -> PathBuf {
    std::env::temp_dir().join(format!("officepilot-mcp-{port}"))
}

pub fn serve(opts: Options) -> Result<(), String> {
    let loopback = is_loopback(&opts.host);
    if !loopback && opts.token.is_none() {
        return Err(format!("监听 {} 会对其它机器开放，必须设置 --token（或环境变量 OFFICEPILOT_MCP_TOKEN）", opts.host));
    }
    let store = if opts.root.as_os_str().is_empty() || opts.root == std::env::current_dir().unwrap_or_default() { default_store(opts.port) } else { opts.root.join("officepilot-mcp") };
    std::fs::create_dir_all(store.join("uploads")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(store.join("sessions")).map_err(|e| e.to_string())?;
    let addr = format!("{}:{}", opts.host.trim_matches(['[', ']']), opts.port);
    let server = HttpServer::http(&addr).map_err(|e| format!("监听 {addr} 失败：{e}"))?;
    let shown_host = if opts.host == "0.0.0.0" { "127.0.0.1".to_string() } else { opts.host.clone() };
    let base_url = opts.public_url.clone().unwrap_or_else(|| format!("http://{shown_host}:{}", opts.port)).trim_end_matches('/').to_string();
    let state = Arc::new(State { token: opts.token.clone(), store: store.clone(), base_url: base_url.clone(), loopback, sessions: Mutex::new(HashMap::new()) });
    eprintln!("officepilot mcp：{base_url}/mcp（Streamable HTTP），{base_url}/sse（旧版 SSE）；文件仓库 {}{}", store.display(), if opts.token.is_some() { "；需要令牌" } else { "" });
    {
        let state = state.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(300));
            state.cleanup();
        });
    }
    for req in server.incoming_requests() {
        let state = state.clone();
        std::thread::spawn(move || handle(state, req));
    }
    Ok(())
}
