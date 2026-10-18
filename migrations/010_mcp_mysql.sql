-- MCP 插件：用户接入的 MCP 服务器（请求头 / 环境变量的值加密存储在 secrets）
CREATE TABLE IF NOT EXISTS mcp_servers (
    id VARCHAR(36) PRIMARY KEY,
    owner_id VARCHAR(36) NOT NULL,
    slug VARCHAR(32) NOT NULL,
    name VARCHAR(100) NOT NULL,
    transport VARCHAR(16) NOT NULL,
    url VARCHAR(1000),
    command VARCHAR(500),
    args TEXT,
    config TEXT NOT NULL,
    secrets TEXT NOT NULL,
    enabled INT NOT NULL DEFAULT 1,
    created_at VARCHAR(50) NOT NULL,
    updated_at VARCHAR(50) NOT NULL,
    UNIQUE KEY uk_mcp_servers_slug (owner_id, slug),
    INDEX idx_mcp_servers_owner (owner_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
