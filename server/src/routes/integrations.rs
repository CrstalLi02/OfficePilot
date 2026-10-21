//! 集成：把 officepilot skill 装进本机的编程 Agent，给出 MCP 接入方式，桌面版把命令行装到 PATH。
//!
//! 写入本机目录的操作只在桌面版（或本机开发时显式开启）提供，并且只在用户点击时执行；
//! 网页 / Docker 部署只提供 skill 下载和接入说明。

use std::path::{Path, PathBuf};

use axum::extract::Path as UrlPath;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::error::AppError;

/// skill 内容随服务端一起打包
const SKILL_MD: &str = include_str!("../../../cli/skill/officepilot/SKILL.md");
const SKILL_NAME: &str = "officepilot";
const VERSION_FILE: &str = ".officepilot-version";

pub fn router() -> Router {
    Router::new()
        .route("/api/integrations", get(overview))
        .route("/api/integrations/skill.zip", get(skill_zip))
        .route("/api/integrations/skill/:agent", post(install).delete(uninstall))
        .route("/api/integrations/cli/link", post(link_cli))
        .route("/api/integrations/cli/link", delete(unlink_cli))
}

fn bad(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

/// 能否写本机目录：桌面版，或本机开发时设置 WALIOFFICE_LOCAL_INTEGRATIONS=1
fn local_allowed() -> bool {
    crate::config::is_desktop() || std::env::var("WALIOFFICE_LOCAL_INTEGRATIONS").map(|v| v == "1").unwrap_or(false)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

fn skill_version() -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(SKILL_MD.as_bytes()))[..10].to_string()
}

struct Agent {
    id: &'static str,
    name: &'static str,
    /// 用来判断是否装了这个 Agent 的目录（相对 HOME）
    marker: &'static str,
    /// skills 目录（相对 HOME）
    skills: &'static str,
}

/// 各 Agent 读取用户级 skill 的目录：Claude Code ~/.claude/skills，Cursor ~/.cursor/skills，
/// Gemini CLI ~/.gemini/skills，Codex 读跨 Agent 的 ~/.agents/skills
const AGENTS: &[Agent] = &[
    Agent { id: "claude-code", name: "Claude Code", marker: ".claude", skills: ".claude/skills" },
    Agent { id: "codex", name: "Codex", marker: ".codex", skills: ".agents/skills" },
    Agent { id: "cursor", name: "Cursor", marker: ".cursor", skills: ".cursor/skills" },
    Agent { id: "gemini", name: "Gemini CLI", marker: ".gemini", skills: ".gemini/skills" },
];

fn agent_dir(agent: &Agent) -> Option<PathBuf> {
    Some(home()?.join(agent.skills).join(SKILL_NAME))
}

fn installed_version(dir: &Path) -> Option<String> {
    if !dir.join("SKILL.md").exists() {
        return None;
    }
    Some(std::fs::read_to_string(dir.join(VERSION_FILE)).map(|v| v.trim().to_string()).unwrap_or_else(|_| "unknown".into()))
}

/// 本机 officepilot 的位置：桌面版由环境变量告知（随安装包附带），否则在 PATH 里找
pub fn cli_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OFFICEPILOT_CLI").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let exe = if cfg!(windows) { "officepilot.exe" } else { "officepilot" };
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(exe)).find(|p| p.is_file()))
}

fn link_path() -> Option<PathBuf> {
    Some(home()?.join(".local").join("bin").join(if cfg!(windows) { "officepilot.exe" } else { "officepilot" }))
}

fn quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.contains(' ') { format!("\"{s}\"") } else { s.into_owned() }
}

async fn overview(_user: AuthUser) -> Result<Json<Value>, AppError> {
    let latest = skill_version();
    let local = local_allowed();
    let agents: Vec<Value> = AGENTS
        .iter()
        .map(|a| {
            let dir = agent_dir(a);
            let detected = home().map(|h| h.join(a.marker).exists()).unwrap_or(false);
            let installed = if local { dir.as_deref().and_then(installed_version) } else { None };
            // 同名但不是我们装的 skill：不显示安装 / 更新
            let foreign = local && dir.as_deref().map(|d| d.join("SKILL.md").exists() && !d.join(VERSION_FILE).exists()).unwrap_or(false);
            json!({
                "id": a.id,
                "name": a.name,
                "detected": local && detected,
                "dir": if local { dir.map(|d| d.to_string_lossy().into_owned()) } else { None },
                "installed": installed.is_some() && !foreign,
                "foreign": foreign,
                "up_to_date": installed.as_deref() == Some(latest.as_str()),
            })
        })
        .collect();
    let cli = cli_path();
    let command = cli.as_deref().map(quote).unwrap_or_else(|| "officepilot".into());
    let linked = link_path().map(|l| l.exists() && std::fs::read_link(&l).ok().as_deref() == cli.as_deref()).unwrap_or(false);
    Ok(Json(json!({
        "local": local,
        "skill_version": latest,
        "agents": agents,
        "cli": {
            "path": cli.map(|p| p.to_string_lossy().into_owned()),
            "link": link_path().map(|p| p.to_string_lossy().into_owned()),
            "linked": linked,
        },
        "mcp": {
            "claude_code": format!("claude mcp add --transport stdio officepilot -- {command} mcp"),
            "json": serde_json::to_string_pretty(&json!({ "mcpServers": { "officepilot": { "command": cli_path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "officepilot".into()), "args": ["mcp"] } } })).unwrap_or_default(),
            "http": format!("{command} mcp --http 3093 --token <令牌>"),
        },
    })))
}

async fn skill_zip(_user: AuthUser) -> Result<Response, AppError> {
    use std::io::Write;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    w.start_file(format!("{SKILL_NAME}/SKILL.md"), opts).map_err(|e| bad(e.to_string()))?;
    w.write_all(SKILL_MD.as_bytes()).map_err(|e| bad(e.to_string()))?;
    let bytes = w.finish().map_err(|e| bad(e.to_string()))?.into_inner();
    Ok((
        [(header::CONTENT_TYPE, "application/zip".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{SKILL_NAME}-skill.zip\""))],
        bytes,
    )
        .into_response())
}

fn find_agent(id: &str) -> Result<&'static Agent, AppError> {
    AGENTS.iter().find(|a| a.id == id).ok_or_else(|| AppError::NotFound(format!("没有 {id} 这个集成")))
}

async fn install(_user: AuthUser, UrlPath(agent): UrlPath<String>) -> Result<Json<Value>, AppError> {
    if !local_allowed() {
        return Err(bad("网页版不能写入你电脑上的目录，请下载 skill 压缩包手动安装"));
    }
    let agent = find_agent(&agent)?;
    let dir = agent_dir(agent).ok_or_else(|| bad("找不到用户主目录"))?;
    // 已存在但不是我们装的同名 skill，不覆盖
    if dir.join("SKILL.md").exists() && !dir.join(VERSION_FILE).exists() {
        return Err(bad(format!("{} 已有一个不是 OfficePilot 安装的 {SKILL_NAME} skill，未覆盖", dir.display())));
    }
    std::fs::create_dir_all(&dir).map_err(|e| bad(format!("创建 {} 失败：{e}", dir.display())))?;
    std::fs::write(dir.join("SKILL.md"), SKILL_MD).map_err(|e| bad(e.to_string()))?;
    std::fs::write(dir.join(VERSION_FILE), skill_version()).map_err(|e| bad(e.to_string()))?;
    tracing::info!("已把 officepilot skill 安装到 {}", dir.display());
    Ok(Json(json!({ "installed": true, "dir": dir, "version": skill_version() })))
}

async fn uninstall(_user: AuthUser, UrlPath(agent): UrlPath<String>) -> Result<Json<Value>, AppError> {
    if !local_allowed() {
        return Err(bad("网页版不能修改你电脑上的目录"));
    }
    let agent = find_agent(&agent)?;
    let dir = agent_dir(agent).ok_or_else(|| bad("找不到用户主目录"))?;
    if !dir.join(VERSION_FILE).exists() {
        return Err(bad("这个 skill 不是 OfficePilot 安装的，未删除"));
    }
    // 只删我们写入的两个文件；目录空了再删目录
    let _ = std::fs::remove_file(dir.join("SKILL.md"));
    let _ = std::fs::remove_file(dir.join(VERSION_FILE));
    let _ = std::fs::remove_dir(&dir);
    Ok(Json(json!({ "installed": false })))
}

#[derive(Deserialize, Default)]
struct LinkRequest {
    /// 目标位置已有其它文件时，用户确认后才替换
    #[serde(default)]
    replace: bool,
}

async fn link_cli(_user: AuthUser, body: Option<Json<LinkRequest>>) -> Result<Json<Value>, AppError> {
    if !crate::config::is_desktop() {
        return Err(bad("只有桌面版可以把命令行安装到 PATH"));
    }
    let replace = body.map(|b| b.0.replace).unwrap_or(false);
    let cli = cli_path().ok_or_else(|| bad("安装包里没有找到 officepilot 命令行"))?;
    let link = link_path().ok_or_else(|| bad("找不到用户主目录"))?;
    if link.exists() || link.symlink_metadata().is_ok() {
        if std::fs::read_link(&link).ok().as_deref() == Some(cli.as_path()) {
            return Ok(Json(json!({ "linked": true, "path": link })));
        }
        if !replace {
            // 交给界面询问用户是否替换
            return Ok(Json(json!({ "linked": false, "exists": true, "path": link })));
        }
        std::fs::remove_file(&link).map_err(|e| bad(e.to_string()))?;
    }
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir).map_err(|e| bad(e.to_string()))?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&cli, &link).map_err(|e| bad(format!("创建链接失败：{e}")))?;
    #[cfg(windows)]
    std::fs::copy(&cli, &link).map_err(|e| bad(format!("复制失败：{e}")))?;
    let on_path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| Some(d.as_path()) == link.parent())).unwrap_or(false);
    Ok(Json(json!({ "linked": true, "path": link, "on_path": on_path })))
}

async fn unlink_cli(_user: AuthUser) -> Result<Json<Value>, AppError> {
    if !crate::config::is_desktop() {
        return Err(bad("只有桌面版可以修改 PATH 里的命令"));
    }
    let link = link_path().ok_or_else(|| bad("找不到用户主目录"))?;
    match (std::fs::read_link(&link), cli_path()) {
        (Ok(target), Some(cli)) if target == cli => {
            std::fs::remove_file(&link).map_err(|e| bad(e.to_string()))?;
            Ok(Json(json!({ "linked": false })))
        }
        _ => Err(bad("这个命令不是 OfficePilot 安装的，未删除")),
    }
}
