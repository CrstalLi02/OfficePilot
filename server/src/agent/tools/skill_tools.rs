//! 技能工具：use_skill（加载技能说明）、read_skill_file（读取技能附带的模板 / 参考资料）

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::tool::{OfficeTool, ToolContext, ToolResult};
use crate::skills;

async fn find_enabled(ctx: &ToolContext, name: &str) -> Result<skills::Skill, String> {
    let all = skills::load_all(&crate::state::db_pool(), &ctx.user_id).await.map_err(|e| e.to_string())?;
    match all.into_iter().find(|s| s.meta.name == name) {
        Some(s) if s.enabled => Ok(s),
        Some(_) => Err(format!("技能 {name} 已被用户停用")),
        None => Err(format!("没有名为 {name} 的技能；请使用系统提示中“可用技能”列出的名称")),
    }
}

pub struct UseSkillTool;

#[async_trait]
impl OfficeTool for UseSkillTool {
    fn name(&self) -> &str {
        "use_skill"
    }
    fn description(&self) -> &str {
        "加载一个技能的完整说明（流程、标准、模板清单）。当任务与系统提示中“可用技能”列出的某个技能匹配时，先调用它，再按说明执行。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn produces_artifact(&self) -> bool {
        false
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "技能名称，例如 weekly-report" } },
            "required": ["name"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let name = input["name"].as_str().unwrap_or("").trim();
        match find_enabled(ctx, name).await {
            Ok(skill) => ToolResult::ok(skills::render_body(&skill), vec![]).with_data(json!({
                "name": skill.meta.name,
                "source": skill.source,
                "files": skill.files,
                "summary": format!("已加载技能 {}", skill.meta.name),
            })),
            Err(e) => ToolResult::err(e),
        }
    }
}

pub struct ReadSkillFileTool;

#[async_trait]
impl OfficeTool for ReadSkillFileTool {
    fn name(&self) -> &str {
        "read_skill_file"
    }
    fn description(&self) -> &str {
        "读取技能附带的文件（模板、参考资料等），path 用 use_skill 返回的相对路径，例如 assets/template.md。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn produces_artifact(&self) -> bool {
        false
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "技能名称" },
                "path": { "type": "string", "description": "技能内的相对路径" }
            },
            "required": ["name", "path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let name = input["name"].as_str().unwrap_or("").trim();
        let path = input["path"].as_str().unwrap_or("");
        let skill = match find_enabled(ctx, name).await {
            Ok(s) => s,
            Err(e) => return ToolResult::err(e),
        };
        match skills::read_file(&ctx.user_id, &skill, path) {
            Ok(text) => ToolResult::ok(format!("技能 {name} 的文件 {path}：\n<<<FILE\n{text}\nFILE>>>"), vec![])
                .with_data(json!({ "name": name, "path": path, "summary": format!("已读取 {path}") })),
            Err(e) => ToolResult::err(e),
        }
    }
}
