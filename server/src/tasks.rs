//! 后台任务：Agent 运行与 HTTP 连接解耦。
//!
//! - 每次运行是一条 `agent_tasks` 记录（来源：对话 / 定时任务 / IM），状态随运行推进；
//! - 运行中的事件写入内存回放缓冲并广播：任何时候订阅都会先收到历史事件、再接实时事件，
//!   所以刷新页面、换设备打开都能接上进度；
//! - 每个用户同时运行的任务数有上限，超出的排队；
//! - 服务重启时，未完成的任务标记为“已中断”，可以一键重试。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use once_cell::sync::Lazy;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::sync::{broadcast, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::db::{text_column, DbPool};
use crate::error::AppResult;
use crate::models::ChatRequest;

/// 每个用户同时运行的任务数
const MAX_RUNNING_PER_USER: usize = 3;
/// 任务结束后，回放缓冲继续保留的时间（供晚到的订阅者拿到结果）
const KEEP_AFTER_FINISH: Duration = Duration::from_secs(600);
/// 回放缓冲的事件上限（超出后丢弃最早的文本增量）
const MAX_BUFFERED_EVENTS: usize = 20_000;

#[derive(Debug, Clone, Serialize)]
pub struct TaskEvent {
    pub seq: u64,
    pub event: String,
    pub data: Value,
}

impl TaskEvent {
    /// 运行结束的事件
    pub fn is_terminal(&self) -> bool {
        self.event == "done" || self.event == "error"
    }
}

struct Entry {
    owner_id: String,
    session_id: String,
    cancel: CancellationToken,
    events: Vec<TaskEvent>,
    next_seq: u64,
    finished: bool,
    tx: broadcast::Sender<TaskEvent>,
    /// 后台订阅者（IM 回推等）数量：不算“有人在看”
    background: usize,
}

static TASKS: Lazy<Mutex<HashMap<String, Arc<Mutex<Entry>>>>> = Lazy::new(Default::default);
static USER_SLOTS: Lazy<Mutex<HashMap<String, Arc<Semaphore>>>> = Lazy::new(Default::default);

/// 运行中的任务向外推送事件的句柄（同步，可在任何地方调用）
#[derive(Clone)]
pub struct TaskSink {
    pub task_id: String,
    entry: Arc<Mutex<Entry>>,
}

impl TaskSink {
    pub fn emit(&self, event: &str, data: Value) {
        let mut entry = self.entry.lock().unwrap();
        if entry.finished {
            return;
        }
        let ev = TaskEvent { seq: entry.next_seq, event: event.to_string(), data };
        entry.next_seq += 1;
        if entry.events.len() >= MAX_BUFFERED_EVENTS {
            // 优先丢最早的文本增量，保留工具 / 产物等结构化事件
            if let Some(pos) = entry.events.iter().position(|e| e.event == "message") {
                entry.events.remove(pos);
            } else {
                entry.events.remove(0);
            }
        }
        let terminal = ev.is_terminal();
        entry.events.push(ev.clone());
        let _ = entry.tx.send(ev);
        if terminal {
            entry.finished = true;
        }
    }

    /// 当前是否有人在看（网页订阅数，不含后台订阅），用于决定完成时要不要发通知
    pub fn watchers(&self) -> usize {
        let entry = self.entry.lock().unwrap();
        entry.tx.receiver_count().saturating_sub(entry.background)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Chat,
    Schedule,
    Im,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::Chat => "chat",
            Origin::Schedule => "schedule",
            Origin::Im => "im",
        }
    }
}

/// 去掉请求里体积大的图片数据，保存用于重试
fn storable_request(req: &ChatRequest) -> Value {
    let mut value = serde_json::to_value(req).unwrap_or(Value::Null);
    if let Some(attachments) = value.get_mut("attachments").and_then(Value::as_array_mut) {
        for a in attachments {
            if let Some(obj) = a.as_object_mut() {
                obj.remove("data_url");
            }
        }
    }
    value
}

/// 登记一个新任务（状态 queued），返回推送句柄和取消令牌
pub async fn create(
    pool: &DbPool,
    owner_id: &str,
    session_id: &str,
    origin: Origin,
    title: &str,
    req: &ChatRequest,
) -> AppResult<(TaskSink, CancellationToken)> {
    let task_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO agent_tasks (id, owner_id, session_id, origin, title, status, request, created_at) VALUES (?, ?, ?, ?, ?, 'queued', ?, ?)",
    )
    .bind(&task_id)
    .bind(owner_id)
    .bind(session_id)
    .bind(origin.as_str())
    .bind(title.chars().take(200).collect::<String>())
    .bind(storable_request(req).to_string())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;

    let cancel = CancellationToken::new();
    let (tx, _) = broadcast::channel(1024);
    let entry = Arc::new(Mutex::new(Entry {
        owner_id: owner_id.to_string(),
        session_id: session_id.to_string(),
        cancel: cancel.clone(),
        events: Vec::new(),
        next_seq: 1,
        finished: false,
        tx,
        background: 0,
    }));
    {
        let mut tasks = TASKS.lock().unwrap();
        // 同一会话已有运行中的任务时取消它（新消息优先）
        for (id, other) in tasks.iter() {
            let other = other.lock().unwrap();
            if other.session_id == session_id && !other.finished && id != &task_id {
                other.cancel.cancel();
            }
        }
        tasks.insert(task_id.clone(), entry.clone());
    }
    Ok((TaskSink { task_id, entry }, cancel))
}

/// 等待该用户的运行名额；拿到后把状态改为 running
pub async fn acquire_slot(pool: &DbPool, sink: &TaskSink, owner_id: &str) -> OwnedSemaphorePermit {
    let slots = USER_SLOTS
        .lock()
        .unwrap()
        .entry(owner_id.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(MAX_RUNNING_PER_USER)))
        .clone();
    if slots.available_permits() == 0 {
        sink.emit("state_update", json!({ "phase": "queued", "step": "排队中", "detail": format!("同时最多运行 {MAX_RUNNING_PER_USER} 个任务，正在等待") }));
    }
    let permit = slots.acquire_owned().await.expect("semaphore closed");
    let _ = sqlx::query("UPDATE agent_tasks SET status = 'running', started_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&sink.task_id)
        .execute(pool)
        .await;
    permit
}

/// 写入最终状态；一段时间后清理回放缓冲
pub async fn finish(pool: &DbPool, sink: &TaskSink, status: &str, summary: Option<&str>, error: Option<&str>) {
    {
        let mut entry = sink.entry.lock().unwrap();
        entry.finished = true;
    }
    let _ = sqlx::query("UPDATE agent_tasks SET status = ?, summary = ?, error = ?, finished_at = ? WHERE id = ?")
        .bind(status)
        .bind(summary.map(|s| s.chars().take(1000).collect::<String>()))
        .bind(error.map(|s| s.chars().take(1000).collect::<String>()))
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&sink.task_id)
        .execute(pool)
        .await;
    let task_id = sink.task_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(KEEP_AFTER_FINISH).await;
        TASKS.lock().unwrap().remove(&task_id);
    });
}

/// 订阅：返回 after 之后的历史事件、实时接收端，以及任务是否已结束
pub fn subscribe(task_id: &str, owner_id: &str, after: u64) -> Option<(Vec<TaskEvent>, broadcast::Receiver<TaskEvent>, bool)> {
    let entry = TASKS.lock().unwrap().get(task_id)?.clone();
    let entry = entry.lock().unwrap();
    if entry.owner_id != owner_id {
        return None;
    }
    let backlog = entry.events.iter().filter(|e| e.seq > after).cloned().collect();
    Some((backlog, entry.tx.subscribe(), entry.finished))
}

/// 后台订阅的凭据：持有期间不计入“有人在看”，丢弃时自动注销
pub struct BackgroundGuard {
    entry: Arc<Mutex<Entry>>,
}

impl Drop for BackgroundGuard {
    fn drop(&mut self) {
        let mut entry = self.entry.lock().unwrap();
        entry.background = entry.background.saturating_sub(1);
    }
}

/// 后台订阅（IM 回推进度等）：和 subscribe 一样，但不影响完成通知
pub fn subscribe_background(
    task_id: &str,
    owner_id: &str,
) -> Option<(Vec<TaskEvent>, broadcast::Receiver<TaskEvent>, BackgroundGuard)> {
    let shared = TASKS.lock().unwrap().get(task_id)?.clone();
    let mut entry = shared.lock().unwrap();
    if entry.owner_id != owner_id {
        return None;
    }
    entry.background += 1;
    let backlog = entry.events.clone();
    let rx = entry.tx.subscribe();
    drop(entry);
    Some((backlog, rx, BackgroundGuard { entry: shared }))
}

/// 接收端落后太多（广播缓冲溢出）时，从回放缓冲补齐
pub fn events_after(task_id: &str, after: u64) -> Vec<TaskEvent> {
    TASKS
        .lock()
        .unwrap()
        .get(task_id)
        .map(|e| e.lock().unwrap().events.iter().filter(|ev| ev.seq > after).cloned().collect())
        .unwrap_or_default()
}

pub fn cancel(task_id: &str, owner_id: &str) -> bool {
    match TASKS.lock().unwrap().get(task_id) {
        Some(entry) => {
            let entry = entry.lock().unwrap();
            if entry.owner_id != owner_id || entry.finished {
                return false;
            }
            entry.cancel.cancel();
            true
        }
        None => false,
    }
}

/// 某会话正在运行的任务
pub fn running_for_session(session_id: &str, owner_id: &str) -> Option<String> {
    TASKS.lock().unwrap().iter().find_map(|(id, entry)| {
        let entry = entry.lock().unwrap();
        (entry.session_id == session_id && entry.owner_id == owner_id && !entry.finished).then(|| id.clone())
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRow {
    pub id: String,
    pub session_id: String,
    pub origin: String,
    pub title: String,
    pub status: String,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// 仍在内存中运行（可订阅进度）
    pub live: bool,
}

const COLUMNS: &str = "id, session_id, origin, title, status, summary, error, created_at, started_at, finished_at";

fn row_to_task(row: &sqlx::any::AnyRow) -> AppResult<TaskRow> {
    let id: String = row.try_get(0)?;
    let live = TASKS.lock().unwrap().get(&id).is_some_and(|e| !e.lock().unwrap().finished);
    Ok(TaskRow {
        session_id: text_column(row, 1)?.unwrap_or_default(),
        origin: text_column(row, 2)?.unwrap_or_default(),
        title: text_column(row, 3)?.unwrap_or_default(),
        status: text_column(row, 4)?.unwrap_or_default(),
        summary: text_column(row, 5)?,
        error: text_column(row, 6)?,
        created_at: text_column(row, 7)?.unwrap_or_default(),
        started_at: text_column(row, 8)?,
        finished_at: text_column(row, 9)?,
        live,
        id,
    })
}

pub async fn list(pool: &DbPool, owner_id: &str, limit: i64) -> AppResult<Vec<TaskRow>> {
    let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM agent_tasks WHERE owner_id = ? ORDER BY created_at DESC LIMIT ?"))
        .bind(owner_id)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    rows.iter().map(row_to_task).collect()
}

pub async fn get(pool: &DbPool, owner_id: &str, task_id: &str) -> AppResult<Option<(TaskRow, Value)>> {
    let row = sqlx::query(&format!("SELECT {COLUMNS}, request FROM agent_tasks WHERE id = ? AND owner_id = ?"))
        .bind(task_id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await?;
    match row {
        Some(row) => {
            let request = serde_json::from_str(&text_column(&row, 10)?.unwrap_or_default()).unwrap_or(Value::Null);
            Ok(Some((row_to_task(&row)?, request)))
        }
        None => Ok(None),
    }
}

/// 服务启动时：上次未完成的任务无法从中途恢复，标记为已中断（可重试）
pub async fn mark_interrupted(pool: &DbPool) -> AppResult<u64> {
    let result = sqlx::query("UPDATE agent_tasks SET status = 'interrupted', error = '服务重启，任务中断', finished_at = ? WHERE status IN ('queued', 'running')")
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_pool() -> DbPool {
        sqlx::any::install_default_drivers();
        let pool = sqlx::any::AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/006_agent_tasks.sql")).execute(&pool).await.unwrap();
        pool
    }

    fn req() -> ChatRequest {
        ChatRequest {
            message: "写周报".into(),
            attachments: Some(vec![crate::models::ChatAttachment {
                id: "a".into(),
                name: "x.png".into(),
                kind: "image".into(),
                mime_type: "image/png".into(),
                size: 1,
                text_content: None,
                data_url: Some("data:image/png;base64,AAAA".into()),
            }]),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn replay_then_live_and_terminal() {
        let pool = memory_pool().await;
        let (sink, _cancel) = create(&pool, "u1", "s1", Origin::Chat, "写周报", &req()).await.unwrap();
        sink.emit("message", json!({"text": "你"}));
        sink.emit("message", json!({"text": "好"}));

        // 晚到的订阅者先拿到历史事件
        let (backlog, mut rx, finished) = subscribe(&sink.task_id, "u1", 0).unwrap();
        assert_eq!(backlog.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2]);
        assert!(!finished);
        assert_eq!(sink.watchers(), 1);
        // 断点续传：只要 seq > 1 的
        assert_eq!(subscribe(&sink.task_id, "u1", 1).unwrap().0.len(), 1);
        // 其他用户订阅不到
        assert!(subscribe(&sink.task_id, "u2", 0).is_none());
        assert_eq!(running_for_session("s1", "u1").as_deref(), Some(sink.task_id.as_str()));

        sink.emit("done", json!({"summary": "ok"}));
        let live = rx.recv().await.unwrap();
        assert!(live.is_terminal());
        sink.emit("message", json!({"text": "结束后的事件被忽略"}));
        assert_eq!(events_after(&sink.task_id, 0).len(), 3);
        assert!(running_for_session("s1", "u1").is_none());

        finish(&pool, &sink, "succeeded", Some("ok"), None).await;
        let (row, request) = get(&pool, "u1", &sink.task_id).await.unwrap().unwrap();
        assert_eq!(row.status, "succeeded");
        assert!(!row.live);
        assert!(request["attachments"][0].get("data_url").is_none(), "图片数据不入库");
        assert_eq!(request["message"], "写周报");
    }

    #[tokio::test]
    async fn new_task_cancels_previous_in_same_session_and_restart_marks_interrupted() {
        let pool = memory_pool().await;
        let (first, first_cancel) = create(&pool, "u1", "s2", Origin::Chat, "a", &req()).await.unwrap();
        let (_second, second_cancel) = create(&pool, "u1", "s2", Origin::Chat, "b", &req()).await.unwrap();
        assert!(first_cancel.is_cancelled());
        assert!(!second_cancel.is_cancelled());
        assert!(!cancel(&first.task_id, "u2"), "不能取消别人的任务");

        let _permit = acquire_slot(&pool, &first, "u1").await;
        assert_eq!(get(&pool, "u1", &first.task_id).await.unwrap().unwrap().0.status, "running");
        assert_eq!(mark_interrupted(&pool).await.unwrap(), 2);
        let rows = list(&pool, "u1", 10).await.unwrap();
        assert!(rows.iter().all(|r| r.status == "interrupted"));
    }

    #[tokio::test]
    async fn slots_limit_concurrent_runs() {
        let pool = memory_pool().await;
        let mut permits = Vec::new();
        for i in 0..MAX_RUNNING_PER_USER {
            let (sink, _) = create(&pool, "u3", &format!("s{i}"), Origin::Chat, "t", &req()).await.unwrap();
            permits.push(acquire_slot(&pool, &sink, "u3").await);
        }
        let (queued, _) = create(&pool, "u3", "sx", Origin::Chat, "t", &req()).await.unwrap();
        let waiting = tokio::time::timeout(Duration::from_millis(100), acquire_slot(&pool, &queued, "u3")).await;
        assert!(waiting.is_err(), "超过上限时排队");
        assert!(events_after(&queued.task_id, 0).iter().any(|e| e.data["phase"] == "queued"));
        permits.pop();
        assert!(tokio::time::timeout(Duration::from_millis(500), acquire_slot(&pool, &queued, "u3")).await.is_ok());
    }
}
