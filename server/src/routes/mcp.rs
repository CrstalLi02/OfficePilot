//! MCP 插件管理：增删改查、测试连接（列出工具）、工具开关、从 mcpServers JSON 导入

use axum::extract::Path;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::db::settings_repo;
use crate::error::AppError;
use crate::mcp::{self, pool, McpServer, NamedValue, ServerInput};
use crate::state;

pub fn router() -> Router {
    Router::new()
        .route("/api/mcp/capabilities", get(capabilities))
        .route("/api/mcp/servers", get(list).post(create))
        .route("/api/mcp/servers/:id", put(update).delete(remove))
        .route("/api/mcp/servers/:id/test", post(test))
        .route("/api/mcp/servers/:id/tools", get(tools).put(set_tools))
        .route("/api/mcp/import", post(import))
}

/// 服务端是否允许本地命令、内网地址（界面据此提示）
async fn capabilities(_user: AuthUser) -> Json<Value> {
    Json(json!({ "stdio": mcp::stdio_enabled(), "allow_private": mcp::allow_private(), "max_tools_per_run": pool::MAX_TOOLS_PER_RUN }))
}

async fn list(user: AuthUser) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let mut servers = mcp::list(&pool, &user.0.id).await?;
    // 旧版设置里保存过的 MCP 地址：第一次打开时迁移过来，并从设置里移除
    if servers.is_empty() {
        if let Some(mut settings) = settings_repo::find_by_user(&pool, &user.0.id).await? {
            if !settings.mcp_servers.is_empty() {
                let imported = mcp::import_legacy(&pool, &user.0.id, &settings.mcp_servers).await;
                tracing::info!("[MCP] 迁移旧设置里的 {imported} 个服务");
                settings.mcp_servers.clear();
                let _ = settings_repo::save_for_user(&pool, &user.0.id, &settings).await;
                servers = mcp::list(&pool, &user.0.id).await?;
            }
        }
    }
    Ok(Json(json!({ "servers": servers.iter().map(McpServer::public_json).collect::<Vec<_>>() })))
}

async fn create(user: AuthUser, Json(input): Json<ServerInput>) -> Result<Json<Value>, AppError> {
    Ok(Json(mcp::create(&state::db_pool(), &user.0.id, &input).await?.public_json()))
}

async fn update(user: AuthUser, Path(id): Path<String>, Json(input): Json<ServerInput>) -> Result<Json<Value>, AppError> {
    Ok(Json(mcp::update(&state::db_pool(), &user.0.id, &id, &input).await?.public_json()))
}

async fn remove(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    if !mcp::delete(&state::db_pool(), &user.0.id, &id).await? {
        return Err(AppError::NotFound("插件不存在".into()));
    }
    Ok(Json(json!({ "deleted": id })))
}

fn tools_json(server: &McpServer, tools: &[mcp::client::ToolInfo]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "title": t.title,
                "description": t.description,
                "read_only": t.read_only,
                "enabled": !server.config.disabled_tools.contains(&t.name),
                "exposed_name": mcp::exposed_name(&server.slug, &t.name),
            })
        })
        .collect()
}

async fn load_tools(server: &McpServer, refresh: bool) -> Json<Value> {
    if server.secrets_broken {
        return Json(json!({ "ok": false, "error": "密钥无法解密（服务端密钥变更过），请重新填写请求头或环境变量" }));
    }
    match pool::tools(server, refresh).await {
        Ok(tools) => Json(json!({ "ok": true, "status": pool::status(&server.id), "tools": tools_json(server, &tools) })),
        Err(e) => Json(json!({ "ok": false, "error": e.to_string() })),
    }
}

/// 重新连接并列出工具
async fn test(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let server = mcp::get(&state::db_pool(), &user.0.id, &id).await?;
    Ok(load_tools(&server, true).await)
}

async fn tools(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let server = mcp::get(&state::db_pool(), &user.0.id, &id).await?;
    Ok(load_tools(&server, false).await)
}

#[derive(Deserialize)]
struct ToolsInput {
    disabled: Vec<String>,
}

async fn set_tools(user: AuthUser, Path(id): Path<String>, Json(input): Json<ToolsInput>) -> Result<Json<Value>, AppError> {
    let server = mcp::set_disabled_tools(&state::db_pool(), &user.0.id, &id, input.disabled).await?;
    Ok(Json(server.public_json()))
}

#[derive(Deserialize)]
struct ImportInput {
    /// mcpServers JSON（Claude Desktop / Cursor 等的配置格式），可以是字符串或对象
    config: Value,
}

/// 把一项 mcpServers 配置转成插件输入
pub fn input_from_entry(name: &str, entry: &Value) -> Result<ServerInput, String> {
    let pairs = |key: &str| -> Vec<NamedValue> {
        entry
            .get(key)
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| NamedValue { name: k.clone(), value: v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()) })
                    .collect()
            })
            .unwrap_or_default()
    };
    let url = entry.get("url").or_else(|| entry.get("serverUrl")).and_then(Value::as_str);
    let kind = entry.get("type").or_else(|| entry.get("transport")).and_then(Value::as_str).unwrap_or_default();
    let transport = match (url, entry.get("command").and_then(Value::as_str)) {
        (Some(_), _) if kind == "sse" => "sse",
        (Some(url), _) if kind.is_empty() && reqwest::Url::parse(url).is_ok_and(|u| u.path().trim_end_matches('/').ends_with("/sse")) => "sse",
        (Some(_), _) => "http",
        (None, Some(_)) => "stdio",
        (None, None) => return Err(format!("「{name}」既没有 url 也没有 command")),
    };
    Ok(ServerInput {
        name: name.to_string(),
        slug: None,
        transport: transport.to_string(),
        url: url.map(str::to_string),
        command: entry.get("command").and_then(Value::as_str).map(str::to_string),
        args: entry
            .get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        description: entry.get("description").and_then(Value::as_str).map(str::to_string),
        headers: pairs("headers"),
        env: pairs("env"),
        enabled: Some(!entry.get("disabled").and_then(Value::as_bool).unwrap_or(false)),
    })
}

async fn import(user: AuthUser, Json(input): Json<ImportInput>) -> Result<Json<Value>, AppError> {
    let config = match input.config {
        Value::String(text) => serde_json::from_str::<Value>(&text).map_err(|e| AppError::BadRequest(format!("不是有效的 JSON：{e}")))?,
        other => other,
    };
    // 支持 { "mcpServers": {...} }、{ "servers": {...} }（VS Code）或直接是 { 名称: 配置 }
    let servers = config
        .get("mcpServers")
        .or_else(|| config.get("servers"))
        .unwrap_or(&config)
        .as_object()
        .cloned()
        .ok_or_else(|| AppError::BadRequest("没有找到 mcpServers".into()))?;
    if servers.is_empty() {
        return Err(AppError::BadRequest("mcpServers 是空的".into()));
    }
    let pool = state::db_pool();
    let mut created = Vec::new();
    let mut errors = Vec::new();
    for (name, entry) in servers.iter().take(20) {
        let result = match input_from_entry(name, entry) {
            Ok(input) => mcp::create(&pool, &user.0.id, &input).await.map_err(|e| e.to_string()),
            Err(e) => Err(e),
        };
        match result {
            Ok(server) => created.push(server.public_json()),
            Err(e) => errors.push(format!("{name}：{e}")),
        }
    }
    Ok(Json(json!({ "created": created, "errors": errors })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_common_config_shapes() {
        let stdio = input_from_entry("fs", &json!({ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"], "env": { "DEBUG": "1" } })).unwrap();
        assert_eq!((stdio.transport.as_str(), stdio.command.as_deref(), stdio.args.len()), ("stdio", Some("npx"), 3));
        assert_eq!(stdio.env[0].name, "DEBUG");

        let http = input_from_entry("gh", &json!({ "type": "http", "url": "https://example.com/mcp/", "headers": { "Authorization": "Bearer x" } })).unwrap();
        assert_eq!(http.transport, "http");
        assert_eq!(http.headers[0].value, "Bearer x");

        let sse = input_from_entry("amap", &json!({ "url": "https://example.com/sse?key=k" })).unwrap();
        assert_eq!(sse.transport, "sse", "路径以 /sse 结尾按旧版 SSE");
        let http = input_from_entry("x", &json!({ "url": "https://example.com/mcp?key=k" })).unwrap();
        assert_eq!(http.transport, "http");
        let typed = input_from_entry("x", &json!({ "type": "sse", "url": "https://example.com/events" })).unwrap();
        assert_eq!(typed.transport, "sse");

        let disabled = input_from_entry("x", &json!({ "serverUrl": "https://example.com/mcp", "disabled": true })).unwrap();
        assert_eq!(disabled.enabled, Some(false));
        assert!(input_from_entry("bad", &json!({ "foo": 1 })).is_err());
    }
}
