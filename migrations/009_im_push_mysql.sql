-- IM 会话信息（私聊 / 群聊、群名）与定时任务结果的推送目标
CREATE TABLE IF NOT EXISTS im_chats (
    channel_id VARCHAR(36) NOT NULL,
    chat_id VARCHAR(128) NOT NULL,
    chat_type VARCHAR(16) NOT NULL,
    name VARCHAR(200),
    updated_at VARCHAR(50) NOT NULL,
    PRIMARY KEY (channel_id, chat_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE IF NOT EXISTS im_schedule_targets (
    schedule_id VARCHAR(36) PRIMARY KEY,
    channel_id VARCHAR(36) NOT NULL,
    chat_id VARCHAR(128) NOT NULL,
    created_at VARCHAR(50) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
