//! 飞书长连接：不需要公网地址，机器人主动连到飞书接收事件。
//!
//! 流程：用 App ID / Secret 换取 WebSocket 地址 → 连接 → 定时发 ping →
//! 收到数据帧（可能分片）后立即回 ACK（3 秒内不回飞书会重推），再交给 router 异步处理。
//! 帧是 protobuf（pbbp2.Frame，proto2 且字段 1-4 为 required），这里手写编解码，
//! 保证 SeqID / LogID 为 0 时也会写出，避免服务端校验 required 字段失败。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use super::{feishu, manager, Channel};

/// 控制帧（ping / pong）与数据帧（事件 / 卡片回调）
const METHOD_CONTROL: i32 = 0;
const METHOD_DATA: i32 = 1;
const DEFAULT_PING: Duration = Duration::from_secs(120);
/// 分片在这段时间内没收齐就丢弃
const FRAGMENT_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    pub seq_id: u64,
    pub log_id: u64,
    pub service: i32,
    pub method: i32,
    pub headers: Vec<(String, String)>,
    pub payload_encoding: Option<String>,
    pub payload_type: Option<String>,
    pub payload: Option<Vec<u8>>,
    pub log_id_new: Option<String>,
}

impl Frame {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint_field(&mut out, 1, self.seq_id);
        put_varint_field(&mut out, 2, self.log_id);
        // int32 负数按 64 位补码写
        put_varint_field(&mut out, 3, self.service as i64 as u64);
        put_varint_field(&mut out, 4, self.method as i64 as u64);
        for (k, v) in &self.headers {
            let mut header = Vec::new();
            put_bytes_field(&mut header, 1, k.as_bytes());
            put_bytes_field(&mut header, 2, v.as_bytes());
            put_bytes_field(&mut out, 5, &header);
        }
        if let Some(v) = &self.payload_encoding {
            put_bytes_field(&mut out, 6, v.as_bytes());
        }
        if let Some(v) = &self.payload_type {
            put_bytes_field(&mut out, 7, v.as_bytes());
        }
        if let Some(v) = &self.payload {
            put_bytes_field(&mut out, 8, v);
        }
        if let Some(v) = &self.log_id_new {
            put_bytes_field(&mut out, 9, v.as_bytes());
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut frame = Frame::default();
        let mut reader = Reader { buf, pos: 0 };
        while !reader.done() {
            let key = reader.varint()?;
            let (field, wire) = (key >> 3, key & 7);
            match (field, wire) {
                (1, 0) => frame.seq_id = reader.varint()?,
                (2, 0) => frame.log_id = reader.varint()?,
                (3, 0) => frame.service = reader.varint()? as i32,
                (4, 0) => frame.method = reader.varint()? as i32,
                (5, 2) => {
                    let bytes = reader.bytes()?;
                    let mut inner = Reader { buf: bytes, pos: 0 };
                    let (mut k, mut v) = (String::new(), String::new());
                    while !inner.done() {
                        let key = inner.varint()?;
                        match (key >> 3, key & 7) {
                            (1, 2) => k = String::from_utf8_lossy(inner.bytes()?).into_owned(),
                            (2, 2) => v = String::from_utf8_lossy(inner.bytes()?).into_owned(),
                            (_, w) => inner.skip(w)?,
                        }
                    }
                    frame.headers.push((k, v));
                }
                (6, 2) => frame.payload_encoding = Some(String::from_utf8_lossy(reader.bytes()?).into_owned()),
                (7, 2) => frame.payload_type = Some(String::from_utf8_lossy(reader.bytes()?).into_owned()),
                (8, 2) => frame.payload = Some(reader.bytes()?.to_vec()),
                (9, 2) => frame.log_id_new = Some(String::from_utf8_lossy(reader.bytes()?).into_owned()),
                (_, w) => reader.skip(w)?,
            }
        }
        Ok(frame)
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_varint_field(out: &mut Vec<u8>, field: u64, v: u64) {
    put_varint(out, field << 3);
    put_varint(out, v);
}

fn put_bytes_field(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos).ok_or_else(|| anyhow!("帧被截断"))?;
            self.pos += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        bail!("varint 过长")
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.varint()? as usize;
        let end = self.pos.checked_add(len).filter(|e| *e <= self.buf.len()).ok_or_else(|| anyhow!("帧被截断"))?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }
    fn skip(&mut self, wire: u64) -> Result<()> {
        match wire {
            0 => {
                self.varint()?;
            }
            1 => self.pos += 8,
            2 => {
                self.bytes()?;
            }
            5 => self.pos += 4,
            _ => bail!("不支持的 wire type {wire}"),
        }
        if self.pos > self.buf.len() {
            bail!("帧被截断");
        }
        Ok(())
    }
}

/// 分片重组：按 message_id 收齐 sum 个分片后按 seq 拼接
#[derive(Default)]
pub struct Fragments {
    parts: HashMap<String, (Vec<Option<Vec<u8>>>, Instant)>,
}

impl Fragments {
    pub fn push(&mut self, message_id: &str, sum: usize, seq: usize, payload: Vec<u8>) -> Option<Vec<u8>> {
        if sum <= 1 {
            return Some(payload);
        }
        let now = Instant::now();
        self.parts.retain(|_, (_, at)| now.duration_since(*at) < FRAGMENT_TTL);
        let entry = self.parts.entry(message_id.to_string()).or_insert_with(|| (vec![None; sum], now));
        if seq >= entry.0.len() {
            return None;
        }
        entry.0[seq] = Some(payload);
        if entry.0.iter().all(Option::is_some) {
            let (parts, _) = self.parts.remove(message_id)?;
            return Some(parts.into_iter().flatten().flatten().collect());
        }
        None
    }
}

/// 换取长连接地址，返回 (wss 地址, ping 间隔)
async fn endpoint(client: &feishu::Client) -> Result<(String, Duration)> {
    let resp: Value = reqwest::Client::new()
        .post(format!("{}/callback/ws/endpoint", client.base()))
        .header("locale", "zh")
        .json(&json!({ "AppID": client.app_id(), "AppSecret": client.app_secret() }))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .context("连接飞书失败")?
        .json()
        .await
        .context("飞书返回格式错误")?;
    let code = resp.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        let msg = resp.get("msg").and_then(Value::as_str).unwrap_or("未知错误");
        bail!("获取长连接地址失败（{code}）：{msg}。请确认应用已发布，并在「事件与回调」里选择了“使用长连接接收事件”");
    }
    let url = resp.pointer("/data/URL").and_then(Value::as_str).ok_or_else(|| anyhow!("飞书没有返回长连接地址"))?.to_string();
    let ping = resp.pointer("/data/ClientConfig/PingInterval").and_then(Value::as_u64).map(Duration::from_secs).unwrap_or(DEFAULT_PING);
    Ok((url, ping))
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// 一直保持连接，断线后指数退避重连，直到被取消
pub async fn run(channel: Channel, cancel: CancellationToken) {
    let mut failures: u32 = 0;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        manager::set_state(&channel.id, "connecting", None);
        let started = Instant::now();
        let result = connect_once(&channel, &cancel).await;
        if cancel.is_cancelled() {
            break;
        }
        // 连上并稳定运行过一段时间，退避从头算
        if started.elapsed() > Duration::from_secs(60) {
            failures = 0;
        }
        failures += 1;
        let wait = Duration::from_secs(2u64.saturating_pow(failures.min(6)).min(60));
        let error = match result {
            Ok(()) => "连接被飞书关闭，正在重连".to_string(),
            Err(e) => format!("{e:#}"),
        };
        tracing::warn!("[IM] 飞书长连接断开（渠道 {}）：{error}，{}s 后重连", channel.id, wait.as_secs());
        manager::set_state(&channel.id, "error", Some(error));
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(wait) => {}
        }
    }
    // 被取消时不改状态：manager 已经按新配置设置好了（停用 / 事件回调 / 重新连接）
}

async fn connect_once(channel: &Channel, cancel: &CancellationToken) -> Result<()> {
    let client = feishu::Client::new(channel);
    let (url, mut ping_every) = endpoint(&client).await?;
    let service: i32 = query_param(&url, "service_id").and_then(|s| s.parse().ok()).unwrap_or(0);
    let (ws, _) = tokio_tungstenite::connect_async(url.as_str()).await.context("WebSocket 连接失败")?;
    let (mut write, mut read) = ws.split();

    // 机器人 open_id 用来判断群聊里是否 @ 了它；拿不到也不影响收消息
    match client.bot_info().await {
        Ok((name, open_id)) => manager::set_bot(&channel.id, &name, &open_id),
        Err(e) => tracing::warn!("[IM] 获取机器人信息失败：{e:#}"),
    }
    manager::set_state(&channel.id, "connected", None);
    tracing::info!("[IM] 飞书长连接已建立（渠道 {}）", channel.id);

    let mut fragments = Fragments::default();
    let mut ticker = tokio::time::interval(ping_every);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = write.send(Message::Close(None)).await;
                return Ok(());
            }
            _ = ticker.tick() => {
                let ping = Frame { service, method: METHOD_CONTROL, headers: vec![("type".into(), "ping".into())], ..Default::default() };
                write.send(Message::Binary(ping.encode())).await.context("发送 ping 失败")?;
            }
            message = read.next() => {
                let Some(message) = message else { return Ok(()) };
                let data = match message.context("读取长连接失败")? {
                    Message::Binary(data) => data,
                    Message::Close(_) => return Ok(()),
                    _ => continue,
                };
                let frame = match Frame::decode(&data) {
                    Ok(frame) => frame,
                    Err(e) => {
                        tracing::warn!("[IM] 无法解析飞书帧：{e}");
                        continue;
                    }
                };
                if frame.method == METHOD_CONTROL {
                    // pong 里可能带新的 ping 间隔
                    if frame.header("type") == Some("pong") {
                        let interval = frame.payload.as_deref()
                            .and_then(|p| serde_json::from_slice::<Value>(p).ok())
                            .and_then(|v| v.get("PingInterval").and_then(Value::as_u64))
                            .map(Duration::from_secs);
                        if let Some(interval) = interval.filter(|i| *i != ping_every && !i.is_zero()) {
                            ping_every = interval;
                            ticker = tokio::time::interval(ping_every);
                            ticker.tick().await;
                        }
                    }
                    continue;
                }
                if frame.method != METHOD_DATA {
                    continue;
                }
                let started = Instant::now();
                let sum: usize = frame.header("sum").and_then(|s| s.parse().ok()).unwrap_or(1);
                let seq: usize = frame.header("seq").and_then(|s| s.parse().ok()).unwrap_or(0);
                let message_id = frame.header("message_id").unwrap_or_default().to_string();
                let kind = frame.header("type").unwrap_or_default().to_string();
                let Some(payload) = fragments.push(&message_id, sum, seq, frame.payload.clone().unwrap_or_default()) else {
                    continue;
                };
                let event = serde_json::from_slice::<Value>(&payload);
                // 卡片回调的 ACK 里带一个提示（data 为响应 JSON 的 base64），其他事件不带内容
                let data = match &event {
                    Ok(e) if feishu::event_type(e) == "card.action.trigger" => {
                        json!(base64::engine::general_purpose::STANDARD.encode(feishu::card_toast("已收到，正在处理").to_string()))
                    }
                    _ => Value::Null,
                };
                // 先回 ACK，处理放到后台
                let mut ack = frame.clone();
                ack.headers.push(("biz_rt".into(), started.elapsed().as_millis().to_string()));
                ack.payload = Some(serde_json::to_vec(&json!({ "code": 200, "headers": null, "data": data }))?);
                write.send(Message::Binary(ack.encode())).await.context("发送 ACK 失败")?;
                if kind == "event" || kind == "card" {
                    match event {
                        Ok(event) => {
                            let channel_id = channel.id.clone();
                            tokio::spawn(super::router::handle_event(channel_id, event));
                        }
                        Err(e) => tracing::warn!("[IM] 事件不是 JSON：{e}"),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_keeps_zero_required_fields() {
        let ping = Frame { service: 33, method: METHOD_CONTROL, headers: vec![("type".into(), "ping".into())], ..Default::default() };
        let bytes = ping.encode();
        // 字段 1、2 即使为 0 也要写出（tag 0x08 / 0x10）
        assert_eq!(&bytes[..4], &[0x08, 0x00, 0x10, 0x00]);
        assert_eq!(Frame::decode(&bytes).unwrap(), ping);

        let data = Frame {
            seq_id: 300,
            log_id: u64::MAX,
            service: -1,
            method: METHOD_DATA,
            headers: vec![("type".into(), "event".into()), ("message_id".into(), "m1".into())],
            payload: Some(br#"{"a":1}"#.to_vec()),
            log_id_new: Some("x".into()),
            ..Default::default()
        };
        let decoded = Frame::decode(&data.encode()).unwrap();
        assert_eq!(decoded, data);
        assert_eq!(decoded.header("message_id"), Some("m1"));
    }

    #[test]
    fn decode_rejects_truncated() {
        let bytes = Frame { payload: Some(vec![1; 10]), ..Default::default() }.encode();
        assert!(Frame::decode(&bytes[..bytes.len() - 3]).is_err());
    }

    #[test]
    fn fragments_reassemble_in_order() {
        let mut f = Fragments::default();
        assert_eq!(f.push("x", 1, 0, b"whole".to_vec()), Some(b"whole".to_vec()));
        assert_eq!(f.push("m", 3, 2, b"c".to_vec()), None);
        assert_eq!(f.push("m", 3, 0, b"a".to_vec()), None);
        assert_eq!(f.push("m", 3, 1, b"b".to_vec()), Some(b"abc".to_vec()));
        assert_eq!(f.push("m", 3, 9, b"z".to_vec()), None);
    }

    #[test]
    fn reads_service_id() {
        assert_eq!(query_param("wss://x/ws?device_id=1&service_id=42", "service_id").as_deref(), Some("42"));
        assert_eq!(query_param("wss://x/ws", "service_id"), None);
    }
}
