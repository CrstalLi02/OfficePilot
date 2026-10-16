-- Agent 对工作区的写入 / 整理记录，保存逆操作用于撤销
CREATE TABLE IF NOT EXISTS workspace_ops (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    session_id TEXT,
    summary TEXT NOT NULL,
    -- 逆操作列表（JSON），按顺序执行即可撤销
    inverse TEXT NOT NULL,
    created_at TEXT NOT NULL,
    undone_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_workspace_ops_owner ON workspace_ops(owner_id, created_at);

-- 文件提取文本缓存（全文搜索、读取复用）
CREATE TABLE IF NOT EXISTS file_texts (
    file_id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    text TEXT NOT NULL,
    parser TEXT NOT NULL,
    extracted_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_file_texts_owner ON file_texts(owner_id);
