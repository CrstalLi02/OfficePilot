-- 产物版本历史：每次生成、AI 编辑、用户手动修改或恢复都追加一个版本
CREATE TABLE IF NOT EXISTS artifact_versions (
    artifact_id VARCHAR(36) NOT NULL,
    version INT NOT NULL,
    session_id VARCHAR(36) NOT NULL,
    kind VARCHAR(50) NOT NULL,
    title VARCHAR(500) NOT NULL,
    content LONGTEXT NOT NULL,
    change_summary VARCHAR(1000),
    -- agent | user
    author VARCHAR(20) NOT NULL,
    created_at VARCHAR(50) NOT NULL,
    PRIMARY KEY (artifact_id, version),
    INDEX idx_artifact_versions_session (session_id),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
