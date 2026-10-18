-- MCP 插件：用户接入的 MCP 服务器（请求头 / 环境变量的值加密存储在 secrets）
CREATE TABLE IF NOT EXISTS mcp_servers (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    slug TEXT NOT NULL,
    name TEXT NOT NULL,
    transport TEXT NOT NULL,
    url TEXT,
    command TEXT,
    args TEXT,
    config TEXT NOT NULL,
    secrets TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (owner_id, slug)
);
CREATE INDEX IF NOT EXISTS idx_mcp_servers_owner ON mcp_servers(owner_id);
