use async_trait::async_trait;
use serde_json::json;

use crate::agent::plan::PROPOSE_PLAN;
use crate::agent::tool::{OfficeTool, ToolContext, ToolResult};

/// 提交执行计划，等待用户确认（Plan 模式）。由 agent 循环拦截处理，不会真正执行
pub struct ProposePlanTool;

#[async_trait]
impl OfficeTool for ProposePlanTool {
    /// 会暂停本次运行，必须单独处理
    fn parallel_safe(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        PROPOSE_PLAN
    }

    fn description(&self) -> &str {
        "提交执行计划并等待用户确认。列出要调用的工具、每一步的目的和交付物；用户确认（可能会删改步骤或补充意见）后你会收到结果再继续执行。计划要具体到工具，步骤数与用户要的交付物一一对应，不要添加用户没要的产物。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "goal": { "type": "string", "description": "一句话概括要完成的目标" },
                "steps": {
                    "type": "array",
                    "description": "按执行顺序排列的步骤",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tool": { "type": "string", "description": "这一步调用的工具名，例如 ppt_plan、doc_generate、web_search" },
                            "purpose": { "type": "string", "description": "这一步要做什么（面向用户的简短说明）" },
                            "deliverable": { "type": "string", "description": "这一步的交付物，例如“8 页汇报 PPT”；搜索、规划类步骤可留空" }
                        },
                        "required": ["tool", "purpose"]
                    },
                    "minItems": 1
                },
                "notes": { "type": "string", "description": "需要用户注意或确认的假设（可选），例如默认受众、篇幅、风格" }
            },
            "required": ["goal", "steps"]
        })
    }

    async fn call(&self, _input: serde_json::Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::err("propose_plan 只能在 Plan 模式下由系统处理")
    }
}
