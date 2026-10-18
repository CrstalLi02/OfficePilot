-- IM 会话信息（私聊 / 群聊、群名）与定时任务结果的推送目标
CREATE TABLE IF NOT EXISTS im_chats (
    channel_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    chat_type TEXT NOT NULL,
    name TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (channel_id, chat_id)
);

CREATE TABLE IF NOT EXISTS im_schedule_targets (
    schedule_id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    created_at TEXT NOT NULL
);
