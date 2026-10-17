-- IM 远程控制：机器人渠道、已绑定的 IM 账号、IM 会话与系统会话的对应关系、事件去重
CREATE TABLE IF NOT EXISTS im_channels (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL,
    platform TEXT NOT NULL,
    name TEXT NOT NULL,
    config TEXT NOT NULL,
    secrets TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_im_channels_owner ON im_channels(owner_id);

CREATE TABLE IF NOT EXISTS im_bindings (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    im_user_id TEXT NOT NULL,
    display_name TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (channel_id, im_user_id)
);

CREATE TABLE IF NOT EXISTS im_conversations (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    session_id TEXT,
    mode TEXT NOT NULL,
    pending_run_id TEXT,
    updated_at TEXT NOT NULL,
    UNIQUE (channel_id, chat_id)
);

CREATE TABLE IF NOT EXISTS im_events (
    channel_id TEXT NOT NULL,
    event_key TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (channel_id, event_key)
);
