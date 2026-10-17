//! 定时任务管理：增删改查、立即运行、运行记录、预览触发时间

use axum::extract::Path;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::error::AppError;
use crate::schedules::{self, ScheduleInput};
use crate::state;

pub fn router() -> Router {
    Router::new()
        .route("/api/schedules", get(list).post(create))
        .route("/api/schedules/preview", post(preview))
        .route("/api/schedules/:id", put(update).delete(remove))
        .route("/api/schedules/:id/run", post(run_now))
        .route("/api/schedules/:id/runs", get(runs))
}

async fn list(user: AuthUser) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({ "schedules": schedules::list(&state::db_pool(), &user.0.id).await? })))
}

async fn create(user: AuthUser, Json(input): Json<ScheduleInput>) -> Result<Json<Value>, AppError> {
    Ok(Json(json!(schedules::create(&state::db_pool(), &user.0.id, &input).await?)))
}

async fn update(user: AuthUser, Path(id): Path<String>, Json(input): Json<ScheduleInput>) -> Result<Json<Value>, AppError> {
    Ok(Json(json!(schedules::update(&state::db_pool(), &user.0.id, &id, &input).await?)))
}

async fn remove(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    if !schedules::delete(&state::db_pool(), &user.0.id, &id).await? {
        return Err(AppError::NotFound("定时任务不存在".into()));
    }
    Ok(Json(json!({ "deleted": id })))
}

async fn run_now(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let schedule = schedules::get(&pool, &user.0.id, &id)
        .await?
        .ok_or_else(|| AppError::NotFound("定时任务不存在".into()))?;
    Ok(Json(schedules::trigger(&pool, &user.0.id, &schedule).await?))
}

async fn runs(user: AuthUser, Path(id): Path<String>) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({ "runs": schedules::runs(&state::db_pool(), &user.0.id, &id, 30).await? })))
}

#[derive(Deserialize)]
struct PreviewInput {
    cron: String,
    #[serde(default)]
    timezone: Option<String>,
}

/// 校验 cron 并给出接下来 5 次触发时间（表单实时预览）
async fn preview(_user: AuthUser, Json(input): Json<PreviewInput>) -> Result<Json<Value>, AppError> {
    let timezone = input.timezone.unwrap_or_else(|| schedules::DEFAULT_TIMEZONE.to_string());
    let (cron, tz) = schedules::parse(&input.cron, &timezone).map_err(AppError::BadRequest)?;
    let next: Vec<String> = schedules::next_runs(&cron, tz, Utc::now(), 5)
        .iter()
        .map(|t| t.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string())
        .collect();
    Ok(Json(json!({ "description": schedules::describe(&input.cron), "timezone": timezone, "next_runs": next })))
}
