//! 定时任务：按 cron 表达式（在用户时区）定期发起一次 Agent 运行。
//!
//! - 调度循环每 20 秒检查到期的定时任务，通过 `start_chat_task` 以 `schedule` 来源发起后台任务；
//! - 上一次运行还没结束时跳过本次（记录为 skipped），避免任务堆积；
//! - 停机期间错过的多次触发，恢复后只补跑一次，下次时间从当前时刻往后算；
//! - 只支持 Craft / Ask 模式：定时运行时没有人在场确认计划。

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;

use crate::agent::plan::AgentMode;
use crate::db::{session_repo, text_column, DbPool};
use crate::error::{AppError, AppResult};
use crate::models::ChatRequest;

pub const DEFAULT_TIMEZONE: &str = "Asia/Shanghai";
const TICK: Duration = Duration::from_secs(20);
/// 每个用户最多的定时任务数
const MAX_SCHEDULES_PER_USER: i64 = 50;

fn bad(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

// ───────────────────────── cron ─────────────────────────

/// 校验并解析：只接受 5 段 cron（分 时 日 月 周），最小间隔 10 分钟
pub fn parse(cron: &str, timezone: &str) -> Result<(Cron, Tz), String> {
    let cron = cron.trim();
    if cron.split_whitespace().count() != 5 {
        return Err("cron 需要 5 段：分 时 日 月 周，例如 0 9 * * 1-5 表示工作日 9:00".to_string());
    }
    let tz = Tz::from_str(timezone.trim()).map_err(|_| format!("未知时区：{timezone}，例如 Asia/Shanghai"))?;
    let parsed = Cron::from_str(cron).map_err(|e| format!("cron 表达式无效：{e}"))?;
    // 间隔太密会占满运行名额：取接下来两次触发的间隔做检查
    let now = Utc::now().with_timezone(&tz);
    let first = parsed.find_next_occurrence(&now, false).map_err(|e| e.to_string())?;
    let second = parsed.find_next_occurrence(&first, false).map_err(|e| e.to_string())?;
    if (second - first).num_minutes() < 10 {
        return Err("触发间隔不能小于 10 分钟".to_string());
    }
    Ok((parsed, tz))
}

/// 之后的 n 次触发时间（UTC）
pub fn next_runs(cron: &Cron, tz: Tz, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
    let mut out = Vec::with_capacity(n);
    let mut cursor = after.with_timezone(&tz);
    while out.len() < n {
        match cron.find_next_occurrence(&cursor, false) {
            Ok(next) => {
                out.push(next.with_timezone(&Utc));
                cursor = next;
            }
            Err(_) => break,
        }
    }
    out
}

/// 常见 cron 写法的中文描述，其他情况原样显示
pub fn describe(cron: &str) -> String {
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() != 5 {
        return cron.to_string();
    }
    let (min, hour, dom, month, dow) = (parts[0], parts[1], parts[2], parts[3], parts[4]);
    let simple = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    let weekday = |d: &str| match d {
        "0" | "7" => Some("日"),
        "1" => Some("一"),
        "2" => Some("二"),
        "3" => Some("三"),
        "4" => Some("四"),
        "5" => Some("五"),
        "6" => Some("六"),
        _ => None,
    };
    if let (true, Some(step)) = (min == "0" || simple(min), hour.strip_prefix("*/")) {
        if dom == "*" && month == "*" && dow == "*" {
            return format!("每 {step} 小时（第 {min} 分）");
        }
    }
    if !(simple(min) && simple(hour)) || month != "*" {
        return format!("cron：{cron}");
    }
    let time = format!("{:0>2}:{:0>2}", hour, min);
    match (dom, dow) {
        ("*", "*") => format!("每天 {time}"),
        ("*", "1-5") => format!("每个工作日 {time}"),
        ("*", "0,6") | ("*", "6,0") => format!("每个周末 {time}"),
        ("*", d) if weekday(d).is_some() => format!("每周{} {time}", weekday(d).unwrap()),
        ("*", list) if list.split(',').all(|d| weekday(d).is_some()) => {
            let days: Vec<&str> = list.split(',').filter_map(weekday).collect();
            format!("每周{} {time}", days.join("、"))
        }
        ("L", "*") => format!("每月最后一天 {time}"),
        (d, "*") if simple(d) => format!("每月 {d} 日 {time}"),
        _ => format!("cron：{cron}"),
    }
}

// ───────────────────────── 存储 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub cron: String,
    pub timezone: String,
    pub description: String,
    pub mode: AgentMode,
    pub skills: Vec<String>,
    pub session_strategy: String,
    pub session_id: Option<String>,
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub last_task_id: Option<String>,
    pub created_at: String,
    /// 推送目标：{ channel_id, chat_id, label }
    pub notify: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleInput {
    pub name: String,
    pub prompt: String,
    pub cron: String,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub mode: Option<AgentMode>,
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    /// new（默认）：每次新建会话；same：写入同一个会话
    #[serde(default)]
    pub session_strategy: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 运行结果推送到的 IM 会话；不传或 null 表示不推送
    #[serde(default)]
    pub notify: Option<crate::im::ImTarget>,
}

struct Validated {
    name: String,
    prompt: String,
    cron: String,
    timezone: String,
    mode: AgentMode,
    skills: Vec<String>,
    session_strategy: String,
    next_run_at: Option<String>,
}

fn validate(input: &ScheduleInput, enabled: bool) -> Result<Validated, String> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err("名称不能为空，且不超过 100 个字".to_string());
    }
    let prompt = input.prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > 4000 {
        return Err("任务内容不能为空，且不超过 4000 字".to_string());
    }
    let mode = input.mode.unwrap_or(AgentMode::Craft);
    if mode == AgentMode::Plan {
        return Err("定时任务运行时没有人确认计划，只支持 Craft 或 Ask 模式".to_string());
    }
    let strategy = input.session_strategy.clone().unwrap_or_else(|| "new".to_string());
    if strategy != "new" && strategy != "same" {
        return Err("session_strategy 只能是 new 或 same".to_string());
    }
    let timezone = input.timezone.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| DEFAULT_TIMEZONE.to_string());
    let (cron, tz) = parse(&input.cron, &timezone)?;
    let next = enabled.then(|| next_runs(&cron, tz, Utc::now(), 1).first().map(|t| t.to_rfc3339())).flatten();
    Ok(Validated {
        name: name.to_string(),
        prompt: prompt.to_string(),
        cron: input.cron.trim().to_string(),
        timezone,
        mode,
        skills: input.skills.clone().unwrap_or_default(),
        session_strategy: strategy,
        next_run_at: next,
    })
}

const COLUMNS: &str = "id, name, prompt, cron, timezone, mode, skills, session_strategy, session_id, enabled, next_run_at, last_run_at, last_task_id, created_at";

fn row_to_schedule(row: &sqlx::any::AnyRow) -> AppResult<Schedule> {
    let cron = text_column(row, 3)?.unwrap_or_default();
    let mode: AgentMode = serde_json::from_value(json!(text_column(row, 5)?.unwrap_or_default())).unwrap_or(AgentMode::Craft);
    Ok(Schedule {
        id: row.try_get(0)?,
        name: text_column(row, 1)?.unwrap_or_default(),
        prompt: text_column(row, 2)?.unwrap_or_default(),
        description: describe(&cron),
        cron,
        timezone: text_column(row, 4)?.unwrap_or_default(),
        mode,
        skills: text_column(row, 6)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        session_strategy: text_column(row, 7)?.unwrap_or_default(),
        session_id: text_column(row, 8)?,
        enabled: row.try_get::<i64, _>(9)? != 0,
        next_run_at: text_column(row, 10)?,
        last_run_at: text_column(row, 11)?,
        last_task_id: text_column(row, 12)?,
        created_at: text_column(row, 13)?.unwrap_or_default(),
        notify: None,
    })
}

/// 补上推送目标
async fn with_notify(pool: &DbPool, mut schedule: Schedule) -> Schedule {
    schedule.notify = crate::im::schedule_target(pool, &schedule.id)
        .await
        .map(|(t, label)| json!({ "channel_id": t.channel_id, "chat_id": t.chat_id, "label": label }));
    schedule
}

pub async fn list(pool: &DbPool, owner_id: &str) -> AppResult<Vec<Schedule>> {
    let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM schedules WHERE owner_id = ? ORDER BY created_at DESC"))
        .bind(owner_id)
        .fetch_all(pool)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(with_notify(pool, row_to_schedule(row)?).await);
    }
    Ok(out)
}

pub async fn get(pool: &DbPool, owner_id: &str, id: &str) -> AppResult<Option<Schedule>> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM schedules WHERE id = ? AND owner_id = ?"))
        .bind(id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await?;
    match row.as_ref().map(row_to_schedule).transpose()? {
        Some(s) => Ok(Some(with_notify(pool, s).await)),
        None => Ok(None),
    }
}

pub async fn create(pool: &DbPool, owner_id: &str, input: &ScheduleInput) -> AppResult<Schedule> {
    let count: i64 = sqlx::query("SELECT COUNT(*) FROM schedules WHERE owner_id = ?")
        .bind(owner_id)
        .fetch_one(pool)
        .await?
        .try_get(0)?;
    if count >= MAX_SCHEDULES_PER_USER {
        return Err(bad(format!("最多创建 {MAX_SCHEDULES_PER_USER} 个定时任务")));
    }
    let enabled = input.enabled.unwrap_or(true);
    let v = validate(input, enabled).map_err(bad)?;
    crate::im::validate_target(pool, owner_id, input.notify.as_ref()).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO schedules (id, owner_id, name, prompt, cron, timezone, mode, skills, session_strategy, enabled, next_run_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(owner_id)
    .bind(&v.name)
    .bind(&v.prompt)
    .bind(&v.cron)
    .bind(&v.timezone)
    .bind(v.mode.as_str())
    .bind(serde_json::to_string(&v.skills)?)
    .bind(&v.session_strategy)
    .bind(enabled as i64)
    .bind(&v.next_run_at)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    crate::im::set_schedule_target(pool, owner_id, &id, input.notify.as_ref()).await?;
    Ok(get(pool, owner_id, &id).await?.expect("just inserted"))
}

pub async fn update(pool: &DbPool, owner_id: &str, id: &str, input: &ScheduleInput) -> AppResult<Schedule> {
    let current = get(pool, owner_id, id).await?.ok_or_else(|| AppError::NotFound("定时任务不存在".into()))?;
    let enabled = input.enabled.unwrap_or(current.enabled);
    let v = validate(input, enabled).map_err(bad)?;
    crate::im::validate_target(pool, owner_id, input.notify.as_ref()).await?;
    // 改为写入同一个会话时沿用已有会话；改回每次新建时清空
    let session_id = if v.session_strategy == "same" { current.session_id.clone() } else { None };
    sqlx::query(
        "UPDATE schedules SET name = ?, prompt = ?, cron = ?, timezone = ?, mode = ?, skills = ?, session_strategy = ?, session_id = ?, enabled = ?, next_run_at = ?, updated_at = ?
         WHERE id = ? AND owner_id = ?",
    )
    .bind(&v.name)
    .bind(&v.prompt)
    .bind(&v.cron)
    .bind(&v.timezone)
    .bind(v.mode.as_str())
    .bind(serde_json::to_string(&v.skills)?)
    .bind(&v.session_strategy)
    .bind(session_id)
    .bind(enabled as i64)
    .bind(&v.next_run_at)
    .bind(Utc::now().to_rfc3339())
    .bind(id)
    .bind(owner_id)
    .execute(pool)
    .await?;
    crate::im::set_schedule_target(pool, owner_id, id, input.notify.as_ref()).await?;
    Ok(get(pool, owner_id, id).await?.expect("exists"))
}

pub async fn delete(pool: &DbPool, owner_id: &str, id: &str) -> AppResult<bool> {
    let result = sqlx::query("DELETE FROM schedules WHERE id = ? AND owner_id = ?")
        .bind(id)
        .bind(owner_id)
        .execute(pool)
        .await?;
    if result.rows_affected() > 0 {
        sqlx::query("DELETE FROM schedule_runs WHERE schedule_id = ?").bind(id).execute(pool).await?;
        sqlx::query("DELETE FROM im_schedule_targets WHERE schedule_id = ?").bind(id).execute(pool).await?;
    }
    Ok(result.rows_affected() > 0)
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleRun {
    pub task_id: Option<String>,
    pub status: String,
    pub message: Option<String>,
    pub created_at: String,
}

pub async fn runs(pool: &DbPool, owner_id: &str, id: &str, limit: i64) -> AppResult<Vec<ScheduleRun>> {
    if get(pool, owner_id, id).await?.is_none() {
        return Err(AppError::NotFound("定时任务不存在".into()));
    }
    let rows = sqlx::query("SELECT task_id, status, message, created_at FROM schedule_runs WHERE schedule_id = ? ORDER BY created_at DESC LIMIT ?")
        .bind(id)
        .bind(limit)
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|r| {
            Ok(ScheduleRun {
                task_id: text_column(r, 0)?,
                status: text_column(r, 1)?.unwrap_or_default(),
                message: text_column(r, 2)?,
                created_at: text_column(r, 3)?.unwrap_or_default(),
            })
        })
        .collect()
}

async fn record_run(pool: &DbPool, schedule_id: &str, task_id: Option<&str>, status: &str, message: Option<&str>) {
    let _ = sqlx::query("INSERT INTO schedule_runs (id, schedule_id, task_id, status, message, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(schedule_id)
        .bind(task_id)
        .bind(status)
        .bind(message)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await;
}

// ───────────────────────── 触发 ─────────────────────────

/// 立即运行一次（调度到期或用户点“立即运行”）；上一次还没结束时跳过
pub async fn trigger(pool: &DbPool, owner_id: &str, schedule: &Schedule) -> AppResult<Value> {
    if let Some(last) = &schedule.last_task_id {
        if let Some((row, _)) = crate::tasks::get(pool, owner_id, last).await? {
            if row.live {
                record_run(pool, &schedule.id, None, "skipped", Some("上一次运行还没结束")).await;
                return Ok(json!({ "status": "skipped", "message": "上一次运行还没结束，本次跳过" }));
            }
        }
    }
    let local_now = Utc::now().with_timezone(&Tz::from_str(&schedule.timezone).unwrap_or(chrono_tz::Asia::Shanghai));
    let session_id = match (schedule.session_strategy.as_str(), &schedule.session_id) {
        ("same", Some(existing)) if session_repo::find_by_id(pool, existing).await?.is_some() => existing.clone(),
        _ => {
            let title = if schedule.session_strategy == "same" {
                format!("[定时] {}", schedule.name)
            } else {
                format!("[定时] {} · {}", schedule.name, local_now.format("%m-%d %H:%M"))
            };
            let session = session_repo::create(pool, owner_id, None, None, &title).await?;
            if schedule.session_strategy == "same" {
                sqlx::query("UPDATE schedules SET session_id = ? WHERE id = ?").bind(&session.id).bind(&schedule.id).execute(pool).await?;
            }
            session.id
        }
    };
    let req = ChatRequest {
        message: schedule.prompt.clone(),
        session_id: Some(session_id.clone()),
        mode: Some(schedule.mode),
        skills: (!schedule.skills.is_empty()).then(|| schedule.skills.clone()),
        timezone: Some(schedule.timezone.clone()),
        ..Default::default()
    };
    match crate::routes::chat::start_chat_task(owner_id, req, crate::tasks::Origin::Schedule).await {
        Ok(started) => {
            sqlx::query("UPDATE schedules SET last_run_at = ?, last_task_id = ? WHERE id = ?")
                .bind(Utc::now().to_rfc3339())
                .bind(&started.task_id)
                .bind(&schedule.id)
                .execute(pool)
                .await?;
            record_run(pool, &schedule.id, Some(&started.task_id), "started", None).await;
            // 设置了推送目标：把进度和结果发到那个 IM 会话
            if let Some((target, _)) = crate::im::schedule_target(pool, &schedule.id).await {
                tokio::spawn(crate::im::router::push_schedule_run(
                    target,
                    owner_id.to_string(),
                    schedule.name.clone(),
                    started.task_id.clone(),
                    session_id.clone(),
                ));
            }
            Ok(json!({ "status": "started", "task_id": started.task_id, "session_id": session_id }))
        }
        Err(e) => {
            record_run(pool, &schedule.id, None, "error", Some(&e.to_string())).await;
            Err(e)
        }
    }
}

/// 抢占一次触发：只有 next_run_at 仍等于读到的值时才推进，避免多实例或慢查询导致重复执行
pub async fn claim(pool: &DbPool, id: &str, expected: Option<&str>, next: Option<&str>) -> AppResult<bool> {
    let result = sqlx::query("UPDATE schedules SET next_run_at = ? WHERE id = ? AND next_run_at = ?")
        .bind(next)
        .bind(id)
        .bind(expected)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// 找出到期的定时任务：先推进 next_run_at（从当前时刻往后算，错过的不补跑多次），再触发
pub async fn run_due(pool: &DbPool, now: DateTime<Utc>) -> AppResult<usize> {
    let due = sqlx::query("SELECT owner_id, id FROM schedules WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?")
        .bind(now.to_rfc3339())
        .fetch_all(pool)
        .await?;
    let mut fired = 0;
    for row in due {
        let owner_id: String = row.try_get(0)?;
        let id: String = row.try_get(1)?;
        let Some(schedule) = get(pool, &owner_id, &id).await? else { continue };
        let next = parse(&schedule.cron, &schedule.timezone)
            .ok()
            .and_then(|(cron, tz)| next_runs(&cron, tz, now, 1).first().map(|t| t.to_rfc3339()));
        if !claim(pool, &id, schedule.next_run_at.as_deref(), next.as_deref()).await? {
            continue;
        }
        tracing::info!("[Schedule] 触发 {}（{}）", schedule.name, schedule.description);
        if let Err(e) = trigger(pool, &owner_id, &schedule).await {
            tracing::warn!("[Schedule] {} 触发失败: {e:?}", schedule.name);
        }
        fired += 1;
    }
    Ok(fired)
}

/// 启动调度循环
pub fn spawn_scheduler() {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(e) = run_due(&crate::state::db_pool(), Utc::now()).await {
                tracing::warn!("[Schedule] 调度检查失败: {e:?}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_validates_fields_timezone_and_interval() {
        assert!(parse("0 9 * * 1-5", "Asia/Shanghai").is_ok());
        assert!(parse("0 9 * *", "Asia/Shanghai").is_err(), "需要 5 段");
        assert!(parse("0 0 9 * * 1-5", "Asia/Shanghai").is_err(), "不接受带秒的 6 段");
        assert!(parse("0 9 * * 1-5", "Mars/Olympus").is_err());
        assert!(parse("*/5 * * * *", "UTC").is_err(), "间隔小于 10 分钟");
        assert!(parse("0 */2 * * *", "UTC").is_ok());
        assert!(parse("61 9 * * *", "UTC").is_err());
    }

    #[test]
    fn next_runs_respect_timezone() {
        let (cron, tz) = parse("0 9 * * 1-5", "Asia/Shanghai").unwrap();
        // 2026-10-16 是周五；上海时间 09:00 = UTC 01:00
        let after = DateTime::parse_from_rfc3339("2026-10-16T00:30:00Z").unwrap().with_timezone(&Utc);
        let runs: Vec<String> = next_runs(&cron, tz, after, 3).iter().map(|t| t.to_rfc3339()).collect();
        assert_eq!(runs, ["2026-10-16T01:00:00+00:00", "2026-10-19T01:00:00+00:00", "2026-10-20T01:00:00+00:00"]);
    }

    #[test]
    fn describes_common_patterns_in_chinese() {
        assert_eq!(describe("0 9 * * *"), "每天 09:00");
        assert_eq!(describe("30 17 * * 5"), "每周五 17:30");
        assert_eq!(describe("0 9 * * 1-5"), "每个工作日 09:00");
        assert_eq!(describe("0 10 * * 1,3"), "每周一、三 10:00");
        assert_eq!(describe("0 9 1 * *"), "每月 1 日 09:00");
        assert_eq!(describe("0 18 L * *"), "每月最后一天 18:00");
        assert_eq!(describe("0 */2 * * *"), "每 2 小时（第 0 分）");
        assert_eq!(describe("0 9 * 1 *"), "cron：0 9 * 1 *");
    }

    fn input(cron: &str) -> ScheduleInput {
        ScheduleInput {
            name: "周报".into(),
            prompt: "根据本周会议纪要生成周报".into(),
            cron: cron.into(),
            timezone: Some("Asia/Shanghai".into()),
            mode: None,
            skills: Some(vec!["weekly-report".into()]),
            session_strategy: None,
            enabled: None,
            notify: None,
        }
    }

    async fn memory_pool() -> DbPool {
        sqlx::any::install_default_drivers();
        let pool = sqlx::any::AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/007_schedules.sql")).execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/008_im.sql")).execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/009_im_push.sql")).execute(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn crud_validation_and_due_claiming() {
        let pool = memory_pool().await;
        let s = create(&pool, "u1", &input("30 17 * * 5")).await.unwrap();
        assert_eq!(s.description, "每周五 17:30");
        assert_eq!(s.mode, AgentMode::Craft);
        assert_eq!(s.skills, ["weekly-report"]);
        assert!(s.next_run_at.is_some());

        let mut plan = input("0 9 * * *");
        plan.mode = Some(AgentMode::Plan);
        assert!(create(&pool, "u1", &plan).await.is_err(), "不支持 Plan 模式");

        // 停用后不再计算下次时间
        let mut off = input("30 17 * * 5");
        off.enabled = Some(false);
        let updated = update(&pool, "u1", &s.id, &off).await.unwrap();
        assert!(!updated.enabled && updated.next_run_at.is_none());
        assert!(update(&pool, "u2", &s.id, &off).await.is_err(), "不能改别人的");

        // 到期抢占：同一次触发只能被抢占一次
        update(&pool, "u1", &s.id, &input("30 17 * * 5")).await.unwrap();
        let past = "2000-01-01T00:00:00+00:00";
        sqlx::query("UPDATE schedules SET next_run_at = ? WHERE id = ?").bind(past).bind(&s.id).execute(&pool).await.unwrap();
        assert!(claim(&pool, &s.id, Some(past), Some("2099-01-01T00:00:00+00:00")).await.unwrap());
        assert!(!claim(&pool, &s.id, Some(past), Some("2099-01-02T00:00:00+00:00")).await.unwrap());
        assert_eq!(get(&pool, "u1", &s.id).await.unwrap().unwrap().next_run_at.as_deref(), Some("2099-01-01T00:00:00+00:00"));

        assert!(delete(&pool, "u1", &s.id).await.unwrap());
        assert!(list(&pool, "u1").await.unwrap().is_empty());
    }
}
