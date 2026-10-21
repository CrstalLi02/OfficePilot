//! MCP 服务端：stdio 与 HTTP 两种传输的协议测试

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_officepilot");

struct Stdio_ {
    child: Child,
    out: BufReader<std::process::ChildStdout>,
    id: u64,
}

impl Stdio_ {
    fn start(dir: &std::path::Path) -> Self {
        let mut child = Command::new(BIN).arg("mcp").current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Self { child, out, id: 0 }
    }
    fn send(&mut self, msg: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": self.id, "method": method, "params": params }));
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], self.id);
        v
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.call("tools/call", json!({ "name": name, "arguments": args }))["result"].clone()
    }
}

impl Drop for Stdio_ {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn stdio_session() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Stdio_::start(tmp.path());
    let init = s.call("initialize", json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }));
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("deck_start"));
    s.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let tools = s.call("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in ["read", "docs_apply", "sheet_apply", "slides_apply", "create_pptx", "deck_page", "open"] {
        assert!(names.contains(&n), "缺少工具 {n}");
    }
    let read_tool = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "read").unwrap().clone();
    assert_eq!(read_tool["annotations"]["readOnlyHint"], true);

    let r = s.tool("create_docx", json!({ "out": "a.docx", "markdown": "# 标题\n\n正文 2025" }));
    assert_eq!(r["isError"], false, "{r}");
    let r = s.tool("docs_apply", json!({ "file": "a.docx", "ops": [{ "op": "replace_text", "find": "2025", "replace": "2026" }] }));
    assert_eq!(r["isError"], false, "{r}");
    let r = s.tool("read", json!({ "file": "a.docx" }));
    assert_eq!(r["structuredContent"]["blocks"][1]["text"], "正文 2026");
    // 类型不匹配、操作出错时 isError 并带提示
    let r = s.tool("sheet_apply", json!({ "file": "a.docx", "ops": [] }));
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"].as_str().unwrap().contains("docs_apply"));
    let r = s.tool("docs_apply", json!({ "file": "a.docx", "ops": [{ "op": "replace_text", "find": "不存在", "replace": "x" }] }));
    assert_eq!(r["isError"], true);
    // 一次生成 PPT（规格内联）
    let r = s.tool("create_pptx", json!({ "out": "t.pptx", "spec": { "title": "t", "pages": [{ "layout": "cover", "title": "封面" }, { "layout": "bullets", "title": "要点", "bullets": ["一", "二"] }] } }));
    assert_eq!(r["isError"], false, "{r}");
    let r = s.tool("info", json!({ "file": "t.pptx" }));
    assert_eq!(r["structuredContent"]["info"]["slides"], 2);
    // 资源
    let res = s.call("resources/read", json!({ "uri": "officepilot://guide/deck" }));
    assert!(res["result"]["contents"][0]["text"].as_str().unwrap().contains("deck start"));
    // 未知方法、未知工具
    assert_eq!(s.call("nope", json!({}))["error"]["code"], -32601);
    assert_eq!(s.call("tools/call", json!({ "name": "nope" }))["error"]["code"], -32602);
}

#[test]
fn stdio_allowed_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let inside = tmp.path().join("inside");
    std::fs::create_dir_all(&inside).unwrap();
    let mut child = Command::new(BIN).arg("mcp").current_dir(&inside).env("OFFICEPILOT_ALLOWED_ROOTS", &inside).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let stdin = child.stdin.as_mut().unwrap();
    writeln!(stdin, "{}", json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "create_docx", "arguments": { "out": "../escape.docx", "markdown": "x" } } })).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert!(!tmp.path().join("escape.docx").exists());
    let _ = child.kill();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Http {
    child: Child,
    base: String,
}

impl Drop for Http {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn start_http(dir: &std::path::Path, token: Option<&str>) -> Http {
    let port = free_port();
    let mut cmd = Command::new(BIN);
    cmd.args(["mcp", "--http", &port.to_string(), "--root"]).arg(dir).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(t) = token {
        cmd.args(["--token", t]);
    }
    let child = cmd.spawn().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..50 {
        if ureq::get(&base).call().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Http { child, base }
}

fn rpc(base: &str, token: &str, session: Option<&str>, body: Value) -> Result<(Value, Option<String>), u16> {
    let mut req = ureq::post(&format!("{base}/mcp")).set("Authorization", &format!("Bearer {token}")).set("Content-Type", "application/json").set("Accept", "application/json, text/event-stream");
    if let Some(s) = session {
        req = req.set("Mcp-Session-Id", s);
    }
    match req.send_string(&body.to_string()) {
        Ok(resp) => {
            let sid = resp.header("Mcp-Session-Id").map(str::to_string);
            let text = resp.into_string().unwrap();
            Ok((if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap() }, sid))
        }
        Err(ureq::Error::Status(code, _)) => Err(code),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn http_session_files_and_security() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = start_http(tmp.path(), Some("secret"));
    let base = srv.base.clone();

    // 令牌
    assert_eq!(rpc(&base, "wrong", None, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).unwrap_err(), 401);
    // 外部网站来源
    let evil = ureq::post(&format!("{base}/mcp")).set("Authorization", "Bearer secret").set("Origin", "https://evil.example").send_string("{}");
    assert!(matches!(evil, Err(ureq::Error::Status(403, _))));
    // 没有会话
    assert_eq!(rpc(&base, "secret", None, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).unwrap_err(), 400);
    assert_eq!(rpc(&base, "secret", Some("bogus"), json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).unwrap_err(), 404);

    let (init, sid) = rpc(&base, "secret", None, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "officepilot");
    let sid = sid.expect("返回 Mcp-Session-Id");
    // 通知返回 202、没有内容
    let (n, _) = rpc(&base, "secret", Some(&sid), json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).unwrap();
    assert!(n.is_null());
    let (tools, _) = rpc(&base, "secret", Some(&sid), json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).unwrap();
    assert!(!tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "open"), "HTTP 模式不提供 open");

    // 上传 → 以 URL 编辑 → 下载
    let md = "# 方案\n\n预算 100 万";
    let up = ureq::put(&format!("{base}/files/{}", "%E6%96%B9%E6%A1%88.md")).set("Authorization", "Bearer secret").send_string(md).unwrap().into_string().map(|t| serde_json::from_str::<Value>(&t).unwrap()).unwrap();
    let url = up["url"].as_str().unwrap().to_string();
    let call = |name: &str, args: Value| rpc(&base, "secret", Some(&sid), json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": name, "arguments": args } })).unwrap().0["result"].clone();
    let r = call("convert", json!({ "file": url, "to": "docx", "out": "plan.docx" }));
    assert_eq!(r["isError"], false, "{r}");
    let out_url = r["structuredContent"]["output_url"].as_str().unwrap().to_string();
    assert!(r["content"].as_array().unwrap().iter().any(|c| c["type"] == "resource"), "小文件内嵌返回");
    let mut bytes = Vec::new();
    ureq::get(&out_url).set("Authorization", "Bearer secret").call().unwrap().into_reader().read_to_end(&mut bytes).unwrap();
    assert!(bytes.starts_with(b"PK"));
    let r = call("read", json!({ "file": "plan.docx" }));
    assert_eq!(r["structuredContent"]["blocks"][1]["text"], "预算 100 万");

    // 越界与外部地址
    let r = call("info", json!({ "file": "/etc/hosts" }));
    assert_eq!(r["isError"], true);
    let r = call("info", json!({ "file": "https://example.com/x.docx" }));
    assert_eq!(r["isError"], true);
    let r = call("create_pptx", json!({ "out": "x.pptx", "spec": { "pages": [{ "layout": "image-left", "title": "t", "image": { "path": "/etc/hosts" } }] } }));
    assert_eq!(r["isError"], true, "规格里的图片路径也受限：{r}");
    // 下载时不能跳出仓库
    assert!(ureq::get(&format!("{base}/files/..%2F..%2Fetc%2Fhosts")).set("Authorization", "Bearer secret").call().is_err());

    // 结束会话
    ureq::delete(&format!("{base}/mcp")).set("Authorization", "Bearer secret").set("Mcp-Session-Id", &sid).call().unwrap();
    assert_eq!(rpc(&base, "secret", Some(&sid), json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" })).unwrap_err(), 404);
}

#[test]
fn http_legacy_sse() {
    let tmp = tempfile::tempdir().unwrap();
    let srv = start_http(tmp.path(), None);
    let resp = ureq::get(&format!("{}/sse", srv.base)).call().unwrap();
    let mut reader = BufReader::new(resp.into_reader());
    let mut endpoint = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if let Some(d) = line.strip_prefix("data: ") {
            endpoint = d.trim().to_string();
            break;
        }
    }
    assert!(endpoint.starts_with("/messages?sessionId="));
    let posted = ureq::post(&format!("{}{endpoint}", srv.base)).send_string(&json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" }).to_string()).unwrap();
    assert_eq!(posted.status(), 202);
    let mut data = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if let Some(d) = line.strip_prefix("data: ") {
            data = d.trim().to_string();
            break;
        }
    }
    let v: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(v["id"], 7);
    assert!(v["result"]["tools"].as_array().unwrap().len() > 10);
}

#[test]
fn http_requires_token_off_loopback() {
    let port = free_port();
    let out = Command::new(BIN).args(["mcp", "--http", &port.to_string(), "--host", "0.0.0.0"]).env_remove("OFFICEPILOT_MCP_TOKEN").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--token"));
}
