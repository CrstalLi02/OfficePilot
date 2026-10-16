//! 工作区工具：列目录、搜索、读取（只读），写入、批量整理（可撤销，不提供删除）

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::tool::{OfficeTool, ToolContext, ToolResult};
use crate::workspace::{self, OrganizeOp, Tree};

fn pool() -> crate::db::DbPool {
    crate::state::db_pool()
}

fn progress(ctx: &ToolContext, step: &str, detail: String) {
    ctx.send(
        "state_update",
        json!({ "phase": "running", "step": step, "detail": detail, "at": chrono::Utc::now().to_rfc3339() }),
    );
}

// ───────────────────────── workspace_list ─────────────────────────

pub struct WorkspaceListTool;

#[async_trait]
impl OfficeTool for WorkspaceListTool {
    fn name(&self) -> &str {
        "workspace_list"
    }
    fn description(&self) -> &str {
        "列出用户工作区（“文件”）某个目录下的文件和文件夹，可递归、可按通配符过滤（如 *.xlsx）。用户问“我有哪些文件”“某个文件夹里有什么”，或整理文件前需要先看看现状时使用。"
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
                "path": { "type": "string", "description": "目录路径，默认根目录 /" },
                "recursive": { "type": "boolean", "description": "是否包含所有子目录，默认 false" },
                "pattern": { "type": "string", "description": "文件名通配符，如 *.docx、*合同*" }
            }
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let path = input["path"].as_str().unwrap_or("/");
        let recursive = input["recursive"].as_bool().unwrap_or(false);
        let pattern = input["pattern"].as_str().filter(|p| !p.trim().is_empty());
        let tree = match Tree::load(&pool(), &ctx.user_id).await {
            Ok(tree) => tree,
            Err(e) => return ToolResult::err(format!("读取工作区失败：{e}")),
        };
        match workspace::list(&tree, path, recursive, pattern, 200) {
            Ok((entries, total)) => {
                let lines: Vec<String> = entries
                    .iter()
                    .map(|e| match (e.kind, e.size) {
                        ("file", Some(size)) => format!("{}（{} KB）", e.path, (size + 1023) / 1024),
                        _ => e.path.clone(),
                    })
                    .collect();
                let more = if total > entries.len() { format!("\n…共 {total} 项，只列出前 {}", entries.len()) } else { String::new() };
                let observation = if lines.is_empty() { format!("{path} 下没有匹配的内容") } else { format!("{}{more}", lines.join("\n")) };
                ToolResult::ok(observation, vec![]).with_data(json!({ "path": path, "total": total, "entries": entries }))
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

// ───────────────────────── workspace_search ─────────────────────────

pub struct WorkspaceSearchTool;

#[async_trait]
impl OfficeTool for WorkspaceSearchTool {
    fn name(&self) -> &str {
        "workspace_search"
    }
    fn description(&self) -> &str {
        "在用户工作区里按文件名和正文全文搜索（多个关键词用空格分隔，正文需同时包含全部关键词），返回文件路径和命中片段。用户说“找一下上个月的会议纪要”“哪份文件提到了某个客户”时使用；找到后用 workspace_read 读取全文。"
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
                "query": { "type": "string", "description": "关键词，多个用空格分隔" },
                "limit": { "type": "integer", "description": "最多返回条数，默认 20", "minimum": 1, "maximum": 50 }
            },
            "required": ["query"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let query = input["query"].as_str().unwrap_or("").trim();
        let limit = input["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
        progress(ctx, "搜索文件", format!("正在搜索“{query}”"));
        let pool = pool();
        let tree = match Tree::load(&pool, &ctx.user_id).await {
            Ok(tree) => tree,
            Err(e) => return ToolResult::err(format!("读取工作区失败：{e}")),
        };
        match workspace::search(&pool, &ctx.user_id, &tree, query, limit).await {
            Ok((hits, total, indexed)) => {
                let lines: Vec<String> = hits
                    .iter()
                    .map(|h| match &h.snippet {
                        Some(snippet) => format!("{}（id={}）：…{snippet}…", h.path, h.id),
                        None => format!("{}（id={}，文件名匹配）", h.path, h.id),
                    })
                    .collect();
                let coverage = format!("（正文已索引 {indexed} 个文档）");
                let observation = if lines.is_empty() {
                    format!("没有找到包含“{query}”的文件{coverage}")
                } else {
                    format!("找到 {total} 个文件{coverage}：\n{}", lines.join("\n"))
                };
                ToolResult::ok(observation, vec![]).with_data(json!({ "query": query, "total": total, "hits": hits }))
            }
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

// ───────────────────────── workspace_read ─────────────────────────

pub struct WorkspaceReadTool;

#[async_trait]
impl OfficeTool for WorkspaceReadTool {
    fn name(&self) -> &str {
        "workspace_read"
    }
    fn description(&self) -> &str {
        "读取工作区里的文件内容（Word、PDF、PPT、Excel、Markdown、文本等都会转成文字），长文件可用 offset 分段读取。需要基于用户已有文件总结、提取信息、写报告或做 PPT 时先用它读取原文，不要凭空编写。"
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
                "path": { "type": "string", "description": "文件路径（如 /项目A/需求.docx）或文件 id" },
                "offset": { "type": "integer", "description": "从第几个字符开始读，默认 0", "minimum": 0 },
                "max_chars": { "type": "integer", "description": "最多读取字符数，默认 6000，上限 12000", "minimum": 200, "maximum": 12000 }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("");
        let offset = input["offset"].as_u64().unwrap_or(0) as usize;
        let max_chars = input["max_chars"].as_u64().unwrap_or(6000).clamp(200, 12_000) as usize;
        let pool = pool();
        let tree = match Tree::load(&pool, &ctx.user_id).await {
            Ok(tree) => tree,
            Err(e) => return ToolResult::err(format!("读取工作区失败：{e}")),
        };
        let file = match tree.find_file(key) {
            Ok(Some(file)) => file.clone(),
            Ok(None) => return ToolResult::err(format!("找不到文件：{key}。可以先用 workspace_search 或 workspace_list 查找")),
            Err(e) => return ToolResult::err(e),
        };
        let path = tree.file_path(&file);
        progress(ctx, "读取文件", format!("正在读取 {path}"));
        match workspace::text_of(&pool, &ctx.user_id, &file).await {
            Ok(text) => {
                let (piece, total, next) = workspace::slice_chars(&text, offset, max_chars);
                let header = match next {
                    Some(next) => format!("{path}（共 {total} 字，本次第 {offset}–{next} 字；继续读取请传 offset={next}）"),
                    None => format!("{path}（共 {total} 字，已读到结尾）"),
                };
                ToolResult::ok(format!("{header}\n\n{piece}"), vec![]).with_data(json!({
                    "path": path, "id": file.id, "total_chars": total, "offset": offset, "next_offset": next
                }))
            }
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

// ───────────────────────── workspace_write ─────────────────────────

pub struct WorkspaceWriteTool;

#[async_trait]
impl OfficeTool for WorkspaceWriteTool {
    fn name(&self) -> &str {
        "workspace_write"
    }
    fn description(&self) -> &str {
        "把内容保存到用户工作区（可撤销，同名时自动编号、不会覆盖）。两种用法：1) 新建 .md / .txt / .csv / .json / .html 文本文件，传 path 和 content；2) 把会话里已生成的产物（Word、PPT、Excel、draw.io 等）导出成文件存到指定目录，传 artifact_id 和 folder（可选 name）。用户说“保存到某个文件夹”“存一份到项目 A 里”时使用。"
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
                "path": { "type": "string", "description": "新建文本文件的完整路径，如 /笔记/2026-10 周报.md（目录不存在会自动创建）" },
                "content": { "type": "string", "description": "文本文件内容" },
                "artifact_id": { "type": "string", "description": "要导出的产物 id（见产物清单）" },
                "folder": { "type": "string", "description": "导出产物时的目标目录，默认 /" },
                "name": { "type": "string", "description": "导出产物时的文件名（可选，扩展名以产物格式为准）" }
            }
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let pool = pool();
        let session = Some(ctx.session_id.as_str());
        let result = if let Some(artifact_id) = input["artifact_id"].as_str().filter(|s| !s.is_empty()) {
            let artifacts = ctx.artifacts();
            let Some(artifact) = artifacts.iter().find(|a| a.id == artifact_id || a.title == artifact_id) else {
                return ToolResult::err(format!("找不到产物 {artifact_id}"));
            };
            let folder = input["folder"].as_str().unwrap_or("/");
            progress(ctx, "保存文件", format!("正在把《{}》导出到 {folder}", artifact.title));
            workspace::export_artifact(&pool, &ctx.user_id, artifact, folder, input["name"].as_str(), session).await
        } else {
            let Some(path) = input["path"].as_str().filter(|s| !s.trim().is_empty()) else {
                return ToolResult::err("需要 path + content（新建文本文件）或 artifact_id + folder（导出产物）");
            };
            let content = input["content"].as_str().unwrap_or("");
            workspace::write_text(&pool, &ctx.user_id, path, content, session).await
        };
        match result {
            Ok((path, op_id)) => ToolResult::ok(format!("已保存到 {path}（用户可撤销）"), vec![])
                .with_data(json!({ "path": path, "op_id": op_id, "summary": format!("保存 {path}") })),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

// ───────────────────────── workspace_organize ─────────────────────────

pub struct WorkspaceOrganizeTool;

#[async_trait]
impl OfficeTool for WorkspaceOrganizeTool {
    fn name(&self) -> &str {
        "workspace_organize"
    }
    fn description(&self) -> &str {
        "批量整理用户工作区：建目录、移动、重命名文件或文件夹，把文档转换成 md / txt，把 Excel 转换成 csv（每个 sheet 一个文件）。一次提交一组操作，系统会先校验全部操作，再依次执行；整组可由用户一键撤销。不支持删除。用户说“按项目归档”“统一改名”“把这些 Word 转成 Markdown”时使用；整理前先用 workspace_list 看清现状。"
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
                "ops": {
                    "type": "array",
                    "description": "按顺序执行的操作。路径可用文件 id 代替。示例：{\"op\":\"mkdir\",\"path\":\"/项目A/合同\"}；{\"op\":\"move\",\"from\":\"/报价单.docx\",\"to\":\"/项目A/合同\"}（目标目录不存在会自动创建）；{\"op\":\"rename\",\"path\":\"/a.docx\",\"name\":\"2026-10 报价单.docx\"}；{\"op\":\"convert\",\"path\":\"/需求.docx\",\"to\":\"md\",\"dest\":\"/Markdown\"}",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": { "type": "string", "enum": ["mkdir", "move", "rename", "convert"], "description": "convert 的 to 可为 md / txt / csv（csv 仅限 Excel）" },
                            "path": { "type": "string" },
                            "from": { "type": "string" },
                            "to": { "type": "string" },
                            "name": { "type": "string" },
                            "dest": { "type": "string" }
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["ops"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let ops: Vec<OrganizeOp> = match serde_json::from_value(input["ops"].clone()) {
            Ok(ops) => ops,
            Err(e) => return ToolResult::err(format!("操作格式不正确：{e}")),
        };
        progress(ctx, "整理文件", format!("正在执行 {} 个整理操作", ops.len()));
        match workspace::organize(&pool(), &ctx.user_id, &ops, Some(ctx.session_id.as_str())).await {
            Ok((lines, op_id)) => {
                let summary = format!("整理了 {} 项", lines.len());
                ToolResult::ok(format!("已完成（用户可一键撤销）：\n{}", lines.join("\n")), vec![])
                    .with_data(json!({ "op_id": op_id, "summary": summary, "changes": lines }))
            }
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}
