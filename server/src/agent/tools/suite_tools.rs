//! 编辑器套件（OfficePilot Suite）提供的能力：格式转换、套模板、文档体检、逐页渲染，
//! 以及和用户正在使用的编辑器配合（读取选区、打开文件给用户看）。只在找到套件时注册。
//!
//! 套件的命令行只认磁盘路径，这里负责把工作区文件复制到临时目录交给它处理，
//! 产出另存回工作区（同名自动编号、可撤销），不让模型接触磁盘路径。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};

use crate::agent::tool::{OfficeTool, ToolArtifact, ToolContext, ToolResult};
use crate::db::file_repo::FileRow;
use crate::db::DbPool;
use crate::suite_cli::{self, Scratch, FAST, SLOW};
use crate::workspace::{self, Tree};

const PATH_DESC: &str = "工作区文件路径（如 /项目A/报告.pdf）或文件 id";
/// 渲染最多返回的页数
const MAX_RENDER_PAGES: usize = 12;
/// 给模型的体检结果最多这么多字
const MAX_ISSUES_CHARS: usize = 6_000;

fn pool() -> DbPool {
    crate::state::db_pool()
}

fn progress(ctx: &ToolContext, step: &str, detail: String) {
    ctx.send("state_update", json!({ "phase": "running", "step": step, "detail": detail, "at": chrono::Utc::now().to_rfc3339() }));
}

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default()
}

fn stem_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// 找到工作区文件，返回 (文件, 虚拟路径)
async fn find(pool: &DbPool, owner_id: &str, key: &str) -> Result<(FileRow, String), String> {
    let tree = Tree::load(pool, owner_id).await.map_err(|e| format!("读取工作区失败：{e}"))?;
    match tree.find_file(key)? {
        Some(file) => Ok((file.clone(), tree.file_path(file))),
        None => Err(format!("找不到文件：{key}。可以先用 workspace_search 或 workspace_list 查找；会话里生成的产物要先用 workspace_write 导出到工作区")),
    }
}

/// 把工作区文件复制到临时目录（用原文件名，套件按扩展名识别格式）
async fn stage(scratch: &Scratch, file: &FileRow) -> Result<PathBuf, String> {
    let local = scratch.0.join(crate::editor::safe_name(&file.name));
    let bytes = workspace::read_bytes(file).await.map_err(|e| e.to_string())?;
    tokio::fs::write(&local, bytes).await.map_err(|e| format!("准备文件失败：{e}"))?;
    Ok(local)
}

/// 目标位置：不传时放在原文件同目录；只给文件名时也放在原文件同目录；以 / 结尾是目录
fn target_path(source: &str, dest: Option<&str>, default_name: &str) -> String {
    match dest.map(str::trim).filter(|d| !d.is_empty()) {
        None => format!("{}/{default_name}", dir_of(source)),
        Some(d) if d.ends_with('/') => format!("{d}{default_name}"),
        Some(d) if d.contains('/') => d.to_string(),
        Some(d) => format!("{}/{d}", dir_of(source)),
    }
}

/// 把套件的输出另存进工作区，返回 (最终路径, op id)
async fn save_output(pool: &DbPool, owner_id: &str, session_id: Option<&str>, output: &Path, target: &str) -> Result<(String, Option<String>), String> {
    let bytes = tokio::fs::read(output).await.map_err(|e| format!("读取套件的输出失败：{e}"))?;
    let name = target.rsplit('/').next().unwrap_or(target);
    workspace::save_bytes_as(pool, owner_id, target, &bytes, crate::editor::mime_of(name), session_id).await.map_err(|e| e.to_string())
}

fn os(s: impl Into<OsString>) -> OsString {
    s.into()
}

fn warnings_text(v: &Value) -> String {
    let mut list: Vec<String> = Vec::new();
    for w in v["warnings"].as_array().into_iter().chain(v["detail"]["warnings"].as_array()).flatten() {
        list.push(w["message"].as_str().map(str::to_string).unwrap_or_else(|| w.as_str().map(str::to_string).unwrap_or_else(|| w.to_string())));
    }
    if list.is_empty() { String::new() } else { format!("\n提示：{}", list.join("；")) }
}

// ───────────────────────── 格式转换 ─────────────────────────

pub async fn convert(pool: &DbPool, owner_id: &str, session_id: Option<&str>, key: &str, to: &str, dest: Option<&str>) -> Result<(String, Option<String>, String), String> {
    let to = to.trim().trim_start_matches('.').to_lowercase();
    if to.is_empty() {
        return Err("缺少目标格式 to".into());
    }
    let (file, source) = find(pool, owner_id, key).await?;
    if ext_of(&file.name) == to {
        return Err(format!("{source} 已经是 .{to} 文件"));
    }
    let scratch = Scratch::new().map_err(|e| e.to_string())?;
    let local = stage(&scratch, &file).await?;
    let out = scratch.0.join("out").join(format!("{}.{to}", stem_of(&crate::editor::safe_name(&file.name))));
    tokio::fs::create_dir_all(out.parent().unwrap()).await.map_err(|e| e.to_string())?;
    let result = suite_cli::run(&[os("convert"), local.into_os_string(), os("--to"), os(&to), os("--out"), out.clone().into_os_string()], &scratch.0, SLOW).await?;
    let target = target_path(&source, dest, &format!("{}.{to}", stem_of(&file.name)));
    let (saved, op_id) = save_output(pool, owner_id, session_id, &out, &target).await?;
    let mut note = format!("已把 {source} 转换为 {saved}（另存的新文件，原文件不变，用户可撤销）。{}", warnings_text(&result));
    if ext_of(&file.name) == "pdf" {
        note.push_str("\n注意：PDF 转回可编辑格式只能大致还原结构，复杂排版（多栏、浮动图片、复杂表格）可能走样，回复用户时说明这一点；需要修改内容时用 docs_read / sheet_read / slides_read 查看转换结果。");
    }
    Ok((saved, op_id, note))
}

pub struct OfficeConvertTool;

#[async_trait]
impl OfficeTool for OfficeConvertTool {
    fn name(&self) -> &str {
        "office_convert"
    }
    fn description(&self) -> &str {
        "用编辑器套件转换工作区文件的格式，结果另存为新文件（原文件不变，可撤销）。支持：PDF → Word / Excel / PPT / Markdown / HTML；Word / Excel / PPT / Markdown / HTML / CSV → PDF；HTML → Word；Markdown → Word / HTML；Word → Markdown / HTML；旧格式 xls / odt / odp → 新格式。用户要把 PDF 改成可编辑的文档、或要导出 PDF 时用它。"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": PATH_DESC },
                "to": { "type": "string", "enum": ["pdf", "docx", "xlsx", "pptx", "md", "html", "csv"], "description": "目标格式" },
                "dest": { "type": "string", "description": "可选：保存位置。目录（以 / 结尾）、文件名或完整路径；默认与原文件同目录、同名换扩展名，重名自动编号" }
            },
            "required": ["path", "to"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("");
        let to = input["to"].as_str().unwrap_or("");
        progress(ctx, "转换格式", format!("正在把 {key} 转换为 .{to}"));
        match convert(&pool(), &ctx.user_id, Some(&ctx.session_id), key, to, input["dest"].as_str()).await {
            Ok((path, op_id, note)) => ToolResult::ok(note, vec![]).with_data(json!({ "path": path, "op_id": op_id, "summary": format!("转换为 {path}") })),
            Err(e) => ToolResult::err(format!("转换失败：{e}")),
        }
    }
}

// ───────────────────────── 套模板 ─────────────────────────

pub async fn merge(pool: &DbPool, owner_id: &str, session_id: Option<&str>, key: &str, data: &Value, dest: Option<&str>) -> Result<(String, Option<String>, String), String> {
    let data = match data {
        Value::Object(_) => data.clone(),
        // 有的模型把对象序列化成字符串传进来
        Value::String(s) => serde_json::from_str::<Value>(s).ok().filter(Value::is_object).ok_or("data 必须是 JSON 对象")?,
        _ => return Err("data 必须是 JSON 对象，例如 {\"name\": \"张三\"}".into()),
    };
    let (file, source) = find(pool, owner_id, key).await?;
    let ext = ext_of(&file.name);
    if !["docx", "pptx", "xlsx"].contains(&ext.as_str()) {
        return Err(format!("只能用 Word / PPT / Excel 文件做模板，{source} 不是"));
    }
    let scratch = Scratch::new().map_err(|e| e.to_string())?;
    let local = stage(&scratch, &file).await?;
    let data_file = scratch.0.join("data.json");
    tokio::fs::write(&data_file, serde_json::to_vec(&data).unwrap_or_default()).await.map_err(|e| e.to_string())?;
    let out = scratch.0.join(format!("merged.{ext}"));
    let result = suite_cli::run(&[os("merge"), local.into_os_string(), os("--data"), data_file.into_os_string(), os("--out"), out.clone().into_os_string()], &scratch.0, FAST).await?;
    let target = target_path(&source, dest, &format!("{}（已填写）.{ext}", stem_of(&file.name)));
    let (saved, op_id) = save_output(pool, owner_id, session_id, &out, &target).await?;
    let filled = result["detail"]["filled"].as_u64().unwrap_or(0);
    let mut note = format!("已用模板 {source} 生成 {saved}，填入 {filled} 个占位符（另存的新文件，模板不变，用户可撤销）。");
    let unresolved: Vec<String> = result["detail"]["unresolved_placeholders"].as_array().into_iter().flatten().filter_map(|p| p["placeholder"].as_str().map(str::to_string)).collect();
    if !unresolved.is_empty() {
        note.push_str(&format!("\n这些占位符没有对应的值，原样留在文件里：{}。告诉用户，或补上数据后重新生成。", unresolved.join("、")));
    }
    let unused: Vec<String> = result["detail"]["unused_keys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
    if !unused.is_empty() {
        note.push_str(&format!("\n模板里没有用到这些数据：{}。", unused.join("、")));
    }
    Ok((saved, op_id, note))
}

pub struct OfficeMergeTool;

#[async_trait]
impl OfficeTool for OfficeMergeTool {
    fn name(&self) -> &str {
        "office_merge"
    }
    fn description(&self) -> &str {
        "用数据填写工作区里的 Word / PPT / Excel 模板：把模板中的 {{占位符}} 替换成 data 里对应的值，保留模板原有排版，结果另存为新文件（模板不变，可撤销）。适合批量生成通知、合同、证书、报价单。占位符写法是 {{name}}，嵌套对象用点号（{{客户.名称}}）。"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "template": { "type": "string", "description": "模板文件：工作区路径或文件 id（.docx / .pptx / .xlsx）" },
                "data": { "type": "object", "description": "占位符的值，例如 {\"name\": \"张三\", \"date\": \"2026-10-30\"}" },
                "dest": { "type": "string", "description": "可选：保存位置（文件名、完整路径或以 / 结尾的目录）；默认在模板同目录，文件名加“（已填写）”" }
            },
            "required": ["template", "data"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["template"].as_str().unwrap_or("");
        progress(ctx, "填写模板", format!("正在用 {key} 生成文件"));
        match merge(&pool(), &ctx.user_id, Some(&ctx.session_id), key, &input["data"], input["dest"].as_str()).await {
            Ok((path, op_id, note)) => ToolResult::ok(note, vec![]).with_data(json!({ "path": path, "op_id": op_id, "summary": format!("生成 {path}") })),
            Err(e) => ToolResult::err(format!("填写模板失败：{e}")),
        }
    }
}

// ───────────────────────── 文档体检 ─────────────────────────

pub async fn check(pool: &DbPool, owner_id: &str, key: &str) -> Result<String, String> {
    let (file, source) = find(pool, owner_id, key).await?;
    let (domain, verb) = match ext_of(&file.name).as_str() {
        "docx" => ("docs", "check"),
        "xlsx" | "xlsm" => ("sheet", "check"),
        "pptx" => ("slides", "audit"),
        _ => return Err(format!("只能检查 Word / Excel / PPT 文件，{source} 不是")),
    };
    let scratch = Scratch::new().map_err(|e| e.to_string())?;
    let local = stage(&scratch, &file).await?;
    let result = suite_cli::run(&[os(domain), os(verb), local.into_os_string()], &scratch.0, FAST).await?;
    let detail = &result["detail"];
    let issues: Vec<Value> = match domain {
        // PPT 的问题按页列出
        "slides" => detail["slides"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|s| {
                let page = s["slide"].as_u64().map(|n| n + 1);
                s["issues"].as_array().cloned().unwrap_or_default().into_iter().map(move |mut i| {
                    i["page"] = json!(page);
                    i
                })
            })
            .collect(),
        _ => detail["issues"].as_array().cloned().unwrap_or_default(),
    };
    if issues.is_empty() {
        return Ok(format!("{source} 体检通过，没有发现问题（{}）。", result["summary"].as_str().unwrap_or("")));
    }
    let mut text = serde_json::to_string_pretty(&issues).unwrap_or_default();
    if text.chars().count() > MAX_ISSUES_CHARS {
        text = text.chars().take(MAX_ISSUES_CHARS).collect::<String>() + "\n…（问题较多，已截断）";
    }
    let counts = &detail["counts"];
    let mut note = format!(
        "{source} 体检发现 {} 个问题（错误 {}，警告 {}）：\n{text}",
        issues.len(),
        counts["error"].as_u64().unwrap_or(0),
        counts["warning"].as_u64().unwrap_or(0)
    );
    if domain == "slides" {
        note.push_str("\n页码从 1 开始；e_* 是编辑器套件里的元素编号，修改时用 slides_read 按文字找到对应形状。");
    } else if domain == "docs" {
        note.push_str("\n块序号按编辑器套件的编号，修改时用 docs_read 按文字定位。");
    }
    Ok(note)
}

pub struct OfficeCheckTool;

#[async_trait]
impl OfficeTool for OfficeCheckTool {
    fn name(&self) -> &str {
        "office_check"
    }
    fn description(&self) -> &str {
        "给工作区里的 Word / Excel / PPT 做体检（只读）：Word 查未更新的域、断开的交叉引用、过期目录、缺失图片、遗留的占位符、未处理的修订和批注；Excel 查公式错误（#REF! 等）、未计算的公式、引用了不存在的工作表、数字显示不下；PPT 查文字溢出、元素重叠、超出页面、图片变形。交付前检查文件质量，或用户问“这个文件有没有问题”时用它。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": PATH_DESC } },
            "required": ["path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("");
        progress(ctx, "文档体检", format!("正在检查 {key}"));
        match check(&pool(), &ctx.user_id, key).await {
            Ok(note) => ToolResult::ok(note, vec![]),
            Err(e) => ToolResult::err(format!("体检失败：{e}")),
        }
    }
}

// ───────────────────────── 逐页渲染 ─────────────────────────

/// 返回 (说明, 每页图片的 data URL)
pub async fn render(pool: &DbPool, owner_id: &str, key: &str, page: Option<u64>) -> Result<(String, Vec<String>), String> {
    let (file, source) = find(pool, owner_id, key).await?;
    if !["docx", "xlsx", "pptx", "pdf", "md", "html", "htm"].contains(&ext_of(&file.name).as_str()) {
        return Err(format!("不能渲染 {source}，只支持 Word / Excel / PPT / PDF / Markdown / HTML"));
    }
    let scratch = Scratch::new().map_err(|e| e.to_string())?;
    let local = stage(&scratch, &file).await?;
    let out = scratch.0.join("pages");
    let mut args = vec![os("render"), local.into_os_string(), os("--out"), out.into_os_string()];
    if let Some(p) = page {
        args.extend([os("--page"), os(p.to_string())]);
    }
    let result = suite_cli::run(&args, &scratch.0, SLOW).await?;
    let files: Vec<&Value> = result["detail"]["files"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let mut images = Vec::new();
    for f in files.iter().take(MAX_RENDER_PAGES) {
        let Some(path) = f["path"].as_str() else { continue };
        if let Ok(bytes) = tokio::fs::read(path).await {
            images.push(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)));
        }
    }
    if images.is_empty() {
        return Err("没有渲染出任何页面".into());
    }
    let mut note = format!("已把 {source} 渲染成 {} 页图片，展示给了用户。", images.len());
    if files.len() > images.len() {
        note.push_str(&format!("（共 {} 页，只展示了前 {MAX_RENDER_PAGES} 页；可以用 page 指定某一页）", files.len()));
    }
    Ok((note, images))
}

pub struct OfficeRenderTool;

#[async_trait]
impl OfficeTool for OfficeRenderTool {
    fn name(&self) -> &str {
        "office_render"
    }
    fn description(&self) -> &str {
        "把工作区里的 Word / Excel / PPT / PDF / Markdown / HTML 按真实版式逐页渲染成图片，展示给用户看（只读，最多 12 页）。用户想看文件实际排出来的样子、或改完后想确认效果时用它。你自己看不到图片内容，需要检查版面问题时用 office_check。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn produces_artifact(&self) -> bool {
        true
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": PATH_DESC },
                "page": { "type": "integer", "minimum": 1, "description": "可选：只渲染这一页（从 1 开始）" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("");
        progress(ctx, "渲染页面", format!("正在渲染 {key}"));
        match render(&pool(), &ctx.user_id, key, input["page"].as_u64()).await {
            Ok((note, images)) => {
                let title = format!("{} · 页面预览", key.rsplit('/').next().unwrap_or(key));
                let artifact = ToolArtifact {
                    kind: "image".into(),
                    title: title.clone(),
                    content: json!({
                        "type": "generated_image",
                        "title": title,
                        "images": images,
                        "variants": images.iter().map(|u| json!({ "url": u })).collect::<Vec<_>>(),
                        "provider": "officepilot-suite",
                    }),
                };
                ToolResult::ok(note, vec![artifact])
            }
            Err(e) => ToolResult::err(format!("渲染失败：{e}")),
        }
    }
}

// ───────────────────────── 编辑器：选区与打开 ─────────────────────────

pub async fn selection(pool: &DbPool, root: &Path, owner_id: &str, key: &str) -> Result<String, String> {
    let (file, source) = find(pool, owner_id, key).await?;
    let working = crate::editor::working_copy(root, owner_id, &file.id).await.ok_or_else(|| format!("{source} 没有在编辑器里打开。可以用 editor_open 打开，再请用户在编辑器里选中要处理的内容"))?;
    let scratch = Scratch::new().map_err(|e| e.to_string())?;
    let result = suite_cli::run(&[os("selection"), working.into_os_string()], &scratch.0, FAST).await?;
    let detail = result.get("detail").cloned().unwrap_or(result.clone());
    if detail["collapsed"] == true && detail["text"].as_str().map_or(true, str::is_empty) {
        return Ok(format!("用户在编辑器里打开了 {source}，但现在没有选中任何内容（只有光标位置：{}）。请用户先在编辑器里选中要处理的内容，或直接说明位置。", result["summary"].as_str().unwrap_or("")));
    }
    let mut text = serde_json::to_string_pretty(&detail).unwrap_or_default();
    if text.chars().count() > MAX_ISSUES_CHARS {
        text = text.chars().take(MAX_ISSUES_CHARS).collect::<String>() + "\n…（已截断）";
    }
    Ok(format!(
        "用户在编辑器里对 {source} 的当前选区（{}）：\n{text}\n选区里的序号和 e_* 编号属于编辑器套件；修改时用 docs_read / sheet_read / slides_read 按选中的文字或单元格地址定位。文件正在编辑器里打开，修改会同步到编辑器，套件会提示用户重新载入。",
        result["summary"].as_str().unwrap_or("")
    ))
}

pub struct EditorSelectionTool;

#[async_trait]
impl OfficeTool for EditorSelectionTool {
    fn name(&self) -> &str {
        "editor_selection"
    }
    fn description(&self) -> &str {
        "读取用户在编辑器套件里对某个文件当前选中的内容（选中的文字、单元格范围、幻灯片元素）。用户说“这里”“这一段”“选中的这些”“这页”指编辑器里的内容时，先用它弄清楚指的是什么（只读）。文件需要已经用编辑器打开。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": PATH_DESC } },
            "required": ["path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("");
        match selection(&pool(), &crate::editor::open_root(), &ctx.user_id, key).await {
            Ok(note) => ToolResult::ok(note, vec![]),
            Err(e) => ToolResult::err(format!("读取选区失败：{e}")),
        }
    }
}

/// 编辑器里定位：PDF 页、PPT 页、Excel 区域（都按用户习惯从 1 开始，转换成套件的编号）
fn open_target_args(ext: &str, input: &Value) -> Result<Vec<OsString>, String> {
    let page = input["page"].as_u64().filter(|p| *p >= 1);
    let range = input["range"].as_str().map(str::trim).filter(|r| !r.is_empty());
    Ok(match (ext, page, range) {
        ("pdf", Some(p), _) => vec![os("--page"), os(p.to_string())],
        ("pptx", Some(p), _) => vec![os("--slide"), os((p - 1).to_string())],
        ("xlsx", _, Some(r)) => vec![os("--range"), os(r)],
        (_, None, None) => vec![],
        _ => return Err("page 只适用于 PDF 和 PPT，range 只适用于 Excel".into()),
    })
}

pub struct EditorOpenTool;

#[async_trait]
impl OfficeTool for EditorOpenTool {
    fn name(&self) -> &str {
        "editor_open"
    }
    fn description(&self) -> &str {
        "在用户电脑上用编辑器套件打开工作区里的文件给用户看，可以定位到 PDF / PPT 的某一页或 Excel 的某个区域。用户在编辑器里保存后会自动同步回工作区。只在用户想自己查看或编辑某个文件时使用，不要为了读取内容而打开（读取用 workspace_read / docs_read 等）。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": PATH_DESC },
                "page": { "type": "integer", "minimum": 1, "description": "可选：PDF 或 PPT 定位到第几页（从 1 开始）" },
                "range": { "type": "string", "description": "可选：Excel 选中的区域，如 B2:D5 或 Sheet1!B2:D5" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        // 定时任务和飞书里没有人在电脑前，不弹窗口
        if matches!(ctx.origin.as_deref(), Some("schedule" | "im")) {
            return ToolResult::err("只有在 OfficePilot 里对话时才能在电脑上打开编辑器");
        }
        let key = input["path"].as_str().unwrap_or("");
        let pool = pool();
        let (file, source) = match find(&pool, &ctx.user_id, key).await {
            Ok(v) => v,
            Err(e) => return ToolResult::err(e),
        };
        if !crate::editor::supported(&file.name) {
            return ToolResult::err(format!("编辑器不支持 {source}"));
        }
        let target = match open_target_args(&ext_of(&file.name), &input) {
            Ok(t) => t,
            Err(e) => return ToolResult::err(e),
        };
        let working = match crate::editor::prepare_copy(&crate::editor::open_root(), &ctx.user_id, &file).await {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("准备文件失败：{e}")),
        };
        let opened = if target.is_empty() {
            match crate::editor::editor_app() {
                Some(app) => crate::editor::launch(&app, &working).await.map_err(|e| e.to_string()),
                None => Err("没有找到编辑器套件".into()),
            }
        } else {
            let mut args = vec![os("open"), working.clone().into_os_string()];
            args.extend(target);
            match Scratch::new() {
                Ok(scratch) => suite_cli::run(&args, &scratch.0, SLOW).await.map(|_| ()),
                Err(e) => Err(e.to_string()),
            }
        };
        match opened {
            Ok(()) => ToolResult::ok(format!("已在编辑器里打开 {source}。用户保存后会自动同步回工作区（可撤销）。"), vec![]),
            Err(e) => ToolResult::err(format!("打开编辑器失败：{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_names() {
        assert_eq!(target_path("/项目/报告.pdf", None, "报告.docx"), "/项目/报告.docx");
        assert_eq!(target_path("/报告.pdf", Some("新.docx"), "报告.docx"), "/新.docx");
        assert_eq!(target_path("/项目/报告.pdf", Some("/归档/"), "报告.docx"), "/归档/报告.docx");
        assert_eq!(target_path("/项目/报告.pdf", Some("/归档/改.docx"), "报告.docx"), "/归档/改.docx");
        assert_eq!(stem_of("a.b.docx"), "a.b");
        assert_eq!(open_target_args("pptx", &json!({"page": 3})).unwrap(), vec![os("--slide"), os("2")]);
        assert_eq!(open_target_args("xlsx", &json!({"range": "B2:C3"})).unwrap(), vec![os("--range"), os("B2:C3")]);
        assert!(open_target_args("docx", &json!({"page": 2})).is_err());
        assert!(open_target_args("docx", &json!({})).unwrap().is_empty());
    }

    /// 假的套件命令行：按命令写出可预期的结果
    const FAKE_CLI: &str = r#"#!/bin/sh
cmd="$1"; shift
[ "$cmd" = docs ] || [ "$cmd" = sheet ] || [ "$cmd" = slides ] && { verb="$1"; shift; }
in="$1"; shift
to=""; out=""; data=""
while [ $# -gt 0 ]; do
  case "$1" in
    --to) to="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --data) data="$2"; shift 2 ;;
    *) shift ;;
  esac
done
case "$cmd" in
  convert)
    if [ "$to" = xyz ]; then echo '{"status":"error","error":"unsupported","message":"cannot convert","detail":{"supported":"pdf→docx"}}'; exit 1; fi
    printf 'converted:%s' "$(cat "$in")" > "$out"
    echo "page 1/1"
    echo '{"status":"ok","detail":{"warnings":[]}}' ;;
  merge)
    printf 'merged:%s' "$(cat "$data")" > "$out"
    echo '{"status":"ok","detail":{"filled":1,"unresolved_placeholders":[{"placeholder":"{{x}}"}],"unused_keys":["extra"]}}' ;;
  docs)
    echo '{"status":"ok","summary":"1 issue","detail":{"issues":[{"code":"toc_stale","block":3,"message":"目录过期"}],"counts":{"error":0,"warning":1}}}' ;;
  slides)
    echo '{"status":"ok","summary":"ok","detail":{"issues":[],"slides":[{"slide":1,"issues":[{"code":"overflow","el":"e_2"}]}],"counts":{"error":1,"warning":0}}}' ;;
  render)
    mkdir -p "$out"; printf 'PNGDATA' > "$out/p-01.png"
    echo "{\"status\":\"ok\",\"detail\":{\"files\":[{\"page\":1,\"path\":\"$out/p-01.png\"}]}}" ;;
  selection)
    echo '{"status":"ok","summary":"1 block","detail":{"text":"选中的这段"}}' ;;
esac
"#;

    fn fake_cli() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let path = std::env::temp_dir().join(format!("officepilot-fake-suite-{}.sh", uuid::Uuid::new_v4().simple()));
            std::fs::write(&path, FAKE_CLI).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::env::set_var("OFFICEPILOT_SUITE_CLI", &path);
        });
    }

    async fn memory_pool() -> DbPool {
        sqlx::any::install_default_drivers();
        let pool = sqlx::any::AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        for sql in [
            include_str!("../../../../migrations/001_init.sql"),
            include_str!("../../../../migrations/004_workspace.sql"),
            "INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES ('u1', 'u', 'x', 't', 't');",
        ] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn add(pool: &DbPool, path: &str, content: &str) {
        workspace::save_bytes_as(pool, "u1", path, content.as_bytes(), "application/octet-stream", None).await.unwrap();
    }

    async fn read(pool: &DbPool, path: &str) -> String {
        let tree = Tree::load(pool, "u1").await.unwrap();
        std::fs::read_to_string(&tree.find_file(path).unwrap().unwrap().file_path).unwrap()
    }

    async fn cleanup(pool: &DbPool) {
        for f in Tree::load(pool, "u1").await.unwrap().files() {
            let _ = std::fs::remove_file(&f.file_path);
        }
    }

    #[tokio::test]
    async fn convert_saves_next_to_original_and_undo_removes() {
        fake_cli();
        let pool = memory_pool().await;
        add(&pool, "/项目/报告.pdf", "PDF").await;
        let (path, op, note) = convert(&pool, "u1", None, "/项目/报告.pdf", "docx", None).await.unwrap();
        assert_eq!(path, "/项目/报告.docx");
        assert_eq!(read(&pool, &path).await, "converted:PDF");
        assert!(note.contains("大致还原结构"), "PDF 转换要提醒结构可能走样");
        // 重名自动编号，原文件不变
        let (again, _, _) = convert(&pool, "u1", None, "/项目/报告.pdf", ".DOCX", None).await.unwrap();
        assert_eq!(again, "/项目/报告 (2).docx");
        assert_eq!(read(&pool, "/项目/报告.pdf").await, "PDF");
        // 指定目录
        let (dest, _, _) = convert(&pool, "u1", None, "/项目/报告.pdf", "html", Some("/导出/")).await.unwrap();
        assert_eq!(dest, "/导出/报告.html");

        workspace::undo(&pool, "u1", &op.unwrap()).await.unwrap();
        assert!(Tree::load(&pool, "u1").await.unwrap().find_file("/项目/报告.docx").unwrap().is_none());

        let err = convert(&pool, "u1", None, "/项目/报告.pdf", "xyz", None).await.unwrap_err();
        assert!(err.contains("不支持这种转换") && err.contains("pdf→docx"), "{err}");
        assert!(convert(&pool, "u1", None, "/项目/报告.pdf", "pdf", None).await.unwrap_err().contains("已经是"));
        assert!(convert(&pool, "u1", None, "/没有.pdf", "docx", None).await.unwrap_err().contains("找不到文件"));
        cleanup(&pool).await;
    }

    #[tokio::test]
    async fn merge_check_render_and_selection() {
        fake_cli();
        let pool = memory_pool().await;
        add(&pool, "/模板/通知.docx", "TPL").await;
        add(&pool, "/讲稿.pptx", "PPT").await;
        add(&pool, "/笔记.md", "MD").await;

        let (path, _, note) = merge(&pool, "u1", None, "/模板/通知.docx", &json!({"name": "张三"}), None).await.unwrap();
        assert_eq!(path, "/模板/通知（已填写）.docx");
        assert_eq!(read(&pool, &path).await, r#"merged:{"name":"张三"}"#);
        assert!(note.contains("{{x}}") && note.contains("extra"), "{note}");
        // 模型把对象当字符串传进来
        assert!(merge(&pool, "u1", None, "/模板/通知.docx", &json!("{\"a\":1}"), Some("第二份.docx")).await.is_ok());
        assert!(merge(&pool, "u1", None, "/模板/通知.docx", &json!([1]), None).await.is_err());
        assert!(merge(&pool, "u1", None, "/笔记.md", &json!({}), None).await.unwrap_err().contains("模板"));

        let report = check(&pool, "u1", "/模板/通知.docx").await.unwrap();
        assert!(report.contains("目录过期") && report.contains("1 个问题"), "{report}");
        let slides = check(&pool, "u1", "/讲稿.pptx").await.unwrap();
        assert!(slides.contains("\"page\": 2") && slides.contains("错误 1"), "PPT 页码从 1 开始：{slides}");
        assert!(check(&pool, "u1", "/笔记.md").await.is_err());

        let (note, images) = render(&pool, "u1", "/讲稿.pptx", None).await.unwrap();
        assert_eq!(images, vec![format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode("PNGDATA"))]);
        assert!(note.contains("1 页"));

        let root = std::env::temp_dir().join(format!("officepilot-open-{}", uuid::Uuid::new_v4().simple()));
        let err = selection(&pool, &root, "u1", "/讲稿.pptx").await.unwrap_err();
        assert!(err.contains("没有在编辑器里打开"));
        let tree = Tree::load(&pool, "u1").await.unwrap();
        let file = tree.find_file("/讲稿.pptx").unwrap().unwrap().clone();
        crate::editor::prepare_copy(&root, "u1", &file).await.unwrap();
        let sel = selection(&pool, &root, "u1", "/讲稿.pptx").await.unwrap();
        assert!(sel.contains("选中的这段"), "{sel}");
        let _ = std::fs::remove_dir_all(&root);
        cleanup(&pool).await;
    }

    /// 用真实的编辑器套件跑一遍（需要先运行 scripts/build-suite.sh；会在屏幕上打开编辑器窗口）：
    /// OFFICEPILOT_REAL_SUITE_DIR=<放测试文件的目录> cargo test --lib real_suite -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_suite() {
        let dir = PathBuf::from(std::env::var("OFFICEPILOT_REAL_SUITE_DIR").expect("OFFICEPILOT_REAL_SUITE_DIR"));
        let pool = memory_pool().await;
        for name in ["季度报告.pdf", "季度报告.docx", "周报.html", "tpl.docx", "订单.xlsx", "发布方案.pptx"] {
            let bytes = std::fs::read(dir.join(name)).unwrap();
            workspace::save_bytes_as(&pool, "u1", &format!("/{name}"), &bytes, crate::editor::mime_of(name), None).await.unwrap();
        }
        for (src, to) in [("/季度报告.pdf", "docx"), ("/季度报告.docx", "pdf"), ("/周报.html", "docx"), ("/订单.xlsx", "pdf"), ("/发布方案.pptx", "pdf")] {
            let (path, _, note) = convert(&pool, "u1", None, src, to, None).await.unwrap_or_else(|e| panic!("{src} → {to}: {e}"));
            let tree = Tree::load(&pool, "u1").await.unwrap();
            let size = std::fs::metadata(&tree.find_file(&path).unwrap().unwrap().file_path).unwrap().len();
            println!("convert {src} → {path} ({size} bytes)\n  {}", note.lines().next().unwrap_or(""));
            assert!(size > 500);
        }
        let (path, _, note) = merge(&pool, "u1", None, "/tpl.docx", &json!({"name": "张三", "date": "周五"}), None).await.unwrap();
        println!("merge → {path}\n  {note}");
        for p in ["/tpl.docx", "/订单.xlsx", "/发布方案.pptx"] {
            println!("check {p}:\n{}", check(&pool, "u1", p).await.unwrap());
        }
        let (note, images) = render(&pool, "u1", "/发布方案.pptx", None).await.unwrap();
        println!("render: {note} ({} images, first {} chars)", images.len(), images[0].len());
        assert_eq!(images.len(), 2);

        let root = std::env::temp_dir().join(format!("officepilot-open-{}", uuid::Uuid::new_v4().simple()));
        let tree = Tree::load(&pool, "u1").await.unwrap();
        let file = tree.find_file("/季度报告.docx").unwrap().unwrap().clone();
        let working = crate::editor::prepare_copy(&root, "u1", &file).await.unwrap();
        crate::editor::launch(&crate::editor::suite_app().unwrap(), &working).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        println!("selection: {}", selection(&pool, &root, "u1", "/季度报告.docx").await.unwrap_or_else(|e| format!("ERR {e}")));
        let _ = std::fs::remove_dir_all(&root);
        cleanup(&pool).await;
    }
}
