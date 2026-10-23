//! 产物版本：查看历史、预览旧版本、恢复、保存用户手动修改

use axum::extract::{Path, Query};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::db::{artifact_repo, session_repo, DbPool};
use crate::error::AppError;
use crate::models::Artifact;
use crate::state;

pub fn router() -> Router {
    Router::new()
        .route("/api/artifacts/:artifact_id", axum::routing::put(save_user_edit))
        .route("/api/artifacts/:artifact_id/versions", get(list_versions))
        .route("/api/artifacts/:artifact_id/versions/:version", get(get_version))
        .route("/api/artifacts/:artifact_id/restore", post(restore_version))
}

#[derive(Deserialize)]
struct SessionQuery {
    session_id: String,
}

#[derive(Deserialize)]
struct RestorePayload {
    session_id: String,
    version: i32,
}

#[derive(Deserialize)]
struct UserEditPayload {
    session_id: String,
    #[serde(default)]
    title: Option<String>,
    content: Value,
    #[serde(default)]
    change_summary: Option<String>,
}

/// 校验会话归属并取出当前产物
pub(crate) async fn load_owned(pool: &DbPool, user: &AuthUser, session_id: &str, artifact_id: &str) -> Result<Artifact, AppError> {
    let session = session_repo::find_by_id(pool, session_id)
        .await?
        .ok_or(AppError::NotFound("会话不存在".into()))?;
    if session.owner_id != user.0.id {
        return Err(AppError::Forbidden);
    }
    artifact_repo::find_in_session(pool, session_id, artifact_id)
        .await?
        .ok_or(AppError::NotFound("产物不存在".into()))
}

/// PPT 的幻灯片还存在项目文件里，版本切换后同步写回
fn sync_ppt(artifact: &Artifact) {
    if artifact.kind == "ppt" {
        let summary = artifact.change_summary.clone().unwrap_or_default();
        if let Err(e) = crate::agent::tools::artifact_edit::sync_ppt_project(&artifact.content, &artifact.title, &summary) {
            tracing::warn!("[Artifact] 同步 PPT 项目失败: {e}");
        }
    }
}

async fn list_versions(
    user: AuthUser,
    Path(artifact_id): Path<String>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let current = load_owned(&pool, &user, &query.session_id, &artifact_id).await?;
    let versions = artifact_repo::list_versions(&pool, &query.session_id, &current).await?;
    Ok(Json(json!({ "current_version": current.version, "versions": versions })))
}

async fn get_version(
    user: AuthUser,
    Path((artifact_id, version)): Path<(String, i32)>,
    Query(query): Query<SessionQuery>,
) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let current = load_owned(&pool, &user, &query.session_id, &artifact_id).await?;
    let snapshot = artifact_repo::get_version(&pool, &query.session_id, &current, version)
        .await?
        .ok_or(AppError::NotFound(format!("版本 v{version} 不存在")))?;
    Ok(Json(json!({ "artifact": snapshot })))
}

/// 恢复旧版本：以旧内容生成一个新版本，历史不丢失
async fn restore_version(
    user: AuthUser,
    Path(artifact_id): Path<String>,
    Json(payload): Json<RestorePayload>,
) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let current = load_owned(&pool, &user, &payload.session_id, &artifact_id).await?;
    let snapshot = artifact_repo::get_version(&pool, &payload.session_id, &current, payload.version)
        .await?
        .ok_or(AppError::NotFound(format!("版本 v{} 不存在", payload.version)))?;
    let restored = artifact_repo::commit_new_version(
        &pool,
        &payload.session_id,
        &current,
        snapshot.title,
        snapshot.content,
        format!("恢复到 v{}", payload.version),
        artifact_repo::AUTHOR_USER,
    )
    .await?;
    sync_ppt(&restored);
    crate::file_store::save_artifact_to_files(&pool, &user.0.id, &restored, crate::file_store::SaveTarget::Generated).await;
    Ok(Json(json!({ "artifact": restored })))
}

/// 用户在界面上手动修改（例如在 draw.io 中编辑后保存）
async fn save_user_edit(
    user: AuthUser,
    Path(artifact_id): Path<String>,
    Json(payload): Json<UserEditPayload>,
) -> Result<Json<Value>, AppError> {
    let pool = state::db_pool();
    let current = load_owned(&pool, &user, &payload.session_id, &artifact_id).await?;
    if current.content == payload.content && payload.title.as_deref().map_or(true, |t| t == current.title) {
        return Ok(Json(json!({ "artifact": current, "unchanged": true })));
    }
    let saved = artifact_repo::commit_new_version(
        &pool,
        &payload.session_id,
        &current,
        payload.title.unwrap_or_else(|| current.title.clone()),
        payload.content,
        payload.change_summary.unwrap_or_else(|| "手动编辑".to_string()),
        artifact_repo::AUTHOR_USER,
    )
    .await?;
    sync_ppt(&saved);
    crate::file_store::save_artifact_to_files(&pool, &user.0.id, &saved, crate::file_store::SaveTarget::Generated).await;
    Ok(Json(json!({ "artifact": saved })))
}
