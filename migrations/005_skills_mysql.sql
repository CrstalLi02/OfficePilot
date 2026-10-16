-- 用户对技能的启用 / 停用设置（技能内容来自内置目录或用户导入的文件夹）
CREATE TABLE IF NOT EXISTS skill_settings (
    owner_id VARCHAR(36) NOT NULL,
    name VARCHAR(64) NOT NULL,
    enabled INT NOT NULL DEFAULT 1,
    updated_at VARCHAR(50) NOT NULL,
    PRIMARY KEY (owner_id, name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
