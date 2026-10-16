//! 工作模式（Craft / Plan / Ask）与计划确认。
//!
//! - Craft：直接执行，开放全部生成类工具
//! - Plan：只开放 `propose_plan`，模型先提交计划；用户确认后再以 Craft 工具集继续执行
//! - Ask：只开放只读工具（如联网搜索），只回答问题、不生成产物

use serde::{Deserialize, Serialize};

pub const PROPOSE_PLAN: &str = "propose_plan";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    #[default]
    Craft,
    Plan,
    Ask,
}

impl AgentMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentMode::Craft => "craft",
            AgentMode::Plan => "plan",
            AgentMode::Ask => "ask",
        }
    }

    /// 该模式下是否暴露某个工具
    pub fn allows(&self, tool_name: &str, read_only: bool) -> bool {
        match self {
            AgentMode::Craft => tool_name != PROPOSE_PLAN,
            AgentMode::Plan => tool_name == PROPOSE_PLAN,
            AgentMode::Ask => read_only && tool_name != PROPOSE_PLAN,
        }
    }

    /// 追加到系统提示词的模式说明
    pub fn prompt(&self) -> &'static str {
        match self {
            AgentMode::Craft => "## 当前模式：Craft（直接执行）\n需求清楚时直接调用工具交付产物；缺少的少量信息做合理假设，不要反复确认。",
            AgentMode::Plan => "## 当前模式：Plan（先规划，用户确认后执行）\n凡是需要生成产物（文档、PPT、表格、图表、图片、视频等）或需要多个步骤的请求，必须先调用 propose_plan 提交执行计划，此时不要输出最终内容；用户确认后系统会让你继续执行。纯问答、闲聊直接用文字回答，不需要计划。",
            AgentMode::Ask => "## 当前模式：Ask（只问答）\n只用文字回答、解释和建议，不生成任何文件或产物；需要外部最新信息时可以联网搜索。如果用户想要生成产物，告诉他切换到 Craft 或 Plan 模式。",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanStep {
    #[serde(default)]
    pub id: String,
    /// 计划调用的工具名
    pub tool: String,
    /// 这一步要做什么
    #[serde(default)]
    pub purpose: String,
    /// 交付物（可选）
    #[serde(default)]
    pub deliverable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    #[serde(default)]
    pub goal: String,
    pub steps: Vec<PlanStep>,
    #[serde(default)]
    pub notes: String,
}

impl Plan {
    /// 解析 propose_plan 的参数，补齐缺失的步骤 id
    pub fn from_arguments(arguments: &str) -> Result<Self, String> {
        let mut plan: Plan = serde_json::from_str(arguments)
            .map_err(|e| format!("计划参数无法解析: {e}"))?;
        plan.steps.retain(|s| !s.tool.trim().is_empty());
        if plan.steps.is_empty() {
            return Err("计划至少需要一个步骤".to_string());
        }
        for (i, step) in plan.steps.iter_mut().enumerate() {
            if step.id.trim().is_empty() {
                step.id = format!("s{}", i + 1);
            }
        }
        Ok(plan)
    }

    /// 写入会话历史的文字版计划，后续轮次（例如"按刚才的计划改一下"）能看到
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("**执行计划**");
        if !self.goal.trim().is_empty() {
            out.push_str(&format!("：{}", self.goal.trim()));
        }
        for (i, step) in self.steps.iter().enumerate() {
            out.push_str(&format!("\n{}. `{}` {}", i + 1, step.tool, step.purpose.trim()));
            if !step.deliverable.trim().is_empty() {
                out.push_str(&format!("（交付：{}）", step.deliverable.trim()));
            }
        }
        if !self.notes.trim().is_empty() {
            out.push_str(&format!("\n\n{}", self.notes.trim()));
        }
        out
    }
}

/// 用户对计划的决定
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// 按（可能被编辑过的）计划执行
    Approve,
    /// 根据反馈重新规划
    Revise,
    /// 放弃
    Reject,
}

/// 前端提交的计划确认结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDecision {
    pub run_id: String,
    pub decision: Decision,
    /// 用户编辑后的步骤（approve 时可选，不传则沿用原计划）
    #[serde(default)]
    pub steps: Option<Vec<PlanStep>>,
    #[serde(default)]
    pub feedback: Option<String>,
}

impl PlanDecision {
    /// 作为 propose_plan 的工具结果写回上下文，让模型据此继续
    pub fn tool_result(&self, plan: &Plan) -> String {
        let feedback = self.feedback.as_deref().map(str::trim).filter(|f| !f.is_empty());
        let value = match self.decision {
            Decision::Approve => serde_json::json!({
                "approved": true,
                "steps": self.steps.as_ref().unwrap_or(&plan.steps),
                "feedback": feedback,
                "instruction": "用户已确认计划。现在严格按照 steps 依次调用对应工具执行（互不依赖的步骤可以在同一轮并行调用），不要再调用 propose_plan；用户的补充意见优先。全部完成后简要总结交付了哪些产物。",
            }),
            Decision::Revise => serde_json::json!({
                "approved": false,
                "feedback": feedback,
                "instruction": "用户希望调整计划。请根据反馈重新调用 propose_plan 提交修改后的计划。",
            }),
            Decision::Reject => serde_json::json!({
                "approved": false,
                "instruction": "用户取消了这个计划。简短确认即可，不要调用任何工具。",
            }),
        };
        value.to_string()
    }

    /// 写入会话历史的用户消息
    pub fn user_message(&self) -> String {
        let base = match self.decision {
            Decision::Approve => "确认执行计划",
            Decision::Revise => "修改计划",
            Decision::Reject => "取消计划",
        };
        match self.feedback.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
            Some(feedback) => format!("{base}：{feedback}"),
            None => base.to_string(),
        }
    }

    /// 恢复执行时采用的模式：确认 → Craft 执行；修改 → 继续 Plan；取消 → Ask（不再调用生成工具）
    pub fn next_mode(&self) -> AgentMode {
        match self.decision {
            Decision::Approve => AgentMode::Craft,
            Decision::Revise => AgentMode::Plan,
            Decision::Reject => AgentMode::Ask,
        }
    }

    /// 执行时跟踪进度用的步骤
    pub fn effective_steps(&self, plan: &Plan) -> Vec<PlanStep> {
        match (self.decision, &self.steps) {
            (Decision::Approve, Some(steps)) if !steps.is_empty() => steps.clone(),
            (Decision::Approve, _) => plan.steps.clone(),
            _ => vec![],
        }
    }
}

/// 计划执行进度：工具成功/失败时标记对应的第一个未完成步骤
#[derive(Debug, Clone, Default)]
pub struct PlanTracker {
    steps: Vec<PlanStep>,
    done: Vec<Option<bool>>,
}

impl PlanTracker {
    pub fn new(steps: Vec<PlanStep>) -> Self {
        let done = vec![None; steps.len()];
        Self { steps, done }
    }

    /// 返回被更新的步骤 id
    pub fn record(&mut self, tool: &str, success: bool) -> Option<String> {
        let index = self
            .steps
            .iter()
            .zip(&self.done)
            .position(|(step, state)| state.is_none() && step.tool == tool)?;
        self.done[index] = Some(success);
        Some(self.steps[index].id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_tool_visibility() {
        assert!(AgentMode::Craft.allows("doc_generate", false));
        assert!(!AgentMode::Craft.allows(PROPOSE_PLAN, false));
        assert!(AgentMode::Plan.allows(PROPOSE_PLAN, false));
        assert!(!AgentMode::Plan.allows("doc_generate", false));
        assert!(AgentMode::Ask.allows("web_search", true));
        assert!(!AgentMode::Ask.allows("doc_generate", false));
    }

    #[test]
    fn plan_parsing_fills_ids_and_rejects_empty() {
        let plan = Plan::from_arguments(
            r#"{"goal":"g","steps":[{"tool":"ppt_plan","purpose":"大纲"},{"id":"x","tool":"drawio_generate"},{"tool":" "}]}"#,
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].id, "s1");
        assert_eq!(plan.steps[1].id, "x");
        assert!(plan.to_markdown().contains("1. `ppt_plan` 大纲"));
        assert!(Plan::from_arguments(r#"{"steps":[]}"#).is_err());
        assert!(Plan::from_arguments("not json").is_err());
    }

    #[test]
    fn decision_messages_and_steps() {
        let plan = Plan::from_arguments(r#"{"steps":[{"tool":"a"},{"tool":"b"}]}"#).unwrap();
        let approve = PlanDecision {
            run_id: "r".into(),
            decision: Decision::Approve,
            steps: Some(vec![plan.steps[1].clone()]),
            feedback: Some(" 用英文 ".into()),
        };
        assert_eq!(approve.user_message(), "确认执行计划：用英文");
        assert_eq!(approve.next_mode(), AgentMode::Craft);
        assert_eq!(approve.effective_steps(&plan).len(), 1);
        let result: serde_json::Value = serde_json::from_str(&approve.tool_result(&plan)).unwrap();
        assert_eq!(result["approved"], true);
        assert_eq!(result["steps"].as_array().unwrap().len(), 1);

        let reject = PlanDecision { run_id: "r".into(), decision: Decision::Reject, steps: None, feedback: None };
        assert_eq!(reject.next_mode(), AgentMode::Ask);
        assert!(reject.effective_steps(&plan).is_empty());
    }

    #[test]
    fn tracker_marks_first_pending_matching_step() {
        let plan = Plan::from_arguments(r#"{"steps":[{"tool":"md_generate"},{"tool":"md_generate"},{"tool":"chart_generate"}]}"#).unwrap();
        let mut tracker = PlanTracker::new(plan.steps);
        assert_eq!(tracker.record("md_generate", true).as_deref(), Some("s1"));
        assert_eq!(tracker.record("md_generate", false).as_deref(), Some("s2"));
        assert_eq!(tracker.record("md_generate", true), None);
        assert_eq!(tracker.record("chart_generate", true).as_deref(), Some("s3"));
    }
}
