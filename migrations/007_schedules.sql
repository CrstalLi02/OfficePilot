-- 定时任务：按 cron 表达式（在指定时区）定期发起一次 Agent 运行
CREATE TABLE IF NOT EXISTS schedules (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    name TEXT NOT NULL,
    prompt TEXT NOT NULL,
    cron TEXT NOT NULL,
    timezone TEXT NOT NULL,
    -- craft | ask（没有人在场确认计划，不支持 plan）
    mode TEXT NOT NULL,
    -- JSON 数组：每次运行显式使用的技能
    skills TEXT,
    -- new：每次新建会话；same：写入同一个会话
    session_strategy TEXT NOT NULL,
    session_id TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    next_run_at TEXT,
    last_run_at TEXT,
    last_task_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_schedules_due ON schedules(enabled, next_run_at);

-- 每次触发的记录（启动的任务、因上次未结束而跳过、出错）
CREATE TABLE IF NOT EXISTS schedule_runs (
    id TEXT PRIMARY KEY,
    schedule_id TEXT NOT NULL,
    task_id TEXT,
    -- started | skipped | error
    status TEXT NOT NULL,
    message TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_schedule_runs_schedule ON schedule_runs(schedule_id, created_at);
