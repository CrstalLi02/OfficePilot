//! 读输入、原子写文件、打开文件

use std::io::Read;
use std::path::Path;

use serde_json::json;

use crate::{Done, Fail, Out};

/// JSON 输入：文件路径、- 表示标准输入，或直接写 JSON
pub fn json_input(arg: &str) -> Result<serde_json::Value, Fail> {
    let text = text_input(arg)?;
    serde_json::from_str(&text).map_err(|e| Fail::new(format!("JSON 格式不对：{e}")).hint("检查引号、逗号和括号；可以先用 jq 校验"))
}

pub fn text_input(arg: &str) -> Result<String, Fail> {
    let t = arg.trim_start();
    if arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        Ok(s)
    } else if t.starts_with('[') || t.starts_with('{') {
        Ok(arg.to_string())
    } else {
        std::fs::read_to_string(arg).map_err(|e| Fail::new(format!("读取 {arg} 失败：{e}")))
    }
}

pub fn read_file(path: &str) -> Result<Vec<u8>, Fail> {
    std::fs::read(path).map_err(|e| Fail::new(format!("读取 {path} 失败：{e}")))
}

/// 先写临时文件再改名，写到一半失败不会留下损坏的文件
pub fn write_atomic(path: &str, bytes: &[u8]) -> Result<(), Fail> {
    let target = Path::new(path);
    if let Some(dir) = target.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = target.with_file_name(format!(".{}.tmp-{}", target.file_name().and_then(|n| n.to_str()).unwrap_or("out"), std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| Fail::new(format!("写入 {path} 失败：{e}")))?;
    std::fs::rename(&tmp, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Fail::new(format!("写入 {path} 失败：{e}"))
    })
}

pub fn open(file: &str) -> Out {
    let path = Path::new(file);
    if !path.exists() {
        return Err(Fail::new(format!("文件不存在：{file}")));
    }
    let abs = std::fs::canonicalize(path)?;
    #[cfg(target_os = "macos")]
    let status = {
        let app = Path::new("/Applications/OfficePilot.app");
        let mut cmd = std::process::Command::new("open");
        if app.exists() {
            cmd.args(["-a", "OfficePilot"]);
        }
        cmd.arg(&abs).status()
    };
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("cmd").args(["/C", "start", ""]).arg(&abs).status();
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = std::process::Command::new("xdg-open").arg(&abs).status();
    match status {
        Ok(s) if s.success() => Ok(Done::ok(format!("已打开 {}", abs.display()), json!({ "path": abs }))),
        Ok(s) => Err(Fail::new(format!("打开失败（退出码 {:?}）", s.code()))),
        Err(e) => Err(Fail::new(format!("打开失败：{e}"))),
    }
}
