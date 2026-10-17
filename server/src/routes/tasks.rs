//! 后台任务：列表、详情、订阅进度（可断点续传）、取消、重试

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::models::ChatRequest;
use crate::state;
use crate::tasks;

pub fn router() -> Router {
    Router::new()
        .route("/api/tasks", get(list))
        .route("/api/tasks/:task_id", get(detail))
        .route("/api/tasks/:task_id/stream", get(stream))
        .route("/api/tasks/:task_id/cancel", post(cancel))
        .route("/api/tasks/:task_id/retry", post(retry))
}

async fn list(user: AuthUser) -> Result<Json<Value>, AppError> {
    let rows = tasks::list(&state::db_pool(), &user.0.id, 50).await?;
    Ok(Json(json!({ "tasks": rows })))
}

async fn detail(user: AuthUser, Path(task_id): Path<String>) -> Result<Json<Value>, AppError> {
    let (row, _) = tasks::get(&state::db_pool(), &user.0.id, &task_id)
        .await?
        .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
    Ok(Json(json!(row)))
}

#[derive(Deserialize)]
struct StreamQuery {
    #[serde(default)]
    after: Option<u64>,
}

/// 订阅进度：?after=seq 或 Last-Event-ID 头指定从哪个事件之后开始
async fn stream(user: AuthUser, Path(task_id): Path<String>, Query(q): Query<StreamQuery>, headers: HeaderMap) -> Result<impl IntoResponse, AppError> {
    let after = q.after.or_else(|| {
        headers
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
    });
    crate::routes::chat::task_sse(&task_id, &user.0.id, after.unwrap_or(0))
}

async fn cancel(user: AuthUser, Path(task_id): Path<String>) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({ "cancelled": tasks::cancel(&task_id, &user.0.id) })))
}

/// 用原请求在原会话里重新运行（计划确认这类一次性操作不重放）
async fn retry(user: AuthUser, Path(task_id): Path<String>) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let (row, request) = tasks::get(&pool, &user.0.id, &task_id)
        .await?
        .ok_or_else(|| AppError::NotFound("任务不存在".into()))?;
    if row.live {
        return Err(AppError::BadRequest("任务还在运行".into()));
    }
    let mut req: ChatRequest = serde_json::from_value(request).map_err(|e| AppError::BadRequest(format!("原请求无法解析：{e}")))?;
    if req.resume.is_some() {
        return Err(AppError::BadRequest("计划确认不能重试，请在对话里重新发起".into()));
    }
    req.session_id = Some(row.session_id.clone());
    let started = crate::routes::chat::start_chat_task(&user.0.id, req, tasks::Origin::Chat).await?;
    Ok(Json(json!({ "task_id": started.task_id, "session_id": started.session_id })))
}
