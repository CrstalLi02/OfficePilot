-- IM 远程控制：机器人渠道、已绑定的 IM 账号、IM 会话与系统会话的对应关系、事件去重
CREATE TABLE IF NOT EXISTS im_channels (
    id VARCHAR(36) PRIMARY KEY,
    owner_id VARCHAR(36) NOT NULL,
    platform VARCHAR(16) NOT NULL,
    name VARCHAR(100) NOT NULL,
    config TEXT NOT NULL,
    secrets TEXT NOT NULL,
    enabled INT NOT NULL DEFAULT 1,
    created_at VARCHAR(50) NOT NULL,
    updated_at VARCHAR(50) NOT NULL,
    INDEX idx_im_channels_owner (owner_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS im_bindings (
    id VARCHAR(36) PRIMARY KEY,
    channel_id VARCHAR(36) NOT NULL,
    im_user_id VARCHAR(128) NOT NULL,
    display_name VARCHAR(200),
    created_at VARCHAR(50) NOT NULL,
    UNIQUE KEY uk_im_bindings (channel_id, im_user_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS im_conversations (
    id VARCHAR(36) PRIMARY KEY,
    channel_id VARCHAR(36) NOT NULL,
    chat_id VARCHAR(128) NOT NULL,
    session_id VARCHAR(36),
    mode VARCHAR(16) NOT NULL,
    pending_run_id VARCHAR(36),
    updated_at VARCHAR(50) NOT NULL,
    UNIQUE KEY uk_im_conversations (channel_id, chat_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS im_events (
    channel_id VARCHAR(36) NOT NULL,
    event_key VARCHAR(128) NOT NULL,
    created_at VARCHAR(50) NOT NULL,
    PRIMARY KEY (channel_id, event_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
