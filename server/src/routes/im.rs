//! IM 接入：渠道管理、测试连接、绑定码、已绑定账号，以及飞书事件回调入口（免登录，验签）

use axum::body::Bytes;
use axum::extract::Path;
use axum::http::HeaderMap;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::im::{self, feishu, manager, ChannelInput};
use crate::state;

pub fn router() -> Router {
    Router::new()
        .route("/api/im/channels", get(list).post(create))
        .route("/api/im/channels/:id", put(update).delete(remove))
        .route("/api/im/channels/:id/test", post(test))
        .route("/api/im/channels/:id/bind-code", post(bind_code))
        .route("/api/im/channels/:id/bindings", get(bindings))
        .route("/api/im/channels/:id/bindings/:binding_id", delete(unbind))
        .route("/api/im/feishu/:id/event", post(feishu_event))
}

async fn list(user: AuthUser) -> Result<Json<Value>, AppError> {
    let channels = im::list_channels(&state::db_pool(), &user.0.id).await?;
    Ok(Json(json!({ "channels": channels.iter().map(|c| c.public_json()).collect::<Vec<_>>() })))
}

async fn create(user: AuthUser, Json(input): Json<ChannelInput>) -> Result<Json<Value>, AppError> {
    let channel = im::create_channel(&state::db_pool(), &user.0.id, &input).await?;
    manager::apply(&channel);
    Ok(Json(channel.public_json()))
}

async fn update(user: AuthUser, Path(id): Path<String>, Json(input): Json<ChannelInput>) -> Result<Json<Value>, AppError> {
    let channel = im::update_channel(&state::db_pool(), &user.0.id, &id, &input).await?;
    manager::apply(&channel);
    Ok(Json(channel.public_json()))
}

async fn remove(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    if !im::delete_channel(&state::db_pool(), &user.0.id, &id).await? {
        return Err(AppError::NotFound("渠道不存在".into()));
    }
    manager::forget(&id);
    Ok(Json(json!({ "deleted": id })))
}

/// 用保存的凭证换 token 并读取机器人信息
async fn test(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let channel = im::get_channel(&state::db_pool(), &user.0.id, &id).await?;
    if channel.secrets_broken {
        return Err(AppError::BadRequest("密钥无法解密，请重新填写 App Secret".into()));
    }
    match feishu::Client::new(&channel).bot_info().await {
        Ok((name, open_id)) => {
            manager::set_bot(&channel.id, &name, &open_id);
            Ok(Json(json!({ "ok": true, "bot_name": name })))
        }
        Err(e) => Ok(Json(json!({ "ok": false, "error": format!("{e:#}") }))),
    }
}

async fn bind_code(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let channel = im::get_channel(&state::db_pool(), &user.0.id, &id).await?;
    let (code, expires_in) = im::issue_bind_code(&channel.id);
    Ok(Json(json!({ "code": code, "expires_in": expires_in })))
}

async fn bindings(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let channel = im::get_channel(&pool, &user.0.id, &id).await?;
    Ok(Json(json!({ "bindings": im::list_bindings(&pool, &channel.id).await? })))
}

async fn unbind(user: AuthUser, Path((id, binding_id)): Path<(String, String)>) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let channel = im::get_channel(&pool, &user.0.id, &id).await?;
    if !im::remove_binding(&pool, &channel.id, &binding_id).await? {
        return Err(AppError::NotFound("绑定不存在".into()));
    }
    Ok(Json(json!({ "deleted": binding_id })))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// 飞书事件回调（「事件与回调」→ 将事件发送至开发者服务器）
async fn feishu_event(Path(id): Path<String>, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, AppError> {
    let channel = im::find_channel(&state::db_pool(), &id)
        .await?
        .filter(|c| c.enabled && c.config.connection == "webhook")
        .ok_or_else(|| AppError::NotFound("渠道不存在或未启用事件回调".into()))?;
    let mut event: Value = serde_json::from_slice(&body).map_err(|_| AppError::BadRequest("不是 JSON".into()))?;
    let encrypt_key = channel.secrets.encrypt_key.as_str();
    if !encrypt_key.is_empty() {
        // 配置了 Encrypt Key 时飞书会带签名
        if let Some(sig) = header(&headers, "x-lark-signature") {
            let expected = feishu::signature(
                header(&headers, "x-lark-request-timestamp").unwrap_or_default(),
                header(&headers, "x-lark-request-nonce").unwrap_or_default(),
                encrypt_key,
                &body,
            );
            if !sig.eq_ignore_ascii_case(&expected) {
                return Err(AppError::Unauthorized);
            }
        }
        if let Some(encrypted) = event.get("encrypt").and_then(Value::as_str) {
            let plain = feishu::decrypt(encrypt_key, encrypted).map_err(|e| AppError::BadRequest(e.to_string()))?;
            event = serde_json::from_str(&plain).map_err(|_| AppError::BadRequest("解密后不是 JSON".into()))?;
        }
    }
    let token = event
        .get("token")
        .or_else(|| event.pointer("/header/token"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if token != channel.secrets.verification_token {
        return Err(AppError::Unauthorized);
    }
    if event.get("type").and_then(Value::as_str) == Some("url_verification") {
        return Ok(Json(json!({ "challenge": event.get("challenge").cloned().unwrap_or_default() })));
    }
    // 3 秒内必须应答，处理放到后台
    tokio::spawn(im::router::handle_event(channel.id.clone(), event));
    Ok(Json(json!({ "code": 0 })))
}
