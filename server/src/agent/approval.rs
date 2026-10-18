//! 工具调用前确认：执行可能修改数据的插件工具之前，运行原地暂停，等用户允许或拒绝。
//!
//! - 发出 `tool_approval` 事件（网页时间线显示确认卡片，飞书发确认卡片），处理后发 `tool_approval_resolved`；
//! - 10 分钟没处理按拒绝算；用户停止运行时立即结束等待；
//! - “本次对话都允许”只记在内存里，按（会话，工具）区分；
//! - 定时任务没人在场，需要确认的调用直接拒绝。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::tool::ToolContext;

/// 等待确认的最长时间
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

/// 工具声明的确认信息
#[derive(Debug, Clone)]
pub struct ApprovalSpec {
    /// 来源（插件名）
    pub source: String,
    /// 给人看的工具名
    pub label: String,
    /// 为什么需要确认
    pub reason: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    /// 拒绝，附带告诉模型的原因
    Denied(String),
}

struct Pending {
    owner_id: String,
    session_id: String,
    tool: String,
    tx: oneshot::Sender<Decision>,
}

static PENDING: Lazy<Mutex<HashMap<String, Pending>>> = Lazy::new(Default::default);
static REMEMBERED: Lazy<Mutex<HashSet<(String, String)>>> = Lazy::new(Default::default);

pub fn remembered(session_id: &str, tool: &str) -> bool {
    REMEMBERED.lock().unwrap().contains(&(session_id.to_string(), tool.to_string()))
}

/// 处理一次确认；返回错误说明（不存在、无权、已处理）
pub fn resolve(approval_id: &str, owner_id: &str, decision: Decision, remember: bool) -> Result<(), String> {
    let mut pending = PENDING.lock().unwrap();
    match pending.get(approval_id) {
        None => return Err("这个确认已经处理过或已过期".into()),
        Some(p) if p.owner_id != owner_id => return Err("无权处理这个确认".into()),
        _ => {}
    }
    let p = pending.remove(approval_id).expect("checked");
    if remember && decision == Decision::Approve {
        REMEMBERED.lock().unwrap().insert((p.session_id.clone(), p.tool.clone()));
    }
    let _ = p.tx.send(decision);
    Ok(())
}

/// 等待中的确认是否属于某个用户（飞书回调用来校验）
pub fn owner_of(approval_id: &str) -> Option<String> {
    PENDING.lock().unwrap().get(approval_id).map(|p| p.owner_id.clone())
}

/// 运行中调用：需要确认时发出事件并等待结果
pub async fn request(
    ctx: &ToolContext,
    call_id: &str,
    tool: &str,
    spec: &ApprovalSpec,
    input: &Value,
    cancel: &CancellationToken,
) -> Outcome {
    if remembered(&ctx.session_id, tool) {
        return Outcome::Approved;
    }
    if ctx.origin.as_deref() == Some("schedule") {
        return Outcome::Denied(format!(
            "定时任务运行时没有人确认，已拒绝调用 {}（{}）。如需在定时任务里使用，请在插件设置里把「{}」改为无需确认。",
            spec.label, spec.reason, spec.source
        ));
    }
    let approval_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    PENDING.lock().unwrap().insert(
        approval_id.clone(),
        Pending { owner_id: ctx.user_id.clone(), session_id: ctx.session_id.clone(), tool: tool.to_string(), tx },
    );
    let expires_at = chrono::Utc::now() + chrono::Duration::from_std(APPROVAL_TIMEOUT).unwrap_or_default();
    ctx.send(
        "tool_approval",
        json!({
            "approval_id": approval_id,
            "call_id": call_id,
            "tool": tool,
            "source": spec.source,
            "label": spec.label,
            "reason": spec.reason,
            "read_only": spec.read_only,
            "arguments": input,
            "expires_at": expires_at.to_rfc3339(),
        }),
    );
    // 没有网页在看（关掉了页面）且不是从飞书发起时，发通知提醒
    if ctx.origin.as_deref() != Some("im") && ctx.watchers.as_ref().is_some_and(|w| w() == 0) {
        let pool = crate::state::db_pool();
        let _ = crate::db::notification_repo::create(
            &pool,
            &ctx.user_id,
            "approval",
            &format!("需要你确认：{} · {}", spec.source, spec.label),
            &format!("{}，10 分钟内没有处理会自动拒绝", spec.reason),
            Some(&format!("/?s={}", ctx.session_id)),
        )
        .await;
    }

    let (decision, label) = tokio::select! {
        result = rx => match result {
            Ok(Decision::Approve) => (Outcome::Approved, "approved"),
            Ok(Decision::Reject) => (Outcome::Denied(format!("用户拒绝了这次调用（{}）。不要重试，直接告诉用户没有执行，并说明可以怎么做。", spec.label)), "rejected"),
            Err(_) => (Outcome::Denied("确认被中断".into()), "rejected"),
        },
        _ = tokio::time::sleep(APPROVAL_TIMEOUT) => (Outcome::Denied(format!("等待用户确认超时，没有执行 {}。", spec.label)), "expired"),
        _ = cancel.cancelled() => (Outcome::Denied("用户已停止生成，工具未执行".into()), "cancelled"),
    };
    PENDING.lock().unwrap().remove(&approval_id);
    ctx.send("tool_approval_resolved", json!({ "approval_id": approval_id, "call_id": call_id, "decision": label }));
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(origin: &str) -> (ToolContext, std::sync::Arc<Mutex<Vec<(String, Value)>>>) {
        let events: std::sync::Arc<Mutex<Vec<(String, Value)>>> = Default::default();
        let sink = events.clone();
        let ctx = ToolContext::new("s1".into(), "u1".into(), None, None, vec![], move |e: &str, d: Value| sink.lock().unwrap().push((e.to_string(), d)))
            .with_origin(origin);
        (ctx, events)
    }

    fn spec() -> ApprovalSpec {
        ApprovalSpec { source: "工单".into(), label: "创建工单".into(), reason: "可能修改数据".into(), read_only: false }
    }

    async fn wait_for_id(events: &std::sync::Arc<Mutex<Vec<(String, Value)>>>) -> String {
        for _ in 0..100 {
            if let Some((_, d)) = events.lock().unwrap().iter().find(|(e, _)| e == "tool_approval") {
                return d["approval_id"].as_str().unwrap().to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("没有发出确认事件");
    }

    #[tokio::test]
    async fn approve_reject_and_remember() {
        let (c, events) = ctx("chat");
        let cancel = CancellationToken::new();
        let waiter = { let c = c.clone(); let cancel = cancel.clone(); tokio::spawn(async move { request(&c, "c1", "mcp__t__create", &spec(), &json!({ "title": "x" }), &cancel).await }) };
        let id = wait_for_id(&events).await;
        assert!(resolve(&id, "someone-else", Decision::Approve, false).is_err(), "别人不能处理");
        resolve(&id, "u1", Decision::Approve, true).unwrap();
        assert_eq!(waiter.await.unwrap(), Outcome::Approved);
        assert!(resolve(&id, "u1", Decision::Approve, false).is_err(), "只能处理一次");
        assert!(events.lock().unwrap().iter().any(|(e, d)| e == "tool_approval_resolved" && d["decision"] == "approved"));
        // 记住之后同一会话同一工具直接通过
        assert_eq!(request(&c, "c2", "mcp__t__create", &spec(), &json!({}), &cancel).await, Outcome::Approved);

        let (c, events) = ctx("chat");
        let waiter = { let cancel = cancel.clone(); tokio::spawn(async move { request(&c, "c3", "mcp__t__other", &spec(), &json!({}), &cancel).await }) };
        let id = wait_for_id(&events).await;
        resolve(&id, "u1", Decision::Reject, true).unwrap();
        assert!(matches!(waiter.await.unwrap(), Outcome::Denied(m) if m.contains("拒绝")));
        assert!(!remembered("s1", "mcp__t__other"), "拒绝不会被记住");
    }

    #[tokio::test]
    async fn cancel_and_schedule() {
        let (c, events) = ctx("chat");
        let cancel = CancellationToken::new();
        let waiter = { let cancel = cancel.clone(); tokio::spawn(async move { request(&c, "c1", "mcp__t__w", &spec(), &json!({}), &cancel).await }) };
        wait_for_id(&events).await;
        cancel.cancel();
        assert!(matches!(waiter.await.unwrap(), Outcome::Denied(m) if m.contains("停止")));

        let (c, events) = ctx("schedule");
        let out = request(&c, "c1", "mcp__t__w", &spec(), &json!({}), &CancellationToken::new()).await;
        assert!(matches!(out, Outcome::Denied(m) if m.contains("定时任务")));
        assert!(events.lock().unwrap().is_empty(), "定时任务不发确认事件");
    }
}
