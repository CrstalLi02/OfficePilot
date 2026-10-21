//! OfficePilot 文档引擎：原格式读写 .docx / .xlsx / .pptx。
//!
//! 原则：原文件是唯一的事实来源。读取时把文档解析成带原文位置的块；编辑只替换涉及的
//! XML 片段，zip 里其余条目按原始压缩数据复制，没改的部分逐字节不变。
//! 一批操作要么全部成功，要么文件保持不变，出错时返回能指导修正的错误。

pub mod build;
pub mod docx;
pub mod formula;
pub mod markdown;
pub mod package;
pub mod pptx;
pub mod text_edit;
pub mod xlsx;
pub mod xml;

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Format(String),
    #[error("XML 解析失败：{0}")]
    Xml(String),
    /// 某个操作不合法（第几个、哪种操作、原因与修正建议）
    #[error("第 {index} 个操作（{op}）失败：{message}")]
    Op { index: usize, op: String, message: String },
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Docx,
    Xlsx,
    Pptx,
}

impl Kind {
    pub fn from_name(name: &str) -> Option<Self> {
        let ext = name.rsplit('.').next()?.to_ascii_lowercase();
        match ext.as_str() {
            "docx" | "docm" | "dotx" => Some(Self::Docx),
            "xlsx" | "xlsm" | "xltx" => Some(Self::Xlsx),
            "pptx" | "pptm" | "potx" => Some(Self::Pptx),
            _ => None,
        }
    }

    /// 根据包的主文档判断类型
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        let pkg = package::Package::open(bytes.to_vec()).ok()?;
        let main = pkg.main_part().ok()?;
        if main.starts_with("word/") {
            Some(Self::Docx)
        } else if main.starts_with("xl/") {
            Some(Self::Xlsx)
        } else if main.starts_with("ppt/") {
            Some(Self::Pptx)
        } else {
            None
        }
    }
}

/// 一次修改的记录，用于向用户展示改了什么
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    /// 第几个操作（从 0 开始）
    pub op: usize,
    /// 操作类型
    pub kind: String,
    /// 位置描述：块序号、单元格、第几页等
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    pub changes: Vec<Change>,
    /// 修改后的版本标识（再次编辑时传回，防止基于过期内容修改）
    pub revision: String,
}

/// 内容版本：主部件内容的哈希前 12 位
pub fn revision_of(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(text.as_bytes()))[..12].to_string()
}

pub(crate) fn op_err(index: usize, op: &str, message: impl Into<String>) -> EngineError {
    EngineError::Op { index, op: op.to_string(), message: message.into() }
}

/// 截断过长的文字（用于修改记录）
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}
