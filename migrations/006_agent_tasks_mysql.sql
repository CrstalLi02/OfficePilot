-- 后台任务：每次 Agent 运行一条记录（来自对话、定时任务或 IM），与 HTTP 连接解耦
CREATE TABLE IF NOT EXISTS agent_tasks (
    id VARCHAR(36) PRIMARY KEY,
    owner_id VARCHAR(36) NOT NULL,
    session_id VARCHAR(36) NOT NULL,
    origin VARCHAR(20) NOT NULL,
    title VARCHAR(500) NOT NULL,
    status VARCHAR(32) NOT NULL,
    request MEDIUMTEXT NOT NULL,
    summary VARCHAR(2000),
    error VARCHAR(2000),
    created_at VARCHAR(50) NOT NULL,
    started_at VARCHAR(50),
    finished_at VARCHAR(50),
    INDEX idx_agent_tasks_owner (owner_id, created_at),
    INDEX idx_agent_tasks_status (status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
