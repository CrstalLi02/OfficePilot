//! officepilot 的命令实现：命令行与 MCP 服务端共用同一套函数，结果完全一致。

pub mod convert;
pub mod deck_cmd;
pub mod guides;
pub mod io;
pub mod mcp;
pub mod office;

use serde_json::Value;

/// 命令结果：给人看的摘要 + 给程序的 JSON
pub struct Done {
    pub summary: String,
    pub data: Value,
    /// 检查没通过等“成功执行但结果不合格”的情况
    pub failed: bool,
}

impl Done {
    pub fn ok(summary: impl Into<String>, data: Value) -> Self {
        Self { summary: summary.into(), data, failed: false }
    }
}

/// 失败：说明原因与修正建议
#[derive(Debug)]
pub struct Fail {
    pub message: String,
    pub hint: Option<String>,
}

impl Fail {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), hint: None }
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl From<officepilot_engine::EngineError> for Fail {
    fn from(e: officepilot_engine::EngineError) -> Self {
        Fail::new(e.to_string())
    }
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::new(e.to_string())
    }
}

pub type Out = Result<Done, Fail>;
