//! MCP 工具：每个工具对应一个 officepilot 命令，参数可以内联（客户端没有文件系统也能用）

use std::path::Path;

use serde_json::{json, Value};

use super::Workspace;
use crate::{convert, deck_cmd, guides, io, office, Done, Fail, Out};

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
    pub read_only: bool,
    /// 会覆盖已有文件
    pub destructive: bool,
    /// 只在本机（stdio）提供
    pub local_only: bool,
    pub run: fn(&Workspace, &Value) -> Out,
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string).filter(|s| !s.trim().is_empty())
}

fn req(v: &Value, key: &str) -> Result<String, Fail> {
    s(v, key).ok_or_else(|| Fail::new(format!("缺少参数 {key}")))
}

fn n(v: &Value, key: &str) -> Option<usize> {
    v.get(key).and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).map(|x| x as usize)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// JSON 参数：对象 / 数组直接用，字符串当 JSON 解析
fn json_arg(v: &Value, key: &str) -> Result<Value, Fail> {
    match v.get(key) {
        Some(Value::String(t)) => serde_json::from_str(t).map_err(|e| Fail::new(format!("{key} 不是合法 JSON：{e}"))),
        Some(Value::Null) | None => Err(Fail::new(format!("缺少参数 {key}"))),
        Some(other) => Ok(other.clone()),
    }
}

/// 规格里引用的图片路径：解析并检查范围后改成绝对路径
fn confine_paths(ws: &Workspace, value: &mut Value, base: &Path) -> Result<(), Fail> {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if k == "path" {
                    if let Value::String(p) = v {
                        let rel = if Path::new(p.as_str()).is_absolute() || p.starts_with("http") { p.clone() } else { base.join(p.as_str()).to_string_lossy().into_owned() };
                        *p = ws.resolve_str(&rel)?;
                    }
                } else {
                    confine_paths(ws, v, base)?;
                }
            }
        }
        Value::Array(list) => {
            for v in list {
                confine_paths(ws, v, base)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn apply_kind(ws: &Workspace, v: &Value, ext: &[&str], kind: &str) -> Out {
    let file = ws.resolve_str(&req(v, "file")?)?;
    let lower = file.to_lowercase();
    if !ext.iter().any(|e| lower.ends_with(e)) {
        return Err(Fail::new(format!("{file} 不是 {kind} 文件")).hint("Word 用 docs_apply，Excel 用 sheet_apply，PPT 用 slides_apply"));
    }
    let ops = json_arg(v, "ops")?;
    let out = s(v, "out").map(|o| ws.resolve_str(&o)).transpose()?;
    office::apply(&file, &ops.to_string(), flag(v, "track_changes"), &s(v, "author").unwrap_or_else(|| "OfficePilot AI".into()), out.as_deref(), flag(v, "dry_run"), s(v, "revision").as_deref())
}

fn file_schema(extra: Value) -> Value {
    let mut props = json!({ "file": { "type": "string", "description": "文件路径（相对路径以工作目录为准）；HTTP 模式也可以是 /files/ 上传后返回的 url" } });
    if let (Some(p), Some(e)) = (props.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            p.insert(k.clone(), v.clone());
        }
    }
    json!({ "type": "object", "properties": props, "required": ["file"] })
}

const OPS_COMMON: &str = "按顺序执行的一批操作（JSON 数组），要么全部生效，要么文件不变。完整说明见资源 ";

pub fn all() -> Vec<Tool> {
    vec![
        Tool {
            name: "capabilities",
            description: "列出 officepilot 能做什么：支持的读写格式、转换方向、检查工具。开始前调用一次。",
            schema: || json!({ "type": "object", "properties": {} }),
            read_only: true,
            destructive: false,
            local_only: false,
            run: |_, _| guides::capabilities(),
        },
        Tool {
            name: "guide",
            description: "读取使用说明：overview（总览）/ docs（Word 操作）/ sheet（Excel 操作）/ slides（PPT 操作）/ deck（分阶段做 PPT、版式与设计建议）/ formulas（公式）。写操作前先看对应主题。",
            schema: || json!({ "type": "object", "properties": { "topic": { "type": "string", "enum": ["overview", "docs", "sheet", "slides", "deck", "formulas"] } } }),
            read_only: true,
            destructive: false,
            local_only: false,
            run: |_, v| guides::guide(s(v, "topic").as_deref()),
        },
        Tool {
            name: "info",
            description: "Office 文件概况：Word 的块数与标题大纲、Excel 的工作表与公式数、PPT 的页数与每页标题。",
            schema: || file_schema(json!({})),
            read_only: true,
            destructive: false,
            local_only: false,
            run: |ws, v| office::info(&ws.resolve_str(&req(v, "file")?)?),
        },
        Tool {
            name: "read",
            description: "结构化读取 .docx / .xlsx / .pptx：Word 按块（带序号）、Excel 按区域（值与公式）、PPT 按页（形状 id、文字、备注）。返回 revision，修改时传回可防止基于过期内容修改。修改前必须先读。",
            schema: || file_schema(json!({
                "from": { "type": "integer", "description": "Word：起始块序号；PPT：起始页码" },
                "to": { "type": "integer", "description": "Word：结束块序号；PPT：结束页码（含）" },
                "sheet": { "type": "string", "description": "Excel：工作表名" },
                "range": { "type": "string", "description": "Excel：区域，如 A1:H40" },
                "max_rows": { "type": "integer", "description": "Excel：最多读取行数，默认 200" }
            })),
            read_only: true,
            destructive: false,
            local_only: false,
            run: |ws, v| office::read(&ws.resolve_str(&req(v, "file")?)?, n(v, "from"), n(v, "to"), s(v, "sheet").as_deref(), s(v, "range").as_deref(), n(v, "max_rows").unwrap_or(200) as u32),
        },
        Tool {
            name: "docs_apply",
            description: "原格式修改 Word（.docx）：replace_text / replace_block / insert（Markdown）/ delete / move / set_style / set_cell / comment。块序号都指 read 读到的序号。track_changes=true 写成 Word 修订。默认原地保存，out 另存，dry_run 只预览。",
            schema: || file_schema(json!({
                "ops": { "type": "array", "items": { "type": "object" }, "description": format!("{OPS_COMMON}officepilot://guide/docs。示例：[{{\"op\":\"replace_text\",\"block\":3,\"find\":\"2025\",\"replace\":\"2026\"}},{{\"op\":\"insert\",\"after\":7,\"markdown\":\"## 新章节\\n- 要点\"}}]") },
                "track_changes": { "type": "boolean" },
                "author": { "type": "string" },
                "out": { "type": "string", "description": "另存路径" },
                "dry_run": { "type": "boolean" },
                "revision": { "type": "string" }
            })),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| apply_kind(ws, v, &[".docx", ".docm", ".dotx"], "Word"),
        },
        Tool {
            name: "sheet_apply",
            description: "原格式修改 Excel（.xlsx）：set / set_range（= 开头为实时公式，同时算好结果）/ clear / add_sheet / rename_sheet / format（加粗、颜色、数字格式、对齐）/ column_width / freeze / add_chart。汇总统计请写公式，不要写死结果。",
            schema: || file_schema(json!({
                "ops": { "type": "array", "items": { "type": "object" }, "description": format!("{OPS_COMMON}officepilot://guide/sheet。示例：[{{\"op\":\"set_range\",\"sheet\":\"汇总\",\"start\":\"A1\",\"values\":[[\"地区\",\"金额\"],[\"华东\",\"=SUMIF(订单!A:A,A2,订单!C:C)\"]]}}]") },
                "out": { "type": "string" },
                "dry_run": { "type": "boolean" },
                "revision": { "type": "string" }
            })),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| apply_kind(ws, v, &[".xlsx", ".xlsm", ".xltx"], "Excel"),
        },
        Tool {
            name: "slides_apply",
            description: "原格式修改 PPT（.pptx）：replace_text / set_text（形状 id、名称或 title/subtitle/body）/ set_cell / set_notes / duplicate_slide / delete_slide / move_slide。页码从 1 开始，指 read 读到的页码。",
            schema: || file_schema(json!({
                "ops": { "type": "array", "items": { "type": "object" }, "description": format!("{OPS_COMMON}officepilot://guide/slides。示例：[{{\"op\":\"set_text\",\"slide\":2,\"shape\":\"body\",\"text\":\"第一点\\n  子要点\"}}]") },
                "out": { "type": "string" },
                "dry_run": { "type": "boolean" },
                "revision": { "type": "string" }
            })),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| apply_kind(ws, v, &[".pptx", ".pptm", ".potx"], "PPT"),
        },
        Tool {
            name: "create_docx",
            description: "用 Markdown 新建 Word：标题、段落、无序 / 有序列表、表格、引用、代码、链接、**加粗** *斜体*，套用内置的标题与正文样式。",
            schema: || json!({ "type": "object", "properties": {
                "out": { "type": "string", "description": "输出路径，如 报告.docx" },
                "markdown": { "type": "string" },
                "title": { "type": "string", "description": "文档标题（可选）" }
            }, "required": ["out", "markdown"] }),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| {
                let out = ws.resolve_str(&req(v, "out")?)?;
                let bytes = office::markdown_to_docx(&s(v, "markdown").unwrap_or_default(), s(v, "title").as_deref())?;
                io::write_atomic(&out, &bytes)?;
                Ok(Done::ok(format!("已创建 {out}"), json!({ "path": out })))
            },
        },
        Tool {
            name: "create_xlsx",
            description: "用数据新建 Excel：data 为二维数组，或 {\"sheets\":[{\"name\",\"rows\",\"formats\",\"charts\",\"widths\",\"freeze\"}]}。= 开头的单元格保持为实时公式；表头自动加粗、冻结首行、按内容设列宽。",
            schema: || json!({ "type": "object", "properties": {
                "out": { "type": "string" },
                "data": { "description": "二维数组或 {sheets:[...]}，格式见 officepilot://guide/sheet" }
            }, "required": ["out", "data"] }),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| {
                let out = ws.resolve_str(&req(v, "out")?)?;
                let (bytes, sheets) = office::build_xlsx(json_arg(v, "data")?)?;
                io::write_atomic(&out, &bytes)?;
                Ok(Done::ok(format!("已创建 {out}（{sheets} 个工作表）"), json!({ "path": out, "sheets": sheets })))
            },
        },
        Tool {
            name: "create_pptx",
            description: "一次生成整套 PPT：spec = {\"title\",\"style\",\"pages\":[页面规格…]}。页数多或需要逐页把关时用 deck_start / deck_page / deck_build。版式与字段见 officepilot://guide/deck。",
            schema: || json!({ "type": "object", "properties": {
                "out": { "type": "string" },
                "spec": { "type": "object", "description": "{title, style, pages}" }
            }, "required": ["out", "spec"] }),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| {
                let out = ws.resolve_str(&req(v, "out")?)?;
                let mut spec = json_arg(v, "spec")?;
                confine_paths(ws, &mut spec, &ws.root)?;
                deck_cmd::create_pptx(&spec.to_string(), None, None, &out)
            },
        },
        Tool {
            name: "convert",
            description: "本地转换：docx/pptx → md/txt/html，xlsx → csv/md/html，md/txt → docx，md → html，csv → xlsx，pdf → txt/md（需要本机有 pdftotext）。",
            schema: || file_schema(json!({
                "to": { "type": "string", "enum": ["md", "txt", "html", "csv", "docx", "xlsx"] },
                "out": { "type": "string", "description": "输出路径，默认同名换扩展名" }
            })),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| {
                let out = s(v, "out").map(|o| ws.resolve_str(&o)).transpose()?;
                convert::convert(&ws.resolve_str(&req(v, "file")?)?, &req(v, "to")?, out.as_deref())
            },
        },
        Tool {
            name: "slides_check",
            description: "不渲染地检查一个页面规格（或大纲）：文字溢出、元素重叠、超出页面、字号过小、配色表外的颜色、与大纲不一致。返回 error（必须改）和 warning（建议改）。",
            schema: || json!({ "type": "object", "properties": {
                "spec": { "type": "object", "description": "页面规格，或大纲 {title, pages}" },
                "style": { "type": "object" },
                "outline": { "type": "object" },
                "page": { "type": "integer", "description": "对照大纲第几页" }
            }, "required": ["spec"] }),
            read_only: true,
            destructive: false,
            local_only: false,
            run: |ws, v| {
                let mut spec = json_arg(v, "spec")?;
                confine_paths(ws, &mut spec, &ws.root)?;
                let style = json_arg(v, "style").ok().map(|x| x.to_string());
                let outline = json_arg(v, "outline").ok().map(|x| x.to_string());
                deck_cmd::check(&spec.to_string(), style.as_deref(), outline.as_deref(), n(v, "page"))
            },
        },
        Tool {
            name: "slides_audit",
            description: "检查已有 .pptx 的版面：文字可能放不下、形状重叠、超出页面、字号过小。",
            schema: || file_schema(json!({})),
            read_only: true,
            destructive: false,
            local_only: false,
            run: |ws, v| deck_cmd::audit(&ws.resolve_str(&req(v, "file")?)?),
        },
        Tool {
            name: "deck_start",
            description: "分阶段做 PPT 的第一步：在 dir 写入风格表（尺寸、配色、字体、字号）和大纲（每页标题），检查通过才写入。之后每页用 deck_page 提交。",
            schema: || json!({ "type": "object", "properties": {
                "dir": { "type": "string", "description": "工作目录，如 deck" },
                "style": { "type": "object", "description": "{size, palette{background,surface,text,muted,accent,extra}, fonts{heading,body}, title_size, body_size}" },
                "outline": { "type": "object", "description": "{title, pages:[{title, kind?, points?}]}" }
            }, "required": ["dir", "style", "outline"] }),
            read_only: false,
            destructive: false,
            local_only: false,
            run: |ws, v| deck_cmd::start(&ws.resolve_str(&req(v, "dir")?)?, &json_arg(v, "style")?.to_string(), &json_arg(v, "outline")?.to_string()),
        },
        Tool {
            name: "deck_page",
            description: "提交第 n 页（从 1 开始）的页面规格：对照风格表和大纲检查，没有 error 才保存；有 error 时按提示修改后重交。",
            schema: || json!({ "type": "object", "properties": {
                "dir": { "type": "string" },
                "n": { "type": "integer" },
                "page": { "type": "object", "description": "页面规格：{layout, title, subtitle, bullets, left, right, metrics, table, chart, image, quote, elements, notes}" }
            }, "required": ["dir", "n", "page"] }),
            read_only: false,
            destructive: false,
            local_only: false,
            run: |ws, v| deck_page(ws, v, false),
        },
        Tool {
            name: "deck_replace",
            description: "重做已提交的第 n 页，检查通过后自动重新组装上次 deck_build 输出的文件。",
            schema: || json!({ "type": "object", "properties": {
                "dir": { "type": "string" }, "n": { "type": "integer" }, "page": { "type": "object" }
            }, "required": ["dir", "n", "page"] }),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| deck_page(ws, v, true),
        },
        Tool {
            name: "deck_status",
            description: "查看 deck 工作目录里哪些页已提交、还差哪些。",
            schema: || json!({ "type": "object", "properties": { "dir": { "type": "string" } }, "required": ["dir"] }),
            read_only: true,
            destructive: false,
            local_only: false,
            run: |ws, v| deck_cmd::status(&ws.resolve_str(&req(v, "dir")?)?),
        },
        Tool {
            name: "deck_build",
            description: "把已提交的页面组装成 .pptx（原生表格、可编辑图表、演讲者备注），并审查版面。",
            schema: || json!({ "type": "object", "properties": {
                "dir": { "type": "string" }, "out": { "type": "string" }, "allow_missing": { "type": "boolean" }
            }, "required": ["dir", "out"] }),
            read_only: false,
            destructive: true,
            local_only: false,
            run: |ws, v| deck_cmd::build(&ws.resolve_str(&req(v, "dir")?)?, &ws.resolve_str(&req(v, "out")?)?, flag(v, "allow_missing")),
        },
        Tool {
            name: "open",
            description: "用 OfficePilot 桌面版或系统默认程序打开文件，交给用户查看。",
            schema: || file_schema(json!({})),
            read_only: true,
            destructive: false,
            local_only: true,
            run: |ws, v| io::open(&ws.resolve_str(&req(v, "file")?)?),
        },
    ]
}

fn deck_page(ws: &Workspace, v: &Value, replace: bool) -> Out {
    let dir = ws.resolve(&req(v, "dir")?)?;
    let n = n(v, "n").ok_or_else(|| Fail::new("缺少参数 n（页码）"))?;
    let mut page = json_arg(v, "page")?;
    confine_paths(ws, &mut page, &dir)?;
    deck_cmd::page(&dir.to_string_lossy(), n, &page.to_string(), replace)
}
