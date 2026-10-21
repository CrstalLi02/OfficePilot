//! stdio 传输：每行一条 JSON-RPC 消息，回复写到标准输出，日志写到标准错误

use std::io::{BufRead, Write};

use serde_json::json;

use super::protocol::Server;
use super::Workspace;

pub fn serve(ws: Workspace) -> std::io::Result<()> {
    let mut server = Server::new(ws);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    eprintln!("officepilot mcp：已就绪（stdio），工作目录 {}", server.ws.root.display());
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str(&line) {
            Ok(msg) => server.handle(msg),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("JSON 解析失败：{e}") } })),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{}", serde_json::to_string(&reply).unwrap_or_default())?;
            stdout.flush()?;
        }
    }
    Ok(())
}
