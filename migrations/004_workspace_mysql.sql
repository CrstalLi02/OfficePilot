-- Agent 对工作区的写入 / 整理记录，保存逆操作用于撤销
CREATE TABLE IF NOT EXISTS workspace_ops (
    id VARCHAR(36) PRIMARY KEY,
    owner_id VARCHAR(36) NOT NULL,
    session_id VARCHAR(36),
    summary VARCHAR(1000) NOT NULL,
    -- 逆操作列表（JSON），按顺序执行即可撤销
    inverse MEDIUMTEXT NOT NULL,
    created_at VARCHAR(50) NOT NULL,
    undone_at VARCHAR(50),
    INDEX idx_workspace_ops_owner (owner_id, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- 文件提取文本缓存（全文搜索、读取复用）
CREATE TABLE IF NOT EXISTS file_texts (
    file_id VARCHAR(36) PRIMARY KEY,
    owner_id VARCHAR(36) NOT NULL,
    text LONGTEXT NOT NULL,
    parser VARCHAR(50) NOT NULL,
    extracted_at VARCHAR(50) NOT NULL,
    INDEX idx_file_texts_owner (owner_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
