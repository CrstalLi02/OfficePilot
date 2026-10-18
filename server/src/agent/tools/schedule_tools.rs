//! 定时任务工具：用自然语言创建（模型把“每周五下午五点”换算成 cron）、查看已有定时任务

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};

use crate::agent::tool::{OfficeTool, ToolContext, ToolResult};
use crate::schedules::{self, ScheduleInput};

pub struct ScheduleCreateTool;

#[async_trait]
impl OfficeTool for ScheduleCreateTool {
    fn name(&self) -> &str {
        "schedule_create"
    }
    fn description(&self) -> &str {
        "创建定时任务：按 cron 时间定期自动执行一段任务说明（例如每周五 17:00 根据本周会议纪要写周报并保存到文件）。用户说“每天 / 每周 / 每月……帮我……”“定时”“到点提醒我生成……”时使用。prompt 要写成完整、可以独立执行的任务说明（包括交付物、保存位置），因为运行时不会带上当前对话。遇到这类周期性请求直接调用本工具，不要先 use_skill 或现在就生成内容；需要的技能填进 skills 参数，到点运行时再加载。只创建，不会立即执行；创建后告诉用户名称、触发规则和下一次运行时间。"
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    fn produces_artifact(&self) -> bool {
        false
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "简短名称，例如“每周周报”" },
                "prompt": { "type": "string", "description": "每次运行时交给 Agent 的完整任务说明" },
                "cron": { "type": "string", "description": "5 段 cron（分 时 日 月 周），按用户时区理解。例：0 9 * * 1-5 工作日 9:00；30 17 * * 5 每周五 17:30；0 9 1 * * 每月 1 日 9:00；0 18 L * * 每月最后一天 18:00。最小间隔 10 分钟" },
                "mode": { "type": "string", "enum": ["craft", "ask"], "description": "craft：生成产物（默认）；ask：只回答（例如每天早上汇总新闻）" },
                "skills": { "type": "array", "items": { "type": "string" }, "description": "每次运行显式使用的技能（可选），例如 [\"weekly-report\"]" },
                "session_strategy": { "type": "string", "enum": ["new", "same"], "description": "new：每次新建一个会话（默认）；same：都写进同一个会话，方便连续追踪" }
            },
            "required": ["name", "prompt", "cron"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let mut schedule_input: ScheduleInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("参数不正确：{e}")),
        };
        schedule_input.timezone = ctx.timezone.clone().or(schedule_input.timezone);
        // 推送目标只取自服务端：在飞书里创建的定时任务，结果推回这个飞书会话
        schedule_input.notify = ctx.im_target.clone();
        let pushes_to_im = schedule_input.notify.is_some();
        match schedules::create(&crate::state::db_pool(), &ctx.user_id, &schedule_input).await {
            Ok(s) => {
                let next: Vec<String> = schedules::parse(&s.cron, &s.timezone)
                    .map(|(cron, tz)| {
                        schedules::next_runs(&cron, tz, Utc::now(), 3)
                            .iter()
                            .map(|t| t.with_timezone(&tz).format("%m-%d %H:%M").to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                ToolResult::ok(
                    format!(
                        "已创建定时任务《{}》：{}（时区 {}），接下来三次运行：{}。{}用户可以在“后台任务 → 定时任务”里停用、修改或立即运行。",
                        s.name,
                        s.description,
                        s.timezone,
                        next.join("、"),
                        if pushes_to_im { "每次运行的进度和结果会推送到当前这个飞书会话。" } else { "" }
                    ),
                    vec![],
                )
                .with_data(json!({ "schedule_id": s.id, "description": s.description, "next_runs": next, "summary": format!("已创建：{}", s.description) }))
            }
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

pub struct ScheduleListTool;

#[async_trait]
impl OfficeTool for ScheduleListTool {
    fn name(&self) -> &str {
        "schedule_list"
    }
    fn description(&self) -> &str {
        "查看用户已有的定时任务（名称、触发规则、是否启用、下次运行时间）。用户问“我有哪些定时任务”或创建前需要避免重复时使用。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn produces_artifact(&self) -> bool {
        false
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn call(&self, _input: Value, ctx: &ToolContext) -> ToolResult {
        match schedules::list(&crate::state::db_pool(), &ctx.user_id).await {
            Ok(list) if list.is_empty() => ToolResult::ok("还没有定时任务", vec![]),
            Ok(list) => {
                let lines: Vec<String> = list
                    .iter()
                    .map(|s| {
                        format!(
                            "- 《{}》{}，{}，下次：{}",
                            s.name,
                            s.description,
                            if s.enabled { "已启用" } else { "已停用" },
                            s.next_run_at.as_deref().unwrap_or("—")
                        )
                    })
                    .collect();
                ToolResult::ok(lines.join("\n"), vec![]).with_data(json!({ "count": list.len() }))
            }
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}
