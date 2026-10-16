//! 产物版本历史

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{session_repo, text_column, DbPool};
use crate::error::AppResult;
use crate::models::Artifact;

pub const AUTHOR_AGENT: &str = "agent";
pub const AUTHOR_USER: &str = "user";

/// 版本列表项（不含内容）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionMeta {
    pub version: i32,
    pub title: String,
    pub change_summary: Option<String>,
    pub author: String,
    pub created_at: String,
}

/// 记录产物的一个版本；同一版本号已存在时跳过（重复推送、重试都是安全的）
pub async fn record_version(pool: &DbPool, session_id: &str, artifact: &Artifact, author: &str) -> AppResult<()> {
    let exists = sqlx::query("SELECT 1 FROM artifact_versions WHERE artifact_id = ? AND version = ?")
        .bind(&artifact.id)
        .bind(artifact.version)
        .fetch_optional(pool)
        .await?
        .is_some();
    if exists {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO artifact_versions (artifact_id, version, session_id, kind, title, content, change_summary, author, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&artifact.id)
    .bind(artifact.version)
    .bind(session_id)
    .bind(&artifact.kind)
    .bind(&artifact.title)
    .bind(serde_json::to_string(&artifact.content)?)
    .bind(artifact.change_summary.as_deref())
    .bind(author)
    .bind(&artifact.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// AI 修订产生的版本号若已被占用（例如期间用户恢复过版本），顺延到最新版本之后
pub async fn assign_next_version(pool: &DbPool, artifact: &mut Artifact) -> AppResult<()> {
    if artifact.version <= 1 {
        return Ok(());
    }
    let latest: Option<i64> = sqlx::query("SELECT MAX(version) FROM artifact_versions WHERE artifact_id = ?")
        .bind(&artifact.id)
        .fetch_one(pool)
        .await?
        .try_get(0)?;
    if let Some(latest) = latest {
        if latest >= artifact.version as i64 {
            artifact.version = latest as i32 + 1;
        }
    }
    Ok(())
}

/// 版本列表（新到旧）。早于版本功能的产物没有记录时，把当前内容补记为它的版本
pub async fn list_versions(pool: &DbPool, session_id: &str, current: &Artifact) -> AppResult<Vec<VersionMeta>> {
    record_version(pool, session_id, current, AUTHOR_AGENT).await?;
    let rows = sqlx::query(
        "SELECT version, title, change_summary, author, created_at FROM artifact_versions
         WHERE artifact_id = ? AND session_id = ? ORDER BY version DESC",
    )
    .bind(&current.id)
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(VersionMeta {
                version: row.try_get::<i64, _>(0)? as i32,
                title: text_column(row, 1)?.unwrap_or_default(),
                change_summary: text_column(row, 2)?,
                author: text_column(row, 3)?.unwrap_or_default(),
                created_at: text_column(row, 4)?.unwrap_or_default(),
            })
        })
        .collect()
}

/// 某个版本的完整快照（以当前产物为模板，替换标题、内容、版本号等）
pub async fn get_version(pool: &DbPool, session_id: &str, current: &Artifact, version: i32) -> AppResult<Option<Artifact>> {
    let row = sqlx::query(
        "SELECT title, content, change_summary, created_at FROM artifact_versions
         WHERE artifact_id = ? AND session_id = ? AND version = ?",
    )
    .bind(&current.id)
    .bind(session_id)
    .bind(version)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let mut snapshot = current.clone();
    snapshot.version = version;
    snapshot.title = text_column(&row, 0)?.unwrap_or_default();
    snapshot.content = serde_json::from_str(&text_column(&row, 1)?.unwrap_or_default())?;
    snapshot.change_summary = text_column(&row, 2)?;
    snapshot.updated_at = text_column(&row, 3)?.unwrap_or_default();
    Ok(Some(snapshot))
}

/// 在会话产物中查找
pub async fn find_in_session(pool: &DbPool, session_id: &str, artifact_id: &str) -> AppResult<Option<Artifact>> {
    Ok(session_repo::get_artifacts(pool, session_id)
        .await?
        .into_iter()
        .find(|a| a.id == artifact_id))
}

/// 以新内容生成下一个版本：写入版本表，并替换会话里的当前产物
pub async fn commit_new_version(
    pool: &DbPool,
    session_id: &str,
    current: &Artifact,
    title: String,
    content: serde_json::Value,
    change_summary: String,
    author: &str,
) -> AppResult<Artifact> {
    // 先确保旧版本已记录（老产物补记）
    record_version(pool, session_id, current, AUTHOR_AGENT).await?;
    let latest: Option<i64> = sqlx::query("SELECT MAX(version) FROM artifact_versions WHERE artifact_id = ?")
        .bind(&current.id)
        .fetch_one(pool)
        .await?
        .try_get(0)?;
    let mut next = current.clone();
    next.version = latest.unwrap_or(current.version as i64).max(current.version as i64) as i32 + 1;
    next.title = title;
    next.content = content;
    next.change_summary = Some(change_summary);
    next.updated_at = chrono::Utc::now().to_rfc3339();
    record_version(pool, session_id, &next, author).await?;

    let mut artifacts = session_repo::get_artifacts(pool, session_id).await?;
    match artifacts.iter_mut().find(|a| a.id == next.id) {
        Some(slot) => *slot = next.clone(),
        None => artifacts.insert(0, next.clone()),
    }
    session_repo::save_artifacts(pool, session_id, &artifacts).await?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_pool() -> DbPool {
        sqlx::any::install_default_drivers();
        let pool = sqlx::any::AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for sql in [
            include_str!("../../../migrations/001_init.sql"),
            include_str!("../../../migrations/003_artifact_versions.sql"),
            "INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES ('u1', 'u', 'x', 't', 't');
             INSERT INTO sessions (id, owner_id, title, created_at, updated_at) VALUES ('s1', 'u1', 't', 't', 't');",
        ] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    fn artifact() -> Artifact {
        Artifact {
            id: "a1".into(),
            kind: "markdown".into(),
            tool_kind: "doc".into(),
            title: "周报".into(),
            status: "ready".into(),
            content: serde_json::json!({ "markdown": "# 周报\n\nv1" }),
            version: 1,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            change_summary: None,
        }
    }

    #[tokio::test]
    async fn versions_backfill_append_and_snapshot() {
        let pool = memory_pool().await;
        let v1 = artifact();
        // 模拟早于版本功能的产物：只在会话里，版本表为空
        session_repo::save_artifacts(&pool, "s1", &[v1.clone()]).await.unwrap();

        let v2 = commit_new_version(&pool, "s1", &v1, "周报".into(), serde_json::json!({ "markdown": "# 周报\n\nv2" }), "改了正文".into(), AUTHOR_AGENT)
            .await
            .unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(v2.id, "a1");

        // 会话里的当前产物已替换为 v2
        let current = find_in_session(&pool, "s1", "a1").await.unwrap().unwrap();
        assert_eq!(current.version, 2);

        let list = list_versions(&pool, "s1", &current).await.unwrap();
        assert_eq!(list.iter().map(|v| v.version).collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(list[0].change_summary.as_deref(), Some("改了正文"));

        let old = get_version(&pool, "s1", &current, 1).await.unwrap().unwrap();
        assert_eq!(old.content["markdown"], "# 周报\n\nv1");
        assert!(get_version(&pool, "s1", &current, 9).await.unwrap().is_none());

        // 重复记录同一版本是安全的
        record_version(&pool, "s1", &current, AUTHOR_AGENT).await.unwrap();
        assert_eq!(list_versions(&pool, "s1", &current).await.unwrap().len(), 2);

        // 用户恢复 v1 → 生成 v3
        let v3 = commit_new_version(&pool, "s1", &current, old.title.clone(), old.content.clone(), "恢复到 v1".into(), AUTHOR_USER)
            .await
            .unwrap();
        assert_eq!(v3.version, 3);
        assert_eq!(v3.content["markdown"], "# 周报\n\nv1");
    }
}
