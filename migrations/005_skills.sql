-- 用户对技能的启用 / 停用设置（技能内容来自内置目录或用户导入的文件夹）
CREATE TABLE IF NOT EXISTS skill_settings (
    owner_id TEXT NOT NULL,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (owner_id, name)
);
