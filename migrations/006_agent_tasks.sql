-- 后台任务：每次 Agent 运行一条记录（来自对话、定时任务或 IM），与 HTTP 连接解耦
CREATE TABLE IF NOT EXISTS agent_tasks (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    -- chat | schedule | im
    origin TEXT NOT NULL,
    title TEXT NOT NULL,
    -- queued | running | succeeded | failed | cancelled | awaiting_approval | interrupted
    status TEXT NOT NULL,
    -- 发起时的请求（JSON，图片附件的 data_url 不保存），用于重试
    request TEXT NOT NULL,
    summary TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_agent_tasks_owner ON agent_tasks(owner_id, created_at);
CREATE INDEX IF NOT EXISTS idx_agent_tasks_status ON agent_tasks(status);
