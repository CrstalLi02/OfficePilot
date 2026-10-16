use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::models::{Artifact, ChatAttachment};

/// 工具执行上下文
#[derive(Clone)]
pub struct ToolContext {
    pub session_id: String,
    pub user_id: String,
    pub project_id: Option<String>,
    pub preferred_model: Option<String>,
    pub attachments: Vec<ChatAttachment>,
    /// 当前会话中已有的产物历史（用于跨工具引用，如视频工具引用之前的图片产物）
    pub prior_artifacts: Vec<Artifact>,
    /// SSE 推送回调（向前端发送实时进度）
    pub emit: Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>,
    /// 共享上下文（跨工具传递，如 PPT 大纲规划）
    pub scratchpad: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    /// 用户工具配置（前端传入，如视频时长、宽高比等）
    pub tool_config: Option<serde_json::Value>,
    /// 当前工具调用的 id；并行执行时用来区分各工具推送的进度事件
    pub call_id: Option<String>,
}

impl ToolContext {
    pub fn new(
        session_id: String,
        user_id: String,
        project_id: Option<String>,
        preferred_model: Option<String>,
        attachments: Vec<ChatAttachment>,
        emit: impl Fn(&str, serde_json::Value) + Send + Sync + 'static,
    ) -> Self {
        Self {
            session_id,
            user_id,
            project_id,
            preferred_model,
            attachments,
            prior_artifacts: Vec::new(),
            emit: Arc::new(emit),
            scratchpad: Arc::new(Mutex::new(HashMap::new())),
            tool_config: None,
            call_id: None,
        }
    }

    /// 为单次工具调用派生上下文：推送的对象型事件自动带上 `call_id`
    pub fn for_call(&self, call_id: &str) -> Self {
        let inner = self.emit.clone();
        let id = call_id.to_string();
        let mut ctx = self.clone();
        ctx.call_id = Some(id.clone());
        ctx.emit = Arc::new(move |event: &str, mut data: serde_json::Value| {
            if let Some(obj) = data.as_object_mut() {
                obj.entry("call_id")
                    .or_insert_with(|| serde_json::Value::String(id.clone()));
            }
            inner(event, data);
        });
        ctx
    }

    pub fn with_tool_config(mut self, config: serde_json::Value) -> Self {
        self.tool_config = Some(config);
        self
    }

    /// 设置会话中已有的产物历史
    pub fn with_prior_artifacts(mut self, artifacts: Vec<Artifact>) -> Self {
        self.prior_artifacts = artifacts;
        self
    }

    /// 获取工具配置中的某个字段值
    pub fn get_config<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.tool_config
            .as_ref()
            .and_then(|cfg| cfg.get(key))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn send(&self, event: &str, data: serde_json::Value) {
        (self.emit)(event, data);
    }
}

/// 工具产生的产物
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArtifact {
    pub kind: String, // document | ppt | drawio | sheet | image | code | mixed
    pub title: String,
    pub content: serde_json::Value,
}

/// 工具结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<ToolArtifact>>,
    /// 给 LLM 的观察文本（ReAct Observation）
    pub observation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continue_loop: Option<bool>,
}

impl ToolResult {
    pub fn ok(observation: impl Into<String>, artifacts: Vec<ToolArtifact>) -> Self {
        Self {
            success: true,
            data: None,
            error: None,
            artifacts: Some(artifacts),
            observation: observation.into(),
            continue_loop: None,
        }
    }

    pub fn err(observation: impl Into<String>) -> Self {
        let obs = observation.into();
        Self {
            success: false,
            data: None,
            error: Some(obs.clone()),
            artifacts: None,
            observation: obs,
            continue_loop: None,
        }
    }
}

/// 工具定义 trait
#[async_trait]
pub trait OfficeTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema 参数定义
    fn parameters(&self) -> serde_json::Value;
    /// 只读取信息、不生成产物也没有副作用（Ask 模式只开放这类工具）
    fn is_read_only(&self) -> bool {
        false
    }
    fn produces_artifact(&self) -> bool {
        true
    }
    /// 能否与同一轮的其他工具并发执行。依赖 scratchpad 或有先后顺序的工具返回 false，
    /// 调度时作为屏障单独串行执行
    fn parallel_safe(&self) -> bool {
        true
    }

    async fn call(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolResult;
}

/// 便利类型别名
pub type DynTool = Arc<dyn OfficeTool>;

impl ToolResult {
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
}
