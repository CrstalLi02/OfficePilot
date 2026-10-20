//! 原格式编辑工作区里的 Word / Excel / PPT：先 *_read 看结构，再 *_apply 提交一批操作。
//!
//! 只改写涉及的部分，其余内容逐字节保留；原地保存前存快照，用户可一键撤销；
//! 也可以另存为新文件。一批操作要么全部生效，要么文件不变。

use async_trait::async_trait;
use officepilot_engine::{docx, pptx, xlsx, ApplyReport, Kind};
use serde_json::{json, Value};

use crate::agent::tool::{OfficeTool, ToolContext, ToolResult};
use crate::db::file_repo::FileRow;
use crate::workspace::{self, Tree};

/// 可编辑文件的大小上限
const MAX_BYTES: u64 = 60 * 1024 * 1024;

fn pool() -> crate::db::DbPool {
    crate::state::db_pool()
}

fn progress(ctx: &ToolContext, step: &str, detail: String) {
    ctx.send("state_update", json!({ "phase": "running", "step": step, "detail": detail, "at": chrono::Utc::now().to_rfc3339() }));
}

fn kind_label(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Docx => ("Word", "docs"),
        Kind::Xlsx => ("Excel", "sheet"),
        Kind::Pptx => ("PPT", "slides"),
    }
}

/// 找到工作区文件并读出内容，检查类型
async fn load(ctx: &ToolContext, key: &str, kind: Kind) -> Result<(FileRow, String, Vec<u8>), ToolResult> {
    let pool = pool();
    let tree = Tree::load(&pool, &ctx.user_id).await.map_err(|e| ToolResult::err(format!("读取工作区失败：{e}")))?;
    let file = match tree.find_file(key) {
        Ok(Some(file)) => file.clone(),
        Ok(None) => return Err(ToolResult::err(format!("找不到文件：{key}。可以先用 workspace_search 或 workspace_list 查找；会话里生成的产物要先用 workspace_write 导出到工作区"))),
        Err(e) => return Err(ToolResult::err(e)),
    };
    let path = tree.file_path(&file);
    if Kind::from_name(&file.name) != Some(kind) {
        let (label, prefix) = kind_label(kind);
        let actual = Kind::from_name(&file.name).map(|k| format!("，它是 {} 文件，请用 {}_* 工具", kind_label(k).0, kind_label(k).1)).unwrap_or_default();
        return Err(ToolResult::err(format!("{prefix}_* 只能编辑 {label} 文件（{path}）{actual}")));
    }
    if file.file_size as u64 > MAX_BYTES {
        return Err(ToolResult::err(format!("{path} 太大（{} MB），暂不支持编辑", file.file_size / 1024 / 1024)));
    }
    let bytes = workspace::read_bytes(&file).await.map_err(|e| ToolResult::err(e.to_string()))?;
    Ok((file, path, bytes))
}

fn mime_of(kind: Kind) -> &'static str {
    match kind {
        Kind::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Kind::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Kind::Pptx => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("内部错误：{e}"))?
}

fn ops_array(input: &Value) -> Result<Value, ToolResult> {
    match &input["ops"] {
        Value::Array(a) if !a.is_empty() => Ok(input["ops"].clone()),
        // 有的模型把数组序列化成字符串传进来
        Value::String(s) => serde_json::from_str::<Value>(s).ok().filter(|v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false)).ok_or_else(|| ToolResult::err("ops 必须是非空数组")),
        _ => Err(ToolResult::err("ops 必须是非空数组")),
    }
}

fn changes_text(report: &ApplyReport) -> String {
    report
        .changes
        .iter()
        .map(|c| {
            let mut line = format!("- #{} {} {}", c.op, c.kind, c.target);
            if let Some(b) = &c.before {
                line.push_str(&format!("：原“{}”", b.replace('\n', " ")));
            }
            if let Some(a) = &c.after {
                line.push_str(&format!(" → “{}”", a.replace('\n', " ")));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 保存修改：原地（可撤销）或另存为
async fn save(ctx: &ToolContext, file: &FileRow, path: &str, kind: Kind, bytes: Vec<u8>, report: ApplyReport, save_as: Option<&str>) -> ToolResult {
    let pool = pool();
    let session = Some(ctx.session_id.as_str());
    let count = report.changes.len();
    let (saved_path, op_id) = match save_as.map(str::trim).filter(|s| !s.is_empty()) {
        Some(target) => {
            let target = if target.contains('/') {
                target.to_string()
            } else {
                // 只给文件名时放在原文件同目录
                let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                format!("{dir}/{target}")
            };
            match workspace::save_bytes_as(&pool, &ctx.user_id, &target, &bytes, mime_of(kind), session).await {
                Ok(r) => r,
                Err(e) => return ToolResult::err(e.to_string()),
            }
        }
        None => {
            let summary = format!("修改 {path}（{count} 处）");
            match workspace::replace_content(&pool, &ctx.user_id, file, &bytes, &summary, session).await {
                Ok(op) => (path.to_string(), op),
                Err(e) => return ToolResult::err(e.to_string()),
            }
        }
    };
    let changes = changes_text(&report);
    let observation = format!(
        "已保存到 {saved_path}（{count} 处改动，用户可一键撤销；未改动的内容原样保留）。新版本 revision={}。\n{changes}\n注意：块序号 / 页码可能已变化，再次修改前先重新读取。",
        report.revision
    );
    ToolResult::ok(observation, vec![]).with_data(json!({
        "path": saved_path,
        "file_id": file.id,
        "op_id": op_id,
        "summary": format!("修改 {saved_path}（{count} 处）"),
        "changes": report.changes,
        "revision": report.revision,
    }))
}

fn check_revision(input: &Value, current: &str) -> Result<(), ToolResult> {
    match input["revision"].as_str().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) if r != current => Err(ToolResult::err(format!("文件在你读取之后已经变化（revision {r} → {current}），请重新读取后再修改"))),
        _ => Ok(()),
    }
}

const PATH_DESC: &str = "工作区文件路径（如 /项目A/报告.docx）或文件 id";
const SAVE_AS_DESC: &str = "可选：另存为新文件（文件名或完整路径），不改原文件；不传则原地保存（可撤销）";
const REVISION_DESC: &str = "读取时返回的 revision；传了会检查文件在读取后没被改过";

// ───────────────────────── Word ─────────────────────────

pub struct DocsReadTool;

#[async_trait]
impl OfficeTool for DocsReadTool {
    fn name(&self) -> &str {
        "docs_read"
    }
    fn description(&self) -> &str {
        "读取工作区里的 Word（.docx）结构：按块列出段落、标题、列表项和表格，每块带序号，并给出标题大纲和 revision。修改已有 Word 文档前先用它看清要改的块序号；长文档用 from / to 分段读取。"
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
                "path": { "type": "string", "description": PATH_DESC },
                "from": { "type": "integer", "minimum": 0, "description": "从第几块开始，默认 0" },
                "to": { "type": "integer", "minimum": 0, "description": "读到第几块（含），默认 from+79" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("").to_string();
        let (_, path, bytes) = match load(ctx, &key, Kind::Docx).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        progress(ctx, "读取文档", format!("正在读取 {path}"));
        let from = input["from"].as_u64().unwrap_or(0) as usize;
        let to = input["to"].as_u64().map(|t| t as usize).unwrap_or(from + 79);
        let result = blocking(move || {
            let doc = docx::Document::open(bytes).map_err(|e| e.to_string())?;
            let info = doc.info().map_err(|e| e.to_string())?;
            let blocks = doc.blocks().map_err(|e| e.to_string())?;
            Ok((info, blocks))
        })
        .await;
        let (info, blocks) = match result {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("无法解析 {path}：{e}")),
        };
        let end = to.min(blocks.len().saturating_sub(1));
        let slice = if from < blocks.len() { &blocks[from..=end] } else { &[][..] };
        let outline: Vec<String> = info.headings.iter().take(60).map(|h| format!("{}[{}] {}", "  ".repeat(h.level.saturating_sub(1) as usize), h.index, h.text)).collect();
        let more = if end + 1 < blocks.len() { format!("\n…共 {} 块，继续读取请传 from={}", blocks.len(), end + 1) } else { String::new() };
        let observation = format!(
            "{path}：{} 块（{} 个表格、{} 张图片{}{}），revision={}\n大纲：\n{}\n\n块 {from}–{end}：\n{}{more}",
            info.blocks,
            info.tables,
            info.images,
            if info.has_revisions { "，含修订" } else { "" },
            if info.comments > 0 { format!("，{} 条批注", info.comments) } else { String::new() },
            info.revision,
            if outline.is_empty() { "（无标题）".to_string() } else { outline.join("\n") },
            docx::format_blocks(slice, 400),
        );
        ToolResult::ok(observation, vec![]).with_data(json!({ "path": path, "info": info }))
    }
}

pub struct DocsApplyTool;

#[async_trait]
impl OfficeTool for DocsApplyTool {
    fn name(&self) -> &str {
        "docs_apply"
    }
    fn description(&self) -> &str {
        "原格式修改工作区里已有的 Word（.docx）：提交一组操作，只改涉及的段落，其余格式、图片、批注原样保留，原地保存可撤销。块序号都指 docs_read 读到的序号（同一批里插入、删除不会让序号错位）。用户要求修订模式 / 留痕 / 让别人审阅时设 track_changes=true，改动会成为 Word 修订。改写已有文档时用它，不要重新生成一份新文档。"
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
                "path": { "type": "string", "description": PATH_DESC },
                "ops": {
                    "type": "array",
                    "description": "按顺序执行的操作。示例：{\"op\":\"replace_text\",\"block\":3,\"find\":\"2025 年\",\"replace\":\"2026 年\"}（不给 block 则全文替换）；{\"op\":\"replace_block\",\"block\":5,\"text\":\"新的段落，支持 **加粗**\"}；{\"op\":\"insert\",\"after\":7,\"markdown\":\"## 新章节\\n正文\\n- 要点\\n| 列1 | 列2 |\\n|---|---|\\n| a | b |\"}（也可 before 或 at:\"end\"）；{\"op\":\"delete\",\"block\":9} 或 {\"op\":\"delete\",\"from\":9,\"to\":12}；{\"op\":\"move\",\"block\":4,\"after\":10}；{\"op\":\"set_style\",\"block\":2,\"style\":\"heading 2\"}（heading 1–6 / title / normal / quote）；{\"op\":\"set_cell\",\"block\":6,\"row\":1,\"col\":2,\"text\":\"250\"}（行列从 0 开始）；{\"op\":\"comment\",\"block\":3,\"text\":\"批注内容\"}",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": { "type": "string", "enum": ["replace_text", "replace_block", "insert", "delete", "move", "set_style", "set_cell", "comment"] },
                            "block": { "type": "integer" },
                            "find": { "type": "string" },
                            "replace": { "type": "string" },
                            "all": { "type": "boolean" },
                            "text": { "type": "string" },
                            "style": { "type": "string" },
                            "markdown": { "type": "string" },
                            "after": { "type": "integer" },
                            "before": { "type": "integer" },
                            "at": { "type": "string", "enum": ["start", "end"] },
                            "from": { "type": "integer" },
                            "to": { "type": "integer" },
                            "row": { "type": "integer" },
                            "col": { "type": "integer" }
                        },
                        "required": ["op"]
                    }
                },
                "track_changes": { "type": "boolean", "description": "写成 Word 修订（可在 Word 里逐条接受或拒绝），默认 false" },
                "revision": { "type": "string", "description": REVISION_DESC },
                "save_as": { "type": "string", "description": SAVE_AS_DESC }
            },
            "required": ["path", "ops"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("").to_string();
        let ops_value = match ops_array(&input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let ops: Vec<docx::DocOp> = match serde_json::from_value(ops_value) {
            Ok(ops) => ops,
            Err(e) => return ToolResult::err(format!("操作格式不正确：{e}")),
        };
        let (file, path, bytes) = match load(ctx, &key, Kind::Docx).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        progress(ctx, "修改文档", format!("正在修改 {path}（{} 个操作）", ops.len()));
        let opts = docx::ApplyOptions { track_changes: input["track_changes"].as_bool().unwrap_or(false), ..Default::default() };
        let expected = input.clone();
        let result = blocking(move || {
            let mut doc = docx::Document::open(bytes).map_err(|e| e.to_string())?;
            check_revision(&expected, &doc.revision()).map_err(|e| e.observation)?;
            let report = doc.apply(&ops, &opts).map_err(|e| e.to_string())?;
            Ok((doc.save().map_err(|e| e.to_string())?, report))
        })
        .await;
        match result {
            Ok((out, report)) => save(ctx, &file, &path, Kind::Docx, out, report, input["save_as"].as_str()).await,
            Err(e) => ToolResult::err(format!("{e}（文件未改动）")),
        }
    }
}

// ───────────────────────── Excel ─────────────────────────

pub struct SheetReadTool;

#[async_trait]
impl OfficeTool for SheetReadTool {
    fn name(&self) -> &str {
        "sheet_read"
    }
    fn description(&self) -> &str {
        "读取工作区里 Excel（.xlsx）的工作表和单元格：列出所有工作表（已用区域、公式数、图表数），并按区域显示单元格的值和公式（公式显示为 值〔=公式〕）。修改已有 Excel 前先用它确认单元格位置；大表用 range 分段读取。做统计分析请用 data_profile / data_query。"
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
                "path": { "type": "string", "description": PATH_DESC },
                "sheet": { "type": "string", "description": "工作表名，默认第一个" },
                "range": { "type": "string", "description": "区域，如 A1:H40；默认从已用区域开头读" },
                "max_rows": { "type": "integer", "minimum": 1, "maximum": 300, "description": "最多读取行数，默认 60" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("").to_string();
        let (_, path, bytes) = match load(ctx, &key, Kind::Xlsx).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        progress(ctx, "读取表格", format!("正在读取 {path}"));
        let sheet = input["sheet"].as_str().map(str::to_string);
        let range = input["range"].as_str().map(str::to_string);
        let max_rows = input["max_rows"].as_u64().unwrap_or(60).clamp(1, 300) as u32;
        let result = blocking(move || {
            let wb = xlsx::Workbook::open(bytes).map_err(|e| e.to_string())?;
            let info = wb.info().map_err(|e| e.to_string())?;
            let view = wb.read(sheet.as_deref(), range.as_deref(), max_rows).map_err(|e| e.to_string())?;
            Ok((info, view))
        })
        .await;
        let (info, view) = match result {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("无法读取 {path}：{e}")),
        };
        let sheets: Vec<String> = info
            .sheets
            .iter()
            .map(|s| format!("- {}{}：已用区域 {}，{} 个公式{}", s.name, if s.hidden { "（隐藏）" } else { "" }, if s.dimension.is_empty() { "空".into() } else { s.dimension.clone() }, s.formulas, if s.charts > 0 { format!("，{} 个图表", s.charts) } else { String::new() }))
            .collect();
        let observation = format!("{path}，revision={}\n工作表：\n{}\n\n{}", info.revision, sheets.join("\n"), xlsx::format_view(&view, 60));
        ToolResult::ok(observation, vec![]).with_data(json!({ "path": path, "info": info, "range": view.range }))
    }
}

pub struct SheetApplyTool;

#[async_trait]
impl OfficeTool for SheetApplyTool {
    fn name(&self) -> &str {
        "sheet_apply"
    }
    fn description(&self) -> &str {
        "原格式修改工作区里已有的 Excel（.xlsx）：写入值或公式（以 = 开头的保持为实时公式，同时算好结果）、清空区域、新增或重命名工作表、插入柱形 / 条形 / 折线 / 饼图。只改写涉及的单元格，样式、其它工作表原样保留；依赖改动单元格的原有公式会一起更新结果。原地保存可撤销。汇总、统计类需求优先写公式（SUM、SUMIF、AVERAGE 等），不要写死计算结果。"
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
                "path": { "type": "string", "description": PATH_DESC },
                "ops": {
                    "type": "array",
                    "description": "按顺序执行的操作。示例：{\"op\":\"set\",\"sheet\":\"Sheet1\",\"cell\":\"D2\",\"value\":\"=B2*C2\"}；{\"op\":\"set_range\",\"sheet\":\"汇总\",\"start\":\"A1\",\"values\":[[\"地区\",\"金额\"],[\"华东\",\"=SUMIF(订单!A:A,A2,订单!C:C)\"]]}；{\"op\":\"clear\",\"sheet\":\"Sheet1\",\"range\":\"A10:F20\"}；{\"op\":\"add_sheet\",\"name\":\"汇总\",\"values\":[[...]]}；{\"op\":\"rename_sheet\",\"sheet\":\"Sheet1\",\"name\":\"订单\"}；{\"op\":\"add_chart\",\"sheet\":\"汇总\",\"type\":\"column\",\"title\":\"各地区金额\",\"categories\":\"汇总!A2:A5\",\"series\":[{\"name\":\"金额\",\"values\":\"汇总!B2:B5\"}],\"anchor\":\"D2\"}（type 可为 column / bar / line / pie）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": { "type": "string", "enum": ["set", "set_range", "clear", "add_sheet", "rename_sheet", "add_chart"] },
                            "sheet": { "type": "string" },
                            "cell": { "type": "string" },
                            "value": {},
                            "start": { "type": "string" },
                            "values": { "type": "array", "items": { "type": "array", "items": {} } },
                            "range": { "type": "string" },
                            "name": { "type": "string" },
                            "type": { "type": "string" },
                            "title": { "type": "string" },
                            "categories": { "type": "string" },
                            "series": { "type": "array", "items": { "type": "object", "properties": { "name": { "type": "string" }, "values": { "type": "string" } }, "required": ["values"] } },
                            "anchor": { "type": "string" },
                            "width": { "type": "integer" },
                            "height": { "type": "integer" }
                        },
                        "required": ["op"]
                    }
                },
                "revision": { "type": "string", "description": REVISION_DESC },
                "save_as": { "type": "string", "description": SAVE_AS_DESC }
            },
            "required": ["path", "ops"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("").to_string();
        let ops_value = match ops_array(&input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let ops: Vec<xlsx::SheetOp> = match serde_json::from_value(ops_value) {
            Ok(ops) => ops,
            Err(e) => return ToolResult::err(format!("操作格式不正确：{e}")),
        };
        let (file, path, bytes) = match load(ctx, &key, Kind::Xlsx).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        progress(ctx, "修改表格", format!("正在修改 {path}（{} 个操作）", ops.len()));
        let expected = input.clone();
        let result = blocking(move || {
            let mut wb = xlsx::Workbook::open(bytes).map_err(|e| e.to_string())?;
            check_revision(&expected, &wb.revision().map_err(|e| e.to_string())?).map_err(|e| e.observation)?;
            let report = wb.apply(&ops).map_err(|e| e.to_string())?;
            Ok((wb.save().map_err(|e| e.to_string())?, report))
        })
        .await;
        match result {
            Ok((out, report)) => save(ctx, &file, &path, Kind::Xlsx, out, report, input["save_as"].as_str()).await,
            Err(e) => ToolResult::err(format!("{e}（文件未改动）")),
        }
    }
}

// ───────────────────────── PPT ─────────────────────────

pub struct SlidesReadTool;

#[async_trait]
impl OfficeTool for SlidesReadTool {
    fn name(&self) -> &str {
        "slides_read"
    }
    fn description(&self) -> &str {
        "读取工作区里 PPT（.pptx）每一页的版式、形状（带 id 和名称：标题、正文、文本框、图片、表格、图表）、文字和备注。修改已有 PPT 前先用它确认页码和形状。"
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
                "path": { "type": "string", "description": PATH_DESC },
                "from": { "type": "integer", "minimum": 1, "description": "从第几页开始，默认 1" },
                "to": { "type": "integer", "minimum": 1, "description": "读到第几页（含），默认 from+29" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("").to_string();
        let (_, path, bytes) = match load(ctx, &key, Kind::Pptx).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        progress(ctx, "读取演示文稿", format!("正在读取 {path}"));
        let from = input["from"].as_u64().unwrap_or(1).max(1) as usize;
        let to = input["to"].as_u64().map(|t| t as usize).unwrap_or(from + 29);
        let result = blocking(move || {
            let deck = pptx::Deck::open(bytes).map_err(|e| e.to_string())?;
            Ok((deck.info().map_err(|e| e.to_string())?, deck.slides().map_err(|e| e.to_string())?))
        })
        .await;
        let (info, slides) = match result {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("无法读取 {path}：{e}")),
        };
        let selected: Vec<pptx::Slide> = slides.into_iter().filter(|s| s.index >= from && s.index <= to).collect();
        let more = if to < info.slides { format!("\n…共 {} 页，继续读取请传 from={}", info.slides, to + 1) } else { String::new() };
        let observation = format!("{path}：{} 页，页面 {}×{} 磅，revision={}\n{}{more}", info.slides, info.width_pt, info.height_pt, info.revision, pptx::format_slides(&selected, 300));
        ToolResult::ok(observation, vec![]).with_data(json!({ "path": path, "info": info }))
    }
}

pub struct SlidesApplyTool;

#[async_trait]
impl OfficeTool for SlidesApplyTool {
    fn name(&self) -> &str {
        "slides_apply"
    }
    fn description(&self) -> &str {
        "原格式修改工作区里已有的 PPT（.pptx）：查找替换文字、重写某个形状的文字（沿用原字体格式）、改表格单元格、改备注，复制、删除、调整页面顺序。页码从 1 开始，同一批里都指 slides_read 读到的页码。母版、版式、图片原样保留，原地保存可撤销。改已有 PPT 时用它，不要重新生成整套 PPT。"
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
                "path": { "type": "string", "description": PATH_DESC },
                "ops": {
                    "type": "array",
                    "description": "按顺序执行的操作。示例：{\"op\":\"replace_text\",\"find\":\"2025\",\"replace\":\"2026\"}（可加 slide 限定页）；{\"op\":\"set_text\",\"slide\":2,\"shape\":\"body\",\"text\":\"第一点\\n  子要点\\n第二点 **重点**\"}（shape 可以是 slides_read 里的形状 id、名称，或 title / subtitle / body；每行一段，行首两个空格缩进一级）；{\"op\":\"set_cell\",\"slide\":3,\"shape\":\"5\",\"row\":1,\"col\":1,\"text\":\"3.5 亿\"}；{\"op\":\"set_notes\",\"slide\":2,\"text\":\"讲稿\"}；{\"op\":\"duplicate_slide\",\"slide\":2}；{\"op\":\"delete_slide\",\"slide\":4}；{\"op\":\"move_slide\",\"slide\":5,\"to\":2}",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": { "type": "string", "enum": ["replace_text", "set_text", "set_cell", "set_notes", "duplicate_slide", "delete_slide", "move_slide"] },
                            "slide": { "type": "integer" },
                            "shape": { "type": "string" },
                            "find": { "type": "string" },
                            "replace": { "type": "string" },
                            "all": { "type": "boolean" },
                            "text": { "type": "string" },
                            "row": { "type": "integer" },
                            "col": { "type": "integer" },
                            "after": { "type": "integer" },
                            "to": { "type": "integer" }
                        },
                        "required": ["op"]
                    }
                },
                "revision": { "type": "string", "description": REVISION_DESC },
                "save_as": { "type": "string", "description": SAVE_AS_DESC }
            },
            "required": ["path", "ops"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let key = input["path"].as_str().unwrap_or("").to_string();
        let ops_value = match ops_array(&input) {
            Ok(v) => v,
            Err(e) => return e,
        };
        // 形状 id 可能被写成数字
        let ops_value = normalize_shape_ids(ops_value);
        let ops: Vec<pptx::SlideOp> = match serde_json::from_value(ops_value) {
            Ok(ops) => ops,
            Err(e) => return ToolResult::err(format!("操作格式不正确：{e}")),
        };
        let (file, path, bytes) = match load(ctx, &key, Kind::Pptx).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        progress(ctx, "修改演示文稿", format!("正在修改 {path}（{} 个操作）", ops.len()));
        let expected = input.clone();
        let result = blocking(move || {
            let mut deck = pptx::Deck::open(bytes).map_err(|e| e.to_string())?;
            check_revision(&expected, &deck.revision().map_err(|e| e.to_string())?).map_err(|e| e.observation)?;
            let report = deck.apply(&ops).map_err(|e| e.to_string())?;
            Ok((deck.save().map_err(|e| e.to_string())?, report))
        })
        .await;
        match result {
            Ok((out, report)) => save(ctx, &file, &path, Kind::Pptx, out, report, input["save_as"].as_str()).await,
            Err(e) => ToolResult::err(format!("{e}（文件未改动）")),
        }
    }
}

fn normalize_shape_ids(mut ops: Value) -> Value {
    if let Some(list) = ops.as_array_mut() {
        for op in list {
            if let Some(n) = op.get("shape").and_then(Value::as_i64) {
                op["shape"] = Value::String(n.to_string());
            }
        }
    }
    ops
}
