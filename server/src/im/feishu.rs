//! 飞书开放平台：access token、发消息 / 回复 / 更新卡片、上传文件，以及事件回调的解密与解析。
//!
//! 接口地址默认 open.feishu.cn（国际版 Lark 为 open.larksuite.com），
//! 可用环境变量 FEISHU_API_BASE 指向本地模拟服务做测试。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aes::cipher::{BlockDecryptMut, KeyIvInit};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Channel, Incoming};

/// 单个文件上限（飞书限制 30MB）
const MAX_FILE_BYTES: usize = 30 * 1024 * 1024;
/// 图片上限（飞书限制 10MB）
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

pub fn api_base(domain: &str) -> String {
    if let Ok(base) = std::env::var("FEISHU_API_BASE") {
        if !base.trim().is_empty() {
            return base.trim_end_matches('/').to_string();
        }
    }
    match domain {
        "lark" => "https://open.larksuite.com".into(),
        _ => "https://open.feishu.cn".into(),
    }
}

fn http() -> &'static reqwest::Client {
    static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("http client")
    });
    &CLIENT
}

/// tenant_access_token 缓存：key 为 base + app_id
static TOKENS: Lazy<Mutex<HashMap<String, (String, Instant)>>> = Lazy::new(Default::default);

#[derive(Clone)]
pub struct Client {
    base: String,
    app_id: String,
    app_secret: String,
}

/// 检查飞书返回：HTTP 成功且 code == 0
fn check(value: Value, what: &str) -> Result<Value> {
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        let msg = value.get("msg").and_then(Value::as_str).unwrap_or("未知错误");
        bail!("{what}失败（{code}）：{msg}");
    }
    Ok(value)
}

impl Client {
    pub fn new(channel: &Channel) -> Self {
        Self {
            base: api_base(&channel.config.domain),
            app_id: channel.config.app_id.trim().to_string(),
            app_secret: channel.secrets.app_secret.trim().to_string(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    pub fn app_secret(&self) -> &str {
        &self.app_secret
    }

    pub async fn token(&self) -> Result<String> {
        let key = format!("{}|{}", self.base, self.app_id);
        if let Some((token, expires)) = TOKENS.lock().unwrap().get(&key) {
            if *expires > Instant::now() {
                return Ok(token.clone());
            }
        }
        let resp: Value = http()
            .post(format!("{}/open-apis/auth/v3/tenant_access_token/internal", self.base))
            .json(&json!({ "app_id": self.app_id, "app_secret": self.app_secret }))
            .send()
            .await
            .context("连接飞书失败")?
            .json()
            .await
            .context("飞书返回格式错误")?;
        let resp = check(resp, "获取 access token ")?;
        let token = resp
            .get("tenant_access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("飞书没有返回 tenant_access_token"))?
            .to_string();
        // 提前 5 分钟过期，避免用到快失效的 token
        let ttl = resp.get("expire").and_then(Value::as_u64).unwrap_or(7200).saturating_sub(300).max(60);
        TOKENS.lock().unwrap().insert(key, (token.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(token)
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>, what: &str) -> Result<Value> {
        let token = self.token().await?;
        let mut req = http().request(method, format!("{}{}", self.base, path)).bearer_auth(token);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp: Value = req.send().await.with_context(|| format!("{what}：连接飞书失败"))?.json().await.with_context(|| format!("{what}：返回格式错误"))?;
        check(resp, what)
    }

    /// 机器人信息（名称、open_id），用于测试连接与识别群聊里的 @
    pub async fn bot_info(&self) -> Result<(String, String)> {
        let resp = self.call(reqwest::Method::GET, "/open-apis/bot/v3/info", None, "获取机器人信息").await?;
        let bot = resp.get("bot").cloned().unwrap_or_default();
        Ok((
            bot.get("app_name").and_then(Value::as_str).unwrap_or("飞书机器人").to_string(),
            bot.get("open_id").and_then(Value::as_str).unwrap_or_default().to_string(),
        ))
    }

    /// 回复某条消息，返回新消息的 message_id
    pub async fn reply(&self, message_id: &str, msg_type: &str, content: &Value) -> Result<String> {
        let resp = self
            .call(
                reqwest::Method::POST,
                &format!("/open-apis/im/v1/messages/{message_id}/reply"),
                Some(json!({ "msg_type": msg_type, "content": content.to_string() })),
                "回复消息",
            )
            .await?;
        Ok(resp.pointer("/data/message_id").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    /// 主动发到某个会话（chat_id）；C2-b 的定时任务结果推送会用到
    #[allow(dead_code)]
    pub async fn send(&self, chat_id: &str, msg_type: &str, content: &Value) -> Result<String> {
        let resp = self
            .call(
                reqwest::Method::POST,
                "/open-apis/im/v1/messages?receive_id_type=chat_id",
                Some(json!({ "receive_id": chat_id, "msg_type": msg_type, "content": content.to_string() })),
                "发送消息",
            )
            .await?;
        Ok(resp.pointer("/data/message_id").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    /// 原地更新机器人发出的卡片
    pub async fn update_card(&self, message_id: &str, card: &Value) -> Result<()> {
        self.call(
            reqwest::Method::PATCH,
            &format!("/open-apis/im/v1/messages/{message_id}"),
            Some(json!({ "content": card.to_string() })),
            "更新卡片",
        )
        .await?;
        Ok(())
    }

    /// 上传文件或图片，返回 (msg_type, content)，可直接用于发送
    pub async fn upload(&self, name: &str, mime: &str, bytes: Vec<u8>) -> Result<(String, Value)> {
        let token = self.token().await?;
        let is_image = mime.starts_with("image/") && !mime.contains("svg");
        if is_image && bytes.len() <= MAX_IMAGE_BYTES {
            let form = reqwest::multipart::Form::new()
                .text("image_type", "message")
                .part("image", reqwest::multipart::Part::bytes(bytes).file_name(name.to_string()).mime_str(mime)?);
            let resp: Value = http()
                .post(format!("{}/open-apis/im/v1/images", self.base))
                .bearer_auth(token)
                .multipart(form)
                .send()
                .await?
                .json()
                .await?;
            let resp = check(resp, "上传图片")?;
            let key = resp.pointer("/data/image_key").and_then(Value::as_str).unwrap_or_default();
            return Ok(("image".into(), json!({ "image_key": key })));
        }
        if bytes.len() > MAX_FILE_BYTES {
            bail!("文件超过 30MB，请在网页上下载");
        }
        let form = reqwest::multipart::Form::new()
            .text("file_type", file_type(name))
            .text("file_name", name.to_string())
            .part("file", reqwest::multipart::Part::bytes(bytes).file_name(name.to_string()).mime_str(mime)?);
        let resp: Value = http()
            .post(format!("{}/open-apis/im/v1/files", self.base))
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await?
            .json()
            .await?;
        let resp = check(resp, "上传文件")?;
        let key = resp.pointer("/data/file_key").and_then(Value::as_str).unwrap_or_default();
        Ok(("file".into(), json!({ "file_key": key })))
    }
}

/// 飞书上传接口的文件类型
fn file_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "pdf",
        "doc" | "docx" => "doc",
        "xls" | "xlsx" | "csv" => "xls",
        "ppt" | "pptx" => "ppt",
        "mp4" => "mp4",
        "opus" => "opus",
        _ => "stream",
    }
}

// ───────────────────────── 事件回调 ─────────────────────────

/// 解密事件：key = SHA256(encrypt_key)，密文前 16 字节是 IV，AES-256-CBC + PKCS7
pub fn decrypt(encrypt_key: &str, encrypted: &str) -> Result<String> {
    let raw = base64::engine::general_purpose::STANDARD.decode(encrypted.trim()).context("密文不是 base64")?;
    if raw.len() < 32 || raw.len() % 16 != 0 {
        bail!("密文长度不对");
    }
    let key = Sha256::digest(encrypt_key.as_bytes());
    let (iv, data) = raw.split_at(16);
    let mut buf = data.to_vec();
    let plain = cbc::Decryptor::<aes::Aes256>::new_from_slices(&key, iv)
        .map_err(|_| anyhow!("密钥长度不对"))?
        .decrypt_padded_mut::<aes::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|_| anyhow!("解密失败，请检查 Encrypt Key"))?;
    Ok(String::from_utf8(plain.to_vec())?)
}

/// 回调签名：SHA256(timestamp + nonce + encrypt_key + body) 的十六进制
pub fn signature(timestamp: &str, nonce: &str, encrypt_key: &str, body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(nonce.as_bytes());
    hasher.update(encrypt_key.as_bytes());
    hasher.update(body);
    hex::encode(hasher.finalize())
}

/// 解析 v2 事件里的收消息事件（im.message.receive_v1）；其他事件返回 None
pub fn parse_message_event(event: &Value, bot_open_id: Option<&str>) -> Option<Incoming> {
    if event.pointer("/header/event_type").and_then(Value::as_str) != Some("im.message.receive_v1") {
        return None;
    }
    let message = event.pointer("/event/message")?;
    let sender = event.pointer("/event/sender")?;
    if sender.get("sender_type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let message_id = message.get("message_id")?.as_str()?.to_string();
    let event_key = event
        .pointer("/header/event_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| message_id.clone());
    let msg_type = message.get("message_type").and_then(Value::as_str).unwrap_or_default();
    let content: Value = message
        .get("content")
        .and_then(Value::as_str)
        .and_then(|c| serde_json::from_str(c).ok())
        .unwrap_or_default();
    let mentions = message.get("mentions").and_then(Value::as_array).cloned().unwrap_or_default();
    let mentioned_bot = mentions.iter().any(|m| match bot_open_id {
        Some(bot) if !bot.is_empty() => m.pointer("/id/open_id").and_then(Value::as_str) == Some(bot),
        // 不知道机器人 open_id 时：没有“接收群聊所有消息”权限的应用只会收到 @ 它的群消息
        _ => true,
    });
    let (text, unsupported) = match msg_type {
        "text" => (content.get("text").and_then(Value::as_str).unwrap_or_default().to_string(), None),
        "post" => (post_text(&content), None),
        other => (String::new(), Some(other.to_string())),
    };
    // 去掉 @ 占位（@_user_1），只保留正文
    let mut text = text;
    for m in &mentions {
        if let Some(key) = m.get("key").and_then(Value::as_str) {
            text = text.replace(key, "");
        }
    }
    Some(Incoming {
        event_key,
        message_id,
        chat_id: message.get("chat_id").and_then(Value::as_str).unwrap_or_default().to_string(),
        chat_type: message.get("chat_type").and_then(Value::as_str).unwrap_or("p2p").to_string(),
        sender_id: sender.pointer("/sender_id/open_id").and_then(Value::as_str).unwrap_or_default().to_string(),
        sender_name: None,
        text: text.trim().to_string(),
        mentioned_bot,
        unsupported,
    })
}

/// 富文本（post）消息取出纯文本
fn post_text(content: &Value) -> String {
    let body = content
        .get("content")
        .or_else(|| content.as_object().and_then(|o| o.values().find_map(|v| v.get("content"))))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines = Vec::new();
    if let Some(title) = content.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()) {
        lines.push(title.to_string());
    }
    for paragraph in body {
        let line: String = paragraph
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match item.get("tag").and_then(Value::as_str) {
                        Some("text") | Some("a") => item.get("text").and_then(Value::as_str).map(str::to_string),
                        Some("at") => item.get("user_id").and_then(Value::as_str).map(str::to_string),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        lines.push(line);
    }
    lines.join("\n")
}

// ───────────────────────── 卡片 ─────────────────────────

/// 卡片模板颜色
pub enum Tone {
    Running,
    Success,
    Warning,
    Failed,
    Neutral,
}

/// 生成一张消息卡片：标题、Markdown 正文、底部备注、可选的链接按钮
pub fn card(title: &str, tone: Tone, body: &str, note: Option<&str>, link: Option<(&str, &str)>) -> Value {
    let template = match tone {
        Tone::Running => "blue",
        Tone::Success => "green",
        Tone::Warning => "orange",
        Tone::Failed => "red",
        Tone::Neutral => "grey",
    };
    let mut elements = vec![json!({ "tag": "markdown", "content": truncate(body, 6000) })];
    if let Some((label, url)) = link {
        elements.push(json!({
            "tag": "action",
            "actions": [{ "tag": "button", "text": { "tag": "plain_text", "content": label }, "type": "default", "url": url }]
        }));
    }
    if let Some(note) = note.filter(|n| !n.is_empty()) {
        elements.push(json!({ "tag": "note", "elements": [{ "tag": "plain_text", "content": truncate(note, 300) }] }));
    }
    json!({
        "config": { "wide_screen_mode": true, "update_multi": true },
        "header": { "title": { "tag": "plain_text", "content": truncate(title, 60) }, "template": template },
        "elements": elements,
    })
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("…");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;

    fn encrypt(key: &str, iv: [u8; 16], plain: &str) -> String {
        let key = Sha256::digest(key.as_bytes());
        let cipher = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key, &iv).unwrap();
        let mut out = iv.to_vec();
        out.extend(cipher.encrypt_padded_vec_mut::<aes::cipher::block_padding::Pkcs7>(plain.as_bytes()));
        base64::engine::general_purpose::STANDARD.encode(out)
    }

    #[test]
    fn decrypts_official_example() {
        // 飞书文档的示例：encrypt_key = "test key"，明文 "hello world"
        assert_eq!(decrypt("test key", "P37w+VZImNgPEO1RBhJ6RtKl7n6zymIbEG1pReEzghk=").unwrap(), "hello world");
    }

    #[test]
    fn decrypt_round_trip_and_wrong_key() {
        let body = r#"{"schema":"2.0","header":{"event_type":"im.message.receive_v1"}}"#;
        let sealed = encrypt("my-key", [7u8; 16], body);
        assert_eq!(decrypt("my-key", &sealed).unwrap(), body);
        assert!(decrypt("other-key", &sealed).is_err());
    }

    #[test]
    fn parses_group_text_with_mention() {
        let event = json!({
            "schema": "2.0",
            "header": { "event_id": "ev1", "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" }, "sender_type": "user" },
                "message": {
                    "message_id": "om_1", "chat_id": "oc_1", "chat_type": "group", "message_type": "text",
                    "content": "{\"text\":\"@_user_1 帮我写周报\"}",
                    "mentions": [{ "key": "@_user_1", "id": { "open_id": "ou_bot" }, "name": "助手" }]
                }
            }
        });
        let msg = parse_message_event(&event, Some("ou_bot")).unwrap();
        assert_eq!(msg.text, "帮我写周报");
        assert_eq!(msg.event_key, "ev1");
        assert!(msg.mentioned_bot);
        assert_eq!(msg.chat_type, "group");
        assert_eq!(msg.sender_id, "ou_user");
        // @ 的是别人
        assert!(!parse_message_event(&event, Some("ou_other_bot")).unwrap().mentioned_bot);
    }

    #[test]
    fn parses_post_and_marks_unsupported() {
        let event = |msg_type: &str, content: Value| {
            json!({
                "header": { "event_id": "e", "event_type": "im.message.receive_v1" },
                "event": {
                    "sender": { "sender_id": { "open_id": "ou" }, "sender_type": "user" },
                    "message": { "message_id": "m", "chat_id": "c", "chat_type": "p2p", "message_type": msg_type, "content": content.to_string() }
                }
            })
        };
        let post = json!({ "title": "需求", "content": [[{ "tag": "text", "text": "做一份" }, { "tag": "text", "text": "PPT" }]] });
        assert_eq!(parse_message_event(&event("post", post), None).unwrap().text, "需求\n做一份PPT");
        let image = parse_message_event(&event("image", json!({ "image_key": "k" })), None).unwrap();
        assert_eq!(image.unsupported.as_deref(), Some("image"));
        // 机器人自己发的消息忽略
        let mut bot = event("text", json!({ "text": "hi" }));
        bot["event"]["sender"]["sender_type"] = json!("app");
        assert!(parse_message_event(&bot, None).is_none());
    }

    #[test]
    fn signature_is_hex_sha256() {
        let sig = signature("1", "n", "k", b"{}");
        assert_eq!(sig.len(), 64);
        assert_eq!(sig, hex::encode(Sha256::digest(b"1nk{}")));
    }
}
