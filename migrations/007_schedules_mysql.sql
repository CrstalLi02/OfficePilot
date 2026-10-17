-- 定时任务：按 cron 表达式（在指定时区）定期发起一次 Agent 运行
CREATE TABLE IF NOT EXISTS schedules (
    id VARCHAR(36) PRIMARY KEY,
    owner_id VARCHAR(36) NOT NULL,
    name VARCHAR(200) NOT NULL,
    prompt TEXT NOT NULL,
    cron VARCHAR(100) NOT NULL,
    timezone VARCHAR(64) NOT NULL,
    mode VARCHAR(16) NOT NULL,
    skills TEXT,
    session_strategy VARCHAR(16) NOT NULL,
    session_id VARCHAR(36),
    enabled INT NOT NULL DEFAULT 1,
    next_run_at VARCHAR(50),
    last_run_at VARCHAR(50),
    last_task_id VARCHAR(36),
    created_at VARCHAR(50) NOT NULL,
    updated_at VARCHAR(50) NOT NULL,
    INDEX idx_schedules_due (enabled, next_run_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS schedule_runs (
    id VARCHAR(36) PRIMARY KEY,
    schedule_id VARCHAR(36) NOT NULL,
    task_id VARCHAR(36),
    status VARCHAR(16) NOT NULL,
    message VARCHAR(1000),
    created_at VARCHAR(50) NOT NULL,
    INDEX idx_schedule_runs_schedule (schedule_id, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
