-- Plan 模式下等待用户确认的 Agent 运行（保存完整上下文，确认后从中断处恢复）
CREATE TABLE IF NOT EXISTS agent_runs (
    id VARCHAR(36) PRIMARY KEY,
    session_id VARCHAR(36) NOT NULL,
    owner_id VARCHAR(36) NOT NULL,
    -- awaiting_approval | approved | revised | rejected
    status VARCHAR(32) NOT NULL,
    plan MEDIUMTEXT NOT NULL,
    conversation LONGTEXT NOT NULL,
    plan_call_id VARCHAR(255) NOT NULL,
    other_call_ids TEXT NOT NULL,
    allowed_tools TEXT,
    created_at VARCHAR(50) NOT NULL,
    updated_at VARCHAR(50) NOT NULL,
    INDEX idx_agent_runs_session (session_id, status),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
