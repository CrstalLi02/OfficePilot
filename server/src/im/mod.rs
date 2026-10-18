//! IM 远程控制：在飞书（之后是企业微信）里给机器人发消息，驱动 Agent 干活。
//!
//! - 每个用户配置自己的机器人应用（渠道），密钥加密落库，接口只返回是否已填写；
//! - 只有绑定过的 IM 账号能触发运行：网页上生成 6 位绑定码，在 IM 里发「绑定 123456」；
//! - 每个 IM 会话（私聊 / 群）对应一个系统会话，可用 /new 换新会话、/mode 切模式；
//! - 收到的事件按 id 去重（平台会重推），运行通过 `start_chat_task` 以 `im` 来源发起。

pub mod feishu;
pub mod feishu_ws;
pub mod manager;
pub mod router;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;

use crate::agent::plan::AgentMode;
use crate::db::{text_column, DbPool};
use crate::error::{AppError, AppResult};

/// 每个用户最多的渠道数
const MAX_CHANNELS_PER_USER: i64 = 10;
/// 绑定码有效期
const BIND_CODE_TTL: Duration = Duration::from_secs(600);

fn bad(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

// ───────────────────────── 渠道 ─────────────────────────

/// 公开配置（明文存储，接口原样返回）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// 飞书：App ID
    #[serde(default)]
    pub app_id: String,
    /// feishu（open.feishu.cn）/ lark（open.larksuite.com）
    #[serde(default = "default_domain")]
    pub domain: String,
    /// ws（长连接，不需要公网地址）/ webhook（事件回调 URL）
    #[serde(default = "default_connection")]
    pub connection: String,
    /// 新会话的默认模式
    #[serde(default = "default_mode")]
    pub default_mode: AgentMode,
    /// 运行时使用的时区
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_domain() -> String {
    "feishu".into()
}
fn default_connection() -> String {
    "ws".into()
}
fn default_mode() -> AgentMode {
    AgentMode::Craft
}
fn default_timezone() -> String {
    crate::schedules::DEFAULT_TIMEZONE.into()
}

/// 密钥（加密存储，接口只返回是否已填写）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelSecrets {
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub verification_token: String,
    #[serde(default)]
    pub encrypt_key: String,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: String,
    pub owner_id: String,
    pub platform: String,
    pub name: String,
    pub config: ChannelConfig,
    pub secrets: ChannelSecrets,
    /// 密钥无法解密（更换过服务端密钥）
    pub secrets_broken: bool,
    pub enabled: bool,
    pub created_at: String,
}

impl Channel {
    /// 返回给前端：密钥只给是否已填写
    pub fn public_json(&self) -> Value {
        json!({
            "id": self.id,
            "platform": self.platform,
            "name": self.name,
            "config": self.config,
            "has_app_secret": !self.secrets.app_secret.is_empty(),
            "has_verification_token": !self.secrets.verification_token.is_empty(),
            "has_encrypt_key": !self.secrets.encrypt_key.is_empty(),
            "secrets_broken": self.secrets_broken,
            "enabled": self.enabled,
            "created_at": self.created_at,
            "status": manager::status(&self.id),
        })
    }
}

/// 新建 / 修改渠道的输入；密钥留空表示不修改
#[derive(Debug, Clone, Deserialize)]
pub struct ChannelInput {
    #[serde(default)]
    pub platform: Option<String>,
    pub name: String,
    pub config: ChannelConfig,
    #[serde(default)]
    pub app_secret: Option<String>,
    #[serde(default)]
    pub verification_token: Option<String>,
    #[serde(default)]
    pub encrypt_key: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

const CHANNEL_COLUMNS: &str = "id, owner_id, platform, name, config, secrets, enabled, created_at";

fn row_to_channel(row: &sqlx::any::AnyRow) -> AppResult<Channel> {
    let sealed = text_column(row, 5)?.unwrap_or_default();
    let opened = crate::secrets::open(&sealed);
    Ok(Channel {
        id: row.try_get(0)?,
        owner_id: row.try_get(1)?,
        platform: text_column(row, 2)?.unwrap_or_default(),
        name: text_column(row, 3)?.unwrap_or_default(),
        config: text_column(row, 4)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        secrets_broken: opened.is_none() && !sealed.is_empty(),
        secrets: opened.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        enabled: row.try_get::<i64, _>(6)? != 0,
        created_at: text_column(row, 7)?.unwrap_or_default(),
    })
}

fn validate(input: &ChannelInput, secrets: &ChannelSecrets) -> Result<(), String> {
    if input.name.trim().is_empty() || input.name.chars().count() > 50 {
        return Err("名称不能为空，最多 50 个字".into());
    }
    if input.config.app_id.trim().is_empty() {
        return Err("请填写 App ID".into());
    }
    if secrets.app_secret.trim().is_empty() {
        return Err("请填写 App Secret".into());
    }
    if !matches!(input.config.domain.as_str(), "feishu" | "lark") {
        return Err("domain 只能是 feishu 或 lark".into());
    }
    if !matches!(input.config.connection.as_str(), "ws" | "webhook") {
        return Err("连接方式只能是 ws（长连接）或 webhook（事件回调）".into());
    }
    if input.config.connection == "webhook" && secrets.verification_token.trim().is_empty() {
        return Err("事件回调方式需要填写 Verification Token".into());
    }
    if crate::schedules::parse("0 9 * * *", &input.config.timezone).is_err() {
        return Err(format!("未知时区：{}", input.config.timezone));
    }
    Ok(())
}

fn merge_secrets(mut secrets: ChannelSecrets, input: &ChannelInput) -> ChannelSecrets {
    let pick = |value: &Option<String>, current: &mut String| {
        if let Some(v) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            *current = v.to_string();
        }
    };
    pick(&input.app_secret, &mut secrets.app_secret);
    pick(&input.verification_token, &mut secrets.verification_token);
    pick(&input.encrypt_key, &mut secrets.encrypt_key);
    secrets
}

pub async fn list_channels(pool: &DbPool, owner_id: &str) -> AppResult<Vec<Channel>> {
    let rows = sqlx::query(&format!("SELECT {CHANNEL_COLUMNS} FROM im_channels WHERE owner_id = ? ORDER BY created_at"))
        .bind(owner_id)
        .fetch_all(pool)
        .await?;
    rows.iter().map(row_to_channel).collect()
}

/// 所有启用的渠道（服务启动时建立连接）
pub async fn enabled_channels(pool: &DbPool) -> AppResult<Vec<Channel>> {
    let rows = sqlx::query(&format!("SELECT {CHANNEL_COLUMNS} FROM im_channels WHERE enabled = 1"))
        .fetch_all(pool)
        .await?;
    rows.iter().map(row_to_channel).collect()
}

/// 不校验归属：用于事件回调与内部调度
pub async fn find_channel(pool: &DbPool, id: &str) -> AppResult<Option<Channel>> {
    let row = sqlx::query(&format!("SELECT {CHANNEL_COLUMNS} FROM im_channels WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(row_to_channel).transpose()
}

pub async fn get_channel(pool: &DbPool, owner_id: &str, id: &str) -> AppResult<Channel> {
    find_channel(pool, id)
        .await?
        .filter(|c| c.owner_id == owner_id)
        .ok_or_else(|| AppError::NotFound("渠道不存在".into()))
}

pub async fn create_channel(pool: &DbPool, owner_id: &str, input: &ChannelInput) -> AppResult<Channel> {
    let count: i64 = sqlx::query("SELECT COUNT(*) FROM im_channels WHERE owner_id = ?")
        .bind(owner_id)
        .fetch_one(pool)
        .await?
        .try_get(0)?;
    if count >= MAX_CHANNELS_PER_USER {
        return Err(bad(format!("最多创建 {MAX_CHANNELS_PER_USER} 个渠道")));
    }
    let platform = input.platform.clone().unwrap_or_else(|| "feishu".into());
    if platform != "feishu" {
        return Err(bad("目前只支持飞书，企业微信即将支持"));
    }
    let secrets = merge_secrets(ChannelSecrets::default(), input);
    validate(input, &secrets).map_err(bad)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO im_channels (id, owner_id, platform, name, config, secrets, enabled, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(owner_id)
    .bind(&platform)
    .bind(input.name.trim())
    .bind(serde_json::to_string(&input.config)?)
    .bind(crate::secrets::seal(&serde_json::to_string(&secrets)?))
    .bind(input.enabled.unwrap_or(true) as i64)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    get_channel(pool, owner_id, &id).await
}

pub async fn update_channel(pool: &DbPool, owner_id: &str, id: &str, input: &ChannelInput) -> AppResult<Channel> {
    let current = get_channel(pool, owner_id, id).await?;
    let base = if current.secrets_broken { ChannelSecrets::default() } else { current.secrets.clone() };
    let secrets = merge_secrets(base, input);
    validate(input, &secrets).map_err(bad)?;
    sqlx::query("UPDATE im_channels SET name = ?, config = ?, secrets = ?, enabled = ?, updated_at = ? WHERE id = ? AND owner_id = ?")
        .bind(input.name.trim())
        .bind(serde_json::to_string(&input.config)?)
        .bind(crate::secrets::seal(&serde_json::to_string(&secrets)?))
        .bind(input.enabled.unwrap_or(current.enabled) as i64)
        .bind(Utc::now().to_rfc3339())
        .bind(id)
        .bind(owner_id)
        .execute(pool)
        .await?;
    get_channel(pool, owner_id, id).await
}

pub async fn delete_channel(pool: &DbPool, owner_id: &str, id: &str) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM im_channels WHERE id = ? AND owner_id = ?")
        .bind(id)
        .bind(owner_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    for table in ["im_bindings", "im_conversations", "im_events", "im_chats", "im_schedule_targets"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE channel_id = ?")).bind(id).execute(pool).await?;
    }
    Ok(true)
}

// ───────────────────────── 绑定 ─────────────────────────

struct PendingCode {
    channel_id: String,
    expires: Instant,
}

static BIND_CODES: Lazy<Mutex<HashMap<String, PendingCode>>> = Lazy::new(Default::default);

/// 为渠道生成一个 6 位绑定码（同一渠道之前的码作废）
pub fn issue_bind_code(channel_id: &str) -> (String, u64) {
    let mut codes = BIND_CODES.lock().unwrap();
    let now = Instant::now();
    codes.retain(|_, p| p.expires > now && p.channel_id != channel_id);
    let code = loop {
        let n = u32::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap()) % 1_000_000;
        let code = format!("{n:06}");
        if !codes.contains_key(&code) {
            break code;
        }
    };
    codes.insert(code.clone(), PendingCode { channel_id: channel_id.to_string(), expires: now + BIND_CODE_TTL });
    (code, BIND_CODE_TTL.as_secs())
}

/// 校验并消耗绑定码：只对生成它的渠道有效
pub fn consume_bind_code(channel_id: &str, code: &str) -> bool {
    let mut codes = BIND_CODES.lock().unwrap();
    match codes.get(code) {
        Some(p) if p.channel_id == channel_id && p.expires > Instant::now() => {
            codes.remove(code);
            true
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Binding {
    pub id: String,
    pub im_user_id: String,
    pub display_name: Option<String>,
    pub created_at: String,
}

pub async fn is_bound(pool: &DbPool, channel_id: &str, im_user_id: &str) -> AppResult<bool> {
    let row = sqlx::query("SELECT id FROM im_bindings WHERE channel_id = ? AND im_user_id = ?")
        .bind(channel_id)
        .bind(im_user_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

pub async fn add_binding(pool: &DbPool, channel_id: &str, im_user_id: &str, display_name: Option<&str>) -> AppResult<()> {
    if is_bound(pool, channel_id, im_user_id).await? {
        return Ok(());
    }
    sqlx::query("INSERT INTO im_bindings (id, channel_id, im_user_id, display_name, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(channel_id)
        .bind(im_user_id)
        .bind(display_name)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_bindings(pool: &DbPool, channel_id: &str) -> AppResult<Vec<Binding>> {
    let rows = sqlx::query("SELECT id, im_user_id, display_name, created_at FROM im_bindings WHERE channel_id = ? ORDER BY created_at")
        .bind(channel_id)
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|row| {
            Ok(Binding {
                id: row.try_get(0)?,
                im_user_id: text_column(row, 1)?.unwrap_or_default(),
                display_name: text_column(row, 2)?,
                created_at: text_column(row, 3)?.unwrap_or_default(),
            })
        })
        .collect()
}

pub async fn remove_binding(pool: &DbPool, channel_id: &str, binding_id: &str) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM im_bindings WHERE id = ? AND channel_id = ?")
        .bind(binding_id)
        .bind(channel_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

// ───────────────────────── IM 会话 ─────────────────────────

/// 一个 IM 会话（私聊 / 群）当前对应的系统会话与模式
#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: String,
    pub session_id: Option<String>,
    pub mode: AgentMode,
    pub pending_run_id: Option<String>,
}

pub async fn conversation(pool: &DbPool, channel: &Channel, chat_id: &str) -> AppResult<Conversation> {
    let row = sqlx::query("SELECT id, session_id, mode, pending_run_id FROM im_conversations WHERE channel_id = ? AND chat_id = ?")
        .bind(&channel.id)
        .bind(chat_id)
        .fetch_optional(pool)
        .await?;
    if let Some(row) = row {
        return Ok(Conversation {
            id: row.try_get(0)?,
            session_id: text_column(&row, 1)?,
            mode: serde_json::from_value(json!(text_column(&row, 2)?.unwrap_or_default())).unwrap_or(channel.config.default_mode),
            pending_run_id: text_column(&row, 3)?,
        });
    }
    let conv = Conversation {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: None,
        mode: channel.config.default_mode,
        pending_run_id: None,
    };
    sqlx::query("INSERT INTO im_conversations (id, channel_id, chat_id, mode, updated_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&conv.id)
        .bind(&channel.id)
        .bind(chat_id)
        .bind(conv.mode.as_str())
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
    Ok(conv)
}

pub async fn save_conversation(pool: &DbPool, conv: &Conversation) -> AppResult<()> {
    sqlx::query("UPDATE im_conversations SET session_id = ?, mode = ?, pending_run_id = ?, updated_at = ? WHERE id = ?")
        .bind(&conv.session_id)
        .bind(conv.mode.as_str())
        .bind(&conv.pending_run_id)
        .bind(Utc::now().to_rfc3339())
        .bind(&conv.id)
        .execute(pool)
        .await?;
    Ok(())
}

// ───────────────────────── 事件去重 ─────────────────────────

/// 第一次见到这个事件时返回 true；平台重推的重复事件返回 false
pub async fn first_seen(pool: &DbPool, channel_id: &str, event_key: &str) -> bool {
    sqlx::query("INSERT INTO im_events (channel_id, event_key, created_at) VALUES (?, ?, ?)")
        .bind(channel_id)
        .bind(event_key)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .is_ok()
}

/// 清理两天前的去重记录
pub async fn prune_events(pool: &DbPool) {
    let cutoff = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
    let _ = sqlx::query("DELETE FROM im_events WHERE created_at < ?").bind(cutoff).execute(pool).await;
}

// ───────────────────────── 收到的消息 ─────────────────────────

/// 各平台统一后的入站消息
#[derive(Debug, Clone)]
pub struct Incoming {
    pub event_key: String,
    pub message_id: String,
    pub chat_id: String,
    /// p2p / group
    pub chat_type: String,
    pub sender_id: String,
    pub sender_name: Option<String>,
    /// 去掉 @ 占位后的文本
    pub text: String,
    /// 群聊里是否 @ 了机器人
    pub mentioned_bot: bool,
    /// 消息里的图片 / 文件（需要再下载）
    pub resources: Vec<Resource>,
    /// 不支持的消息类型（语音、表情等）
    pub unsupported: Option<String>,
}

/// 消息里附带的图片或文件
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    pub key: String,
    /// image / file
    pub kind: String,
    pub name: Option<String>,
}

// ───────────────────────── 会话信息与推送目标 ─────────────────────────

/// 推送目标：某个渠道里的某个会话
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImTarget {
    pub channel_id: String,
    pub chat_id: String,
}

/// 记录收到消息的会话（类型、群名），用于在界面上选择推送目标
pub async fn remember_chat(pool: &DbPool, channel_id: &str, chat_id: &str, chat_type: &str, name: Option<&str>) {
    let now = Utc::now().to_rfc3339();
    let updated = sqlx::query("UPDATE im_chats SET chat_type = ?, name = COALESCE(?, name), updated_at = ? WHERE channel_id = ? AND chat_id = ?")
        .bind(chat_type)
        .bind(name)
        .bind(&now)
        .bind(channel_id)
        .bind(chat_id)
        .execute(pool)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
    if updated == 0 {
        let _ = sqlx::query("INSERT INTO im_chats (channel_id, chat_id, chat_type, name, updated_at) VALUES (?, ?, ?, ?, ?)")
            .bind(channel_id)
            .bind(chat_id)
            .bind(chat_type)
            .bind(name)
            .bind(&now)
            .execute(pool)
            .await;
    }
}

pub async fn chat_known(pool: &DbPool, channel_id: &str, chat_id: &str) -> bool {
    sqlx::query("SELECT chat_id FROM im_chats WHERE channel_id = ? AND chat_id = ?")
        .bind(channel_id)
        .bind(chat_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .is_some()
}

fn chat_label(channel_name: &str, chat_id: &str, chat_type: &str, name: Option<String>) -> String {
    let tail: String = chat_id.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    match (chat_type, name.filter(|n| !n.trim().is_empty())) {
        ("group", Some(name)) => format!("{channel_name} · 群聊「{name}」"),
        ("group", None) => format!("{channel_name} · 群聊（…{tail}）"),
        _ => format!("{channel_name} · 私聊（…{tail}）"),
    }
}

/// 用户可选的推送目标：自己的渠道里收到过消息的会话
pub async fn targets(pool: &DbPool, owner_id: &str) -> AppResult<Vec<Value>> {
    let rows = sqlx::query(
        "SELECT c.id, c.name, t.chat_id, t.chat_type, t.name FROM im_chats t JOIN im_channels c ON c.id = t.channel_id
         WHERE c.owner_id = ? ORDER BY t.updated_at DESC",
    )
    .bind(owner_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            let channel_id: String = row.try_get(0)?;
            let channel_name = text_column(row, 1)?.unwrap_or_default();
            let chat_id = text_column(row, 2)?.unwrap_or_default();
            let chat_type = text_column(row, 3)?.unwrap_or_default();
            let label = chat_label(&channel_name, &chat_id, &chat_type, text_column(row, 4)?);
            Ok(json!({ "channel_id": channel_id, "chat_id": chat_id, "label": label }))
        })
        .collect()
}

/// 定时任务的推送目标（附带显示名称）
pub async fn schedule_target(pool: &DbPool, schedule_id: &str) -> Option<(ImTarget, String)> {
    let row = sqlx::query(
        "SELECT s.channel_id, s.chat_id, c.name, t.chat_type, t.name FROM im_schedule_targets s
         JOIN im_channels c ON c.id = s.channel_id
         LEFT JOIN im_chats t ON t.channel_id = s.channel_id AND t.chat_id = s.chat_id
         WHERE s.schedule_id = ?",
    )
    .bind(schedule_id)
    .fetch_optional(pool)
    .await
    .ok()??;
    let target = ImTarget { channel_id: row.try_get(0).ok()?, chat_id: text_column(&row, 1).ok()??.to_string() };
    let label = chat_label(
        &text_column(&row, 2).ok()?.unwrap_or_default(),
        &target.chat_id,
        &text_column(&row, 3).ok()?.unwrap_or_default(),
        text_column(&row, 4).ok()?,
    );
    Some((target, label))
}

/// 推送目标必须是该用户自己的渠道里收到过消息的会话
pub async fn validate_target(pool: &DbPool, owner_id: &str, target: Option<&ImTarget>) -> AppResult<()> {
    if let Some(target) = target {
        if find_channel(pool, &target.channel_id).await?.filter(|c| c.owner_id == owner_id).is_none() {
            return Err(bad("推送目标的渠道不存在"));
        }
        if !chat_known(pool, &target.channel_id, &target.chat_id).await {
            return Err(bad("推送目标不存在：需要先在这个飞书会话里给机器人发过消息"));
        }
    }
    Ok(())
}

/// 设置或清除定时任务的推送目标
pub async fn set_schedule_target(pool: &DbPool, owner_id: &str, schedule_id: &str, target: Option<&ImTarget>) -> AppResult<()> {
    validate_target(pool, owner_id, target).await?;
    sqlx::query("DELETE FROM im_schedule_targets WHERE schedule_id = ?").bind(schedule_id).execute(pool).await?;
    if let Some(target) = target {
        sqlx::query("INSERT INTO im_schedule_targets (schedule_id, channel_id, chat_id, created_at) VALUES (?, ?, ?, ?)")
            .bind(schedule_id)
            .bind(&target.channel_id)
            .bind(&target.chat_id)
            .bind(Utc::now().to_rfc3339())
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_codes_are_single_use_and_scoped() {
        let (code, ttl) = issue_bind_code("ch-a");
        assert_eq!(code.len(), 6);
        assert_eq!(ttl, 600);
        assert!(!consume_bind_code("ch-b", &code), "其他渠道不能用");
        assert!(consume_bind_code("ch-a", &code));
        assert!(!consume_bind_code("ch-a", &code), "只能用一次");
        // 重新生成会让旧码作废
        let (first, _) = issue_bind_code("ch-c");
        let (second, _) = issue_bind_code("ch-c");
        assert!(!consume_bind_code("ch-c", &first) || first == second);
        assert!(consume_bind_code("ch-c", &second));
    }

    #[test]
    fn chat_labels() {
        assert_eq!(chat_label("助手", "oc_abcdef12", "group", Some("项目群".into())), "助手 · 群聊「项目群」");
        assert_eq!(chat_label("助手", "oc_abcdef12", "group", None), "助手 · 群聊（…ef12）");
        assert_eq!(chat_label("助手", "oc_abcdef12", "p2p", Some("x".into())), "助手 · 私聊（…ef12）");
    }
}
