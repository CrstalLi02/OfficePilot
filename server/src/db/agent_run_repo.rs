//! Plan 模式下等待用户确认的 Agent 运行

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{text_column, DbPool};
use crate::agent::plan::Plan;
use crate::error::AppResult;
use crate::models::ChatMessage;

pub const STATUS_AWAITING: &str = "awaiting_approval";

#[derive(Debug, Clone)]
pub struct AgentRun {
    pub id: String,
    pub session_id: String,
    pub owner_id: String,
    pub plan: Plan,
    pub conversation: Vec<ChatMessage>,
    pub plan_call_id: String,
    pub other_call_ids: Vec<String>,
    pub allowed_tools: Option<Vec<String>>,
}

/// 会话详情里返回给前端的待确认计划
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPlan {
    pub run_id: String,
    pub plan: Plan,
}

#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &DbPool,
    session_id: &str,
    owner_id: &str,
    plan: &Plan,
    conversation: &[ChatMessage],
    plan_call_id: &str,
    other_call_ids: &[String],
    allowed_tools: Option<&[String]>,
) -> AppResult<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // 同一会话只保留一个待确认计划
    sqlx::query("UPDATE agent_runs SET status = 'superseded', updated_at = ? WHERE session_id = ? AND status = ?")
        .bind(&now)
        .bind(session_id)
        .bind(STATUS_AWAITING)
        .execute(pool)
        .await?;
    sqlx::query(
        "INSERT INTO agent_runs (id, session_id, owner_id, status, plan, conversation, plan_call_id, other_call_ids, allowed_tools, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(session_id)
    .bind(owner_id)
    .bind(STATUS_AWAITING)
    .bind(serde_json::to_string(plan)?)
    .bind(serde_json::to_string(conversation)?)
    .bind(plan_call_id)
    .bind(serde_json::to_string(other_call_ids)?)
    .bind(allowed_tools.map(serde_json::to_string).transpose()?)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

fn from_row(row: &sqlx::any::AnyRow) -> AppResult<AgentRun> {
    let json = |index: usize| -> AppResult<String> { Ok(text_column(row, index)?.unwrap_or_default()) };
    Ok(AgentRun {
        id: row.try_get(0)?,
        session_id: row.try_get(1)?,
        owner_id: row.try_get(2)?,
        plan: serde_json::from_str(&json(3)?)?,
        conversation: serde_json::from_str(&json(4)?)?,
        plan_call_id: row.try_get(5)?,
        other_call_ids: serde_json::from_str(&json(6)?).unwrap_or_default(),
        allowed_tools: text_column(row, 7)?
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::from_str(&s).ok()),
    })
}

const COLUMNS: &str = "id, session_id, owner_id, plan, conversation, plan_call_id, other_call_ids, allowed_tools";

pub async fn find(pool: &DbPool, id: &str) -> AppResult<Option<AgentRun>> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM agent_runs WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.as_ref().map(from_row).transpose()
}

/// 把待确认的运行切换到新状态；返回 false 表示它已经被处理过（防止重复提交）
pub async fn resolve(pool: &DbPool, id: &str, status: &str) -> AppResult<bool> {
    let result = sqlx::query("UPDATE agent_runs SET status = ?, updated_at = ? WHERE id = ? AND status = ?")
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(id)
        .bind(STATUS_AWAITING)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn pending_for_session(pool: &DbPool, session_id: &str) -> AppResult<Option<PendingPlan>> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM agent_runs WHERE session_id = ? AND status = ? ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(session_id)
    .bind(STATUS_AWAITING)
    .fetch_optional(pool)
    .await?;
    Ok(row
        .as_ref()
        .map(from_row)
        .transpose()?
        .map(|run| PendingPlan {
            run_id: run.id,
            plan: run.plan,
        }))
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
            include_str!("../../../migrations/002_agent_runs.sql"),
            "INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES ('u1', 'u', 'x', 't', 't');
             INSERT INTO sessions (id, owner_id, title, created_at, updated_at) VALUES ('s1', 'u1', 't', 't', 't');",
        ] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    #[tokio::test]
    async fn create_find_resolve_and_supersede() {
        let pool = memory_pool().await;
        let plan = Plan::from_arguments(r#"{"goal":"g","steps":[{"tool":"md_generate","purpose":"写"}]}"#).unwrap();
        let conversation = vec![ChatMessage {
            role: "user".into(),
            content: "做个文档".into(),
            tool_calls: None,
            tool_call_id: None,
        }];

        let first = create(&pool, "s1", "u1", &plan, &conversation, "c1", &[], None).await.unwrap();
        let second = create(&pool, "s1", "u1", &plan, &conversation, "c2", &["c3".into()], Some(&["image_prompt".to_string()]))
            .await
            .unwrap();

        // 新计划会顶替旧的待确认计划
        assert!(!resolve(&pool, &first, "approved").await.unwrap(), "旧计划已被顶替，不能再确认");
        let pending = pending_for_session(&pool, "s1").await.unwrap().unwrap();
        assert_eq!(pending.run_id, second);

        let run = find(&pool, &second).await.unwrap().unwrap();
        assert_eq!(run.conversation[0].content, "做个文档");
        assert_eq!(run.other_call_ids, vec!["c3".to_string()]);
        assert_eq!(run.allowed_tools, Some(vec!["image_prompt".to_string()]));

        assert!(resolve(&pool, &second, "approved").await.unwrap());
        assert!(!resolve(&pool, &second, "approved").await.unwrap(), "重复确认应被拒绝");
        assert!(pending_for_session(&pool, "s1").await.unwrap().is_none());
    }
}
