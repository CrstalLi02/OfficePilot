//! MCP 服务端：把 officepilot 的每个命令作为一个 MCP 工具提供。
//!
//! - stdio：助手自己启动 `officepilot mcp`，按行收发 JSON-RPC；
//! - HTTP：`officepilot mcp --http 端口`，Streamable HTTP（兼容旧版 SSE），文件通过 /files 上传下载。
//!
//! 路径一律先解析成绝对路径再检查：设置了允许目录时，解析符号链接后必须落在允许目录里。

pub mod http;
pub mod protocol;
pub mod stdio;
pub mod tools;

use std::path::{Component, Path, PathBuf};

use crate::Fail;

/// HTTP 模式下的文件仓库信息
#[derive(Clone)]
pub struct Remote {
    /// 对外地址，如 http://127.0.0.1:3093
    pub base_url: String,
    /// 仓库根目录：u-<id>/ 是上传的文件，s-<会话>/ 是会话临时目录
    pub store: PathBuf,
    /// 会话目录名（s-xxxx）
    pub bucket: String,
}

/// 一个会话能访问的范围
#[derive(Clone)]
pub struct Workspace {
    /// 相对路径的基准目录
    pub root: PathBuf,
    /// 允许访问的目录（已规范化）；为空表示不限制
    pub allowed: Vec<PathBuf>,
    pub remote: Option<Remote>,
}

/// 去掉 . 和 ..（不访问磁盘）
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 规范化：存在的部分解析符号链接，不存在的尾部按字面拼接
pub fn canonical(path: &Path) -> PathBuf {
    let path = lexical(path);
    let mut existing = path.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent().map(Path::to_path_buf)) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                existing = parent;
            }
            _ => break,
        }
    }
    let mut out = std::fs::canonicalize(&existing).unwrap_or(existing);
    for name in tail.into_iter().rev() {
        out.push(name);
    }
    out
}

pub fn allowed_roots_from_env() -> Vec<PathBuf> {
    std::env::var_os("OFFICEPILOT_ALLOWED_ROOTS")
        .map(|v| std::env::split_paths(&v).filter(|p| !p.as_os_str().is_empty()).map(|p| canonical(&p)).collect())
        .unwrap_or_default()
}

impl Workspace {
    pub fn local(root: PathBuf) -> Self {
        Self { root: canonical(&root), allowed: allowed_roots_from_env(), remote: None }
    }

    /// 把参数里的路径或本服务的文件 URL 解析成允许范围内的绝对路径
    pub fn resolve(&self, input: &str) -> Result<PathBuf, Fail> {
        let input = input.trim();
        if input.is_empty() {
            return Err(Fail::new("路径为空"));
        }
        let path = if input.starts_with("http://") || input.starts_with("https://") {
            let remote = self.remote.as_ref().ok_or_else(|| Fail::new("stdio 模式请直接给本机文件路径"))?;
            let rest = input
                .strip_prefix(remote.base_url.trim_end_matches('/'))
                .and_then(|r| r.strip_prefix("/files/"))
                .ok_or_else(|| Fail::new("只接受本服务 /files/ 下的地址，不会去下载外部链接").hint("先用 PUT /files/<文件名> 上传，再把返回的 url 传进来"))?;
            let rel = urlencoding_decode(rest.split(['?', '#']).next().unwrap_or(""));
            remote.store.join(rel)
        } else {
            let p = Path::new(input);
            if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) }
        };
        let full = canonical(&path);
        if !self.allowed.is_empty() && !self.allowed.iter().any(|a| full.starts_with(a)) {
            return Err(Fail::new(format!("{input} 不在允许访问的目录里")).hint(match &self.remote {
                Some(_) => "HTTP 模式只能访问本会话的目录和上传的文件；相对路径会放进会话目录".to_string(),
                None => "由 OFFICEPILOT_ALLOWED_ROOTS 限定".to_string(),
            }));
        }
        Ok(full)
    }

    pub fn resolve_str(&self, input: &str) -> Result<String, Fail> {
        Ok(self.resolve(input)?.to_string_lossy().into_owned())
    }

    /// 输出文件对应的下载地址（HTTP 模式）
    pub fn url_of(&self, path: &Path) -> Option<String> {
        let remote = self.remote.as_ref()?;
        let store = canonical(&remote.store);
        let rel = canonical(path).strip_prefix(&store).ok()?.to_path_buf();
        let parts: Vec<String> = rel.components().map(|c| urlencoding_encode(&c.as_os_str().to_string_lossy())).collect();
        Some(format!("{}/files/{}", remote.base_url.trim_end_matches('/'), parts.join("/")))
    }
}

pub fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confines_paths() {
        let tmp = std::env::temp_dir().join(format!("op-ws-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("inside")).unwrap();
        let root = canonical(&tmp.join("inside"));
        let ws = Workspace { root: root.clone(), allowed: vec![root.clone()], remote: None };
        assert!(ws.resolve("a.docx").unwrap().starts_with(&root));
        assert!(ws.resolve("../outside.docx").is_err());
        assert!(ws.resolve("/etc/passwd").is_err());
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink("/etc", root.join("link"));
            assert!(ws.resolve("link/passwd").is_err(), "符号链接不能逃出允许目录");
        }
        assert!(ws.resolve("https://evil.example/x.docx").is_err());
        assert_eq!(urlencoding_decode(&urlencoding_encode("报告 v2.docx")), "报告 v2.docx");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
