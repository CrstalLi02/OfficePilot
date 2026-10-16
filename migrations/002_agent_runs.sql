-- Plan 模式下等待用户确认的 Agent 运行（保存完整上下文，确认后从中断处恢复）
CREATE TABLE IF NOT EXISTS agent_runs (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    -- awaiting_approval | approved | revised | rejected
    status TEXT NOT NULL,
    plan TEXT NOT NULL,
    conversation TEXT NOT NULL,
    plan_call_id TEXT NOT NULL,
    other_call_ids TEXT NOT NULL,
    allowed_tools TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_agent_runs_session ON agent_runs(session_id, status);
