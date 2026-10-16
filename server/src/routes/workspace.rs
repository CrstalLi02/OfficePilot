//! 工作区：Agent 写入 / 整理记录与撤销

use axum::extract::Path;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::middleware::AuthUser;
use crate::db::text_column;
use crate::error::AppError;
use crate::state;

pub fn router() -> Router {
    Router::new()
        .route("/api/workspace/ops", get(list_ops))
        .route("/api/workspace/ops/:op_id/undo", post(undo_op))
}

/// 最近的 Agent 文件操作（文件页的"AI 整理记录"）
async fn list_ops(user: AuthUser) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let rows = sqlx::query(
        "SELECT id, session_id, summary, created_at, undone_at FROM workspace_ops
         WHERE owner_id = ? ORDER BY created_at DESC LIMIT 30",
    )
    .bind(&user.0.id)
    .fetch_all(&pool)
    .await?;
    let ops = rows
        .iter()
        .map(|row| {
            Ok(json!({
                "id": row.try_get::<String, _>(0)?,
                "session_id": text_column(row, 1)?,
                "summary": text_column(row, 2)?,
                "created_at": text_column(row, 3)?,
                "undone_at": text_column(row, 4)?,
            }))
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(json!({ "ops": ops })))
}

async fn undo_op(user: AuthUser, Path(op_id): Path<String>) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let report = crate::workspace::undo(&pool, &user.0.id, &op_id).await?;
    Ok(Json(json!({ "undone": report.undone, "skipped": report.skipped })))
}
