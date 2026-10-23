//! 调用编辑器套件（OfficePilot Suite）的命令行：智能助手的格式转换、套模板、体检、渲染、
//! 读取编辑器选区都走这里。
//!
//! 命令行跑在套件自带的运行时上（ELECTRON_RUN_AS_NODE），不依赖本机的 Node；
//! 每次调用在独立的临时目录里进行，输出 JSON（`--json`）。转 PDF 和渲染会启动一个隐藏窗口，
//! 所以同时最多运行两个。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// 转 PDF、渲染这类要启动隐藏窗口的命令
pub const SLOW: Duration = Duration::from_secs(120);
pub const FAST: Duration = Duration::from_secs(30);

static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// 找到套件时智能助手才能使用这些工具（桌面版，或本机开发时开启了编辑器）
pub fn available() -> bool {
    std::env::var_os("OFFICEPILOT_SUITE_CLI").is_some() || (crate::editor::enabled() && crate::editor::suite_app().is_some())
}

/// (可执行文件, 前置参数)：测试或自定义时用 OFFICEPILOT_SUITE_CLI 直接指定一个可执行的命令行
fn command() -> Option<(PathBuf, Vec<OsString>)> {
    if let Some(cli) = std::env::var_os("OFFICEPILOT_SUITE_CLI") {
        return Some((PathBuf::from(cli), vec![]));
    }
    let app = crate::editor::suite_app()?;
    let (exe, script) = if app.extension().is_some_and(|e| e == "app") {
        (crate::editor::suite_executable(&app)?, app.join("Contents/Resources/cli/genoffice.cjs"))
    } else {
        (app.clone(), app.parent()?.join("resources/cli/genoffice.cjs"))
    };
    script.is_file().then(|| (exe, vec![script.into_os_string()]))
}

/// 一个临时工作目录，用完删除
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("officepilot-suite-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 套件报告的错误，翻译成给模型看的说明
fn describe_error(v: &Value) -> String {
    let code = v["error"].as_str().unwrap_or("");
    let message = v["message"].as_str().unwrap_or("未知错误");
    let hint = match code {
        "app_unavailable" => "编辑器套件没有在运行",
        "unsupported" => "不支持这种转换",
        "file_not_found" | "not_found" => "文件不存在",
        "target_not_found" => "找不到指定的位置",
        "unresolved_placeholder" => "模板里有占位符没有对应的值",
        _ => "",
    };
    let mut text = if hint.is_empty() { message.to_string() } else { format!("{hint}（{message}）") };
    if let Some(supported) = v["detail"]["supported"].as_str() {
        text.push_str(&format!("。支持的转换：{supported}"));
    } else if let Some(s) = v["suggestion"].as_str() {
        text.push_str(&format!("。套件的建议：{s}"));
    }
    text
}

/// 从输出里找最后一个 JSON 对象（前面可能有进度文字）
fn parse_output(stdout: &str) -> Option<Value> {
    stdout.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).find(|v| v.get("status").is_some())
}

/// 运行一条命令（自动加 `--json`），成功时返回套件的 JSON 结果
pub async fn run(args: &[OsString], cwd: &Path, timeout: Duration) -> Result<Value, String> {
    let (exe, mut full) = command().ok_or("没有找到编辑器套件（OfficePilot Suite）")?;
    full.extend(args.iter().cloned());
    full.push("--json".into());
    let _slot = SLOTS.acquire().await.map_err(|e| e.to_string())?;
    // `open` 等命令可能顺带启动套件：共用模型时它也要拿到密钥
    let env = crate::suite_ai::prepare_launch().await;
    let mut cmd = tokio::process::Command::new(&exe);
    if let Some((key, value)) = env {
        cmd.env(key, value);
    }
    let child = cmd
        .args(&full)
        .current_dir(cwd)
        .env("ELECTRON_RUN_AS_NODE", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("启动编辑器套件命令行失败：{e}"))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("编辑器套件处理超时（{} 秒）", timeout.as_secs()))?
        .map_err(|e| format!("编辑器套件命令行出错：{e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_output(&stdout) {
        Some(v) if v["status"] == "error" => Err(describe_error(&v)),
        Some(v) => Ok(v),
        None => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: String = stderr.trim().chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect();
            Err(format!("编辑器套件没有返回结果（退出码 {:?}）{}", output.status.code(), if tail.is_empty() { String::new() } else { format!("：{tail}") }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_last_json_line_and_errors() {
        let out = "page 1/1\n{\"status\":\"ok\",\"output_path\":\"/x.docx\"}\n";
        assert_eq!(parse_output(out).unwrap()["output_path"], "/x.docx");
        assert!(parse_output("starting\n").is_none());
        let e = describe_error(&json!({"status":"error","error":"unsupported","message":"cannot convert .pdf to .xyz","detail":{"supported":"pdf→docx"}}));
        assert!(e.starts_with("不支持这种转换") && e.contains("pdf→docx"), "{e}");
        let e = describe_error(&json!({"status":"error","error":"app_unavailable","message":"not running","suggestion":"open it"}));
        assert!(e.contains("没有在运行") && e.contains("open it"));
    }
}
