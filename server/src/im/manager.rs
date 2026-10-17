//! 渠道连接管理：按渠道启停长连接，记录连接状态与机器人信息（内存中，供界面展示）。

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::Utc;
use once_cell::sync::Lazy;
use serde::Serialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::Channel;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// connecting / connected / error / stopped / webhook（等待回调）/ disabled
    pub state: String,
    pub error: Option<String>,
    pub bot_name: Option<String>,
    #[serde(skip)]
    pub bot_open_id: Option<String>,
    pub last_event_at: Option<String>,
    pub updated_at: String,
}

static STATUS: Lazy<Mutex<HashMap<String, Status>>> = Lazy::new(Default::default);
static RUNNING: Lazy<Mutex<HashMap<String, CancellationToken>>> = Lazy::new(Default::default);

pub fn status(channel_id: &str) -> Value {
    STATUS
        .lock()
        .unwrap()
        .get(channel_id)
        .map(|s| json!(s))
        .unwrap_or_else(|| json!({ "state": "stopped" }))
}

pub fn set_state(channel_id: &str, state: &str, error: Option<String>) {
    let mut all = STATUS.lock().unwrap();
    let entry = all.entry(channel_id.to_string()).or_default();
    entry.state = state.to_string();
    entry.error = error;
    entry.updated_at = Utc::now().to_rfc3339();
}

pub fn set_bot(channel_id: &str, name: &str, open_id: &str) {
    let mut all = STATUS.lock().unwrap();
    let entry = all.entry(channel_id.to_string()).or_default();
    entry.bot_name = Some(name.to_string());
    entry.bot_open_id = (!open_id.is_empty()).then(|| open_id.to_string());
}

pub fn bot_open_id(channel_id: &str) -> Option<String> {
    STATUS.lock().unwrap().get(channel_id).and_then(|s| s.bot_open_id.clone())
}

/// 收到事件时记录时间（界面上显示“最近收到消息”）
pub fn touch(channel_id: &str) {
    if let Some(entry) = STATUS.lock().unwrap().get_mut(channel_id) {
        entry.last_event_at = Some(Utc::now().to_rfc3339());
    }
}

pub fn stop(channel_id: &str) {
    if let Some(token) = RUNNING.lock().unwrap().remove(channel_id) {
        token.cancel();
    }
}

/// 按渠道当前配置（重新）建立连接；停用或删除后调用会断开
pub fn apply(channel: &Channel) {
    stop(&channel.id);
    if !channel.enabled {
        set_state(&channel.id, "disabled", None);
        return;
    }
    if channel.secrets_broken {
        set_state(&channel.id, "error", Some("密钥无法解密（服务端密钥变更过），请重新填写 App Secret".into()));
        return;
    }
    match channel.config.connection.as_str() {
        "ws" => {
            let token = CancellationToken::new();
            RUNNING.lock().unwrap().insert(channel.id.clone(), token.clone());
            tokio::spawn(super::feishu_ws::run(channel.clone(), token));
        }
        _ => {
            // 事件回调方式：没有常驻连接，取一次机器人信息用于识别 @
            set_state(&channel.id, "webhook", None);
            let channel = channel.clone();
            tokio::spawn(async move {
                match super::feishu::Client::new(&channel).bot_info().await {
                    Ok((name, open_id)) => set_bot(&channel.id, &name, &open_id),
                    Err(e) => set_state(&channel.id, "error", Some(format!("{e:#}"))),
                }
            });
        }
    }
}

pub fn forget(channel_id: &str) {
    stop(channel_id);
    STATUS.lock().unwrap().remove(channel_id);
}

/// 服务启动时连上所有启用的渠道，并清理过期的去重记录
pub async fn start_all() {
    let pool = crate::state::db_pool();
    super::prune_events(&pool).await;
    match super::enabled_channels(&pool).await {
        Ok(channels) => {
            for channel in &channels {
                apply(channel);
            }
            if !channels.is_empty() {
                tracing::info!("[IM] 已启动 {} 个渠道", channels.len());
            }
        }
        Err(e) => tracing::error!("[IM] 读取渠道失败: {e:?}"),
    }
}
