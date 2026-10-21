//! info / read / apply / create

use officepilot_engine::build::templates;
use officepilot_engine::{docx, pptx, xlsx, ApplyReport, Kind};
use serde_json::{json, Value};

use crate::io::{json_input, read_file, text_input, write_atomic};
use crate::{deck_cmd, Done, Fail, Out};

pub fn kind_of(path: &str, bytes: &[u8]) -> Result<Kind, Fail> {
    Kind::from_name(path).or_else(|| Kind::sniff(bytes)).ok_or_else(|| Fail::new(format!("{path} 不是 .docx / .xlsx / .pptx 文件")).hint("PDF、Markdown、CSV 等请用 officepilot convert"))
}

pub fn info(file: &str) -> Out {
    let bytes = read_file(file)?;
    match kind_of(file, &bytes)? {
        Kind::Docx => {
            let info = docx::Document::open(bytes)?.info()?;
            let outline: Vec<String> = info.headings.iter().take(20).map(|h| format!("{}[{}] {}", "  ".repeat(h.level.saturating_sub(1) as usize), h.index, h.text)).collect();
            Ok(Done::ok(
                format!("{file}：Word，{} 块（{} 段、{} 个表格、{} 张图片），{} 字{}{}\n{}", info.blocks, info.paragraphs, info.tables, info.images, info.characters, if info.has_revisions { "，含修订" } else { "" }, if info.comments > 0 { format!("，{} 条批注", info.comments) } else { String::new() }, outline.join("\n")),
                json!({ "type": "docx", "info": info }),
            ))
        }
        Kind::Xlsx => {
            let info = xlsx::Workbook::open(bytes)?.info()?;
            let sheets: Vec<String> = info.sheets.iter().map(|s| format!("- {}：{}，{} 个公式{}", s.name, if s.dimension.is_empty() { "空".into() } else { s.dimension.clone() }, s.formulas, if s.charts > 0 { format!("，{} 个图表", s.charts) } else { String::new() })).collect();
            Ok(Done::ok(format!("{file}：Excel，{} 个工作表\n{}", info.sheets.len(), sheets.join("\n")), json!({ "type": "xlsx", "info": info })))
        }
        Kind::Pptx => {
            let info = pptx::Deck::open(bytes)?.info()?;
            let titles: Vec<String> = info.titles.iter().enumerate().map(|(i, t)| format!("{}. {}", i + 1, if t.is_empty() { "（无标题）" } else { t })).collect();
            Ok(Done::ok(format!("{file}：PPT，{} 页（{}×{} 磅）\n{}", info.slides, info.width_pt, info.height_pt, titles.join("\n")), json!({ "type": "pptx", "info": info })))
        }
    }
}

pub fn read(file: &str, from: Option<usize>, to: Option<usize>, sheet: Option<&str>, range: Option<&str>, max_rows: u32) -> Out {
    let bytes = read_file(file)?;
    match kind_of(file, &bytes)? {
        Kind::Docx => {
            let doc = docx::Document::open(bytes)?;
            let blocks = doc.blocks()?;
            let from = from.unwrap_or(0);
            let to = to.unwrap_or(usize::MAX).min(blocks.len().saturating_sub(1));
            let slice: Vec<docx::Block> = blocks.into_iter().filter(|b| b.index >= from && b.index <= to).collect();
            Ok(Done::ok(format!("revision={}\n{}", doc.revision(), docx::format_blocks(&slice, 2000).trim_end()), json!({ "type": "docx", "revision": doc.revision(), "blocks": slice })))
        }
        Kind::Xlsx => {
            let wb = xlsx::Workbook::open(bytes)?;
            let view = wb.read(sheet, range, max_rows)?;
            let revision = wb.revision()?;
            let sheets: Vec<&str> = wb.sheets.iter().map(|s| s.name.as_str()).collect();
            Ok(Done::ok(format!("revision={revision}，工作表：{}\n{}", sheets.join("、"), xlsx::format_view(&view, 80).trim_end()), json!({ "type": "xlsx", "revision": revision, "sheets": sheets, "view": view })))
        }
        Kind::Pptx => {
            let deck = pptx::Deck::open(bytes)?;
            let from = from.unwrap_or(1);
            let to = to.unwrap_or(usize::MAX);
            let slides: Vec<pptx::Slide> = deck.slides()?.into_iter().filter(|s| s.index >= from && s.index <= to).collect();
            let revision = deck.revision()?;
            Ok(Done::ok(format!("revision={revision}\n{}", pptx::format_slides(&slides, 2000).trim_end()), json!({ "type": "pptx", "revision": revision, "slides": slides })))
        }
    }
}

fn parse_ops<T: serde::de::DeserializeOwned>(value: Value, guide: &str) -> Result<Vec<T>, Fail> {
    let list = match value {
        Value::Array(a) => a,
        Value::Object(ref o) if o.contains_key("ops") => match o["ops"].clone() {
            Value::Array(a) => a,
            _ => return Err(Fail::new("ops 必须是数组")),
        },
        Value::Object(_) => vec![value],
        _ => return Err(Fail::new("ops 必须是 JSON 数组")),
    };
    if list.is_empty() {
        return Err(Fail::new("ops 为空"));
    }
    list.into_iter()
        .enumerate()
        .map(|(i, v)| serde_json::from_value(v).map_err(|e| Fail::new(format!("第 {i} 个操作格式不对：{e}")).hint(format!("操作说明见 officepilot guide {guide}"))))
        .collect()
}

pub fn changes_summary(report: &ApplyReport) -> String {
    report
        .changes
        .iter()
        .map(|c| {
            let mut line = format!("- #{} {} {}", c.op, c.kind, c.target);
            if let Some(b) = &c.before {
                line.push_str(&format!("：“{}”", b.replace('\n', " ")));
            }
            if let Some(a) = &c.after {
                line.push_str(&format!(" → “{}”", a.replace('\n', " ")));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn check_revision(expected: Option<&str>, current: &str) -> Result<(), Fail> {
    match expected {
        Some(r) if r != current => Err(Fail::new(format!("文件在读取后已经变化（revision {r} → {current}）")).hint("重新 read 后再修改")),
        _ => Ok(()),
    }
}

pub fn apply(file: &str, ops: &str, track: bool, author: &str, out: Option<&str>, dry_run: bool, revision: Option<&str>) -> Out {
    let bytes = read_file(file)?;
    let ops_value = json_input(ops)?;
    let kind = kind_of(file, &bytes)?;
    let (saved, report) = match kind {
        Kind::Docx => {
            let ops: Vec<docx::DocOp> = parse_ops(ops_value, "docs")?;
            let mut doc = docx::Document::open(bytes)?;
            check_revision(revision, &doc.revision())?;
            let report = doc.apply(&ops, &docx::ApplyOptions { track_changes: track, author: author.to_string() })?;
            (doc.save()?, report)
        }
        Kind::Xlsx => {
            let ops: Vec<xlsx::SheetOp> = parse_ops(ops_value, "sheet")?;
            let mut wb = xlsx::Workbook::open(bytes)?;
            check_revision(revision, &wb.revision()?)?;
            let report = wb.apply(&ops)?;
            (wb.save()?, report)
        }
        Kind::Pptx => {
            let ops: Vec<pptx::SlideOp> = parse_ops(normalize_shape_ids(ops_value), "slides")?;
            let mut deck = pptx::Deck::open(bytes)?;
            check_revision(revision, &deck.revision()?)?;
            let report = deck.apply(&ops)?;
            (deck.save()?, report)
        }
    };
    let target = out.unwrap_or(file);
    let n = report.changes.len();
    if dry_run {
        return Ok(Done::ok(format!("检查通过，{n} 处改动（未写入）：\n{}", changes_summary(&report)), json!({ "dry_run": true, "changes": report.changes, "revision": report.revision })));
    }
    write_atomic(target, &saved)?;
    Ok(Done::ok(format!("已保存 {target}，{n} 处改动，revision={}\n{}", report.revision, changes_summary(&report)), json!({ "path": target, "changes": report.changes, "revision": report.revision })))
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

pub fn create(kind: &str, from: Option<&str>, spec: Option<&str>, outline: Option<&str>, style: Option<&str>, title: Option<&str>, out: &str) -> Out {
    match kind {
        "docx" => create_docx(from, title, out),
        "xlsx" => create_xlsx(from.ok_or_else(|| Fail::new("xlsx 需要 --from 数据文件（.json 或 .csv）"))?, out),
        _ => deck_cmd::create_pptx(from.or(spec).ok_or_else(|| Fail::new("pptx 需要 --spec（整套规格 .json 或页面目录）")).map_err(|e| e.hint("见 officepilot guide deck"))?, outline, style, out),
    }
}

pub fn markdown_to_docx(markdown: &str, title: Option<&str>) -> Result<Vec<u8>, Fail> {
    let mut doc = docx::Document::open(templates::docx(title)?)?;
    if !markdown.trim().is_empty() {
        doc.apply(&[docx::DocOp::Insert { after: None, before: None, at: Some("end".into()), markdown: markdown.to_string() }], &docx::ApplyOptions::default())?;
    }
    Ok(doc.save()?)
}

fn create_docx(from: Option<&str>, title: Option<&str>, out: &str) -> Out {
    let markdown = match from {
        Some(f) => text_input(f)?,
        None => String::new(),
    };
    let bytes = markdown_to_docx(&markdown, title)?;
    write_atomic(out, &bytes)?;
    let blocks = docx::Document::open(bytes)?.blocks()?.len();
    Ok(Done::ok(format!("已创建 {out}（{blocks} 块）"), json!({ "path": out, "blocks": blocks })))
}

/// 显示宽度：中日韩字符算 2
fn display_width(s: &str) -> usize {
    s.chars().map(|c| if (c as u32) >= 0x2E80 { 2 } else { 1 }).sum()
}

pub fn csv_rows(text: &str) -> Vec<Vec<Value>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    let delim = if text.lines().next().map(|l| l.matches('\t').count() > l.matches(',').count()).unwrap_or(false) { '\t' } else { ',' };
    let finish = |field: &mut String| -> Value {
        let f = std::mem::take(field);
        Value::String(f)
    };
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == delim && !quoted => row.push(finish(&mut field)),
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(finish(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(finish(&mut field));
        rows.push(row);
    }
    rows
}

/// 规格：{"sheets":[{"name","rows":[[..]],"header":true,"widths":{"A":12},"freeze":"A2","formats":[..],"charts":[..]}]}，
/// 也可以直接是二维数组或 CSV
pub fn build_xlsx(spec: Value) -> Result<(Vec<u8>, usize), Fail> {
    let sheets: Vec<Value> = match spec {
        Value::Array(rows) => vec![json!({ "name": "Sheet1", "rows": rows, "header": true })],
        Value::Object(ref o) if o.contains_key("sheets") => o["sheets"].as_array().cloned().ok_or_else(|| Fail::new("sheets 必须是数组"))?,
        Value::Object(ref o) if o.contains_key("rows") => vec![spec.clone()],
        _ => return Err(Fail::new("数据格式不对").hint("用二维数组，或 {\"sheets\":[{\"name\":\"销售\",\"rows\":[[\"地区\",\"金额\"],[\"华东\",100]]}]}")),
    };
    let names: Vec<String> = sheets.iter().enumerate().map(|(i, s)| s["name"].as_str().map(str::to_string).unwrap_or_else(|| format!("Sheet{}", i + 1))).collect();
    let mut wb = xlsx::Workbook::open(templates::xlsx(&names)?)?;
    let mut ops: Vec<Value> = Vec::new();
    let mut charts: Vec<Value> = Vec::new();
    for (sheet, name) in sheets.iter().zip(&names) {
        let rows = sheet["rows"].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            continue;
        }
        let cols = rows.iter().filter_map(|r| r.as_array().map(Vec::len)).max().unwrap_or(0);
        ops.push(json!({ "op": "set_range", "sheet": name, "start": "A1", "values": rows }));
        let header = sheet["header"].as_bool().unwrap_or(true);
        if header && cols > 0 {
            let last = officepilot_engine::formula::index_to_col(cols as u32);
            ops.push(json!({ "op": "format", "sheet": name, "range": format!("A1:{last}1"), "bold": true, "fill": "#F1F5F9" }));
            ops.push(json!({ "op": "freeze", "sheet": name, "cell": sheet["freeze"].as_str().unwrap_or("A2") }));
        } else if let Some(f) = sheet["freeze"].as_str() {
            ops.push(json!({ "op": "freeze", "sheet": name, "cell": f }));
        }
        // 列宽：给了就用，没给按内容估算
        for c in 0..cols {
            let col = officepilot_engine::formula::index_to_col(c as u32 + 1);
            let given = sheet["widths"].get(&col).and_then(Value::as_f64).or_else(|| sheet["widths"].get(c).and_then(Value::as_f64));
            let width = given.unwrap_or_else(|| {
                let max = rows.iter().filter_map(|r| r.get(c)).map(|v| match v {
                    Value::String(s) if s.starts_with('=') => 10,
                    Value::String(s) => display_width(s),
                    other => other.to_string().len(),
                }).max().unwrap_or(8);
                (max as f64 + 2.0).clamp(8.0, 60.0)
            });
            ops.push(json!({ "op": "column_width", "sheet": name, "columns": col, "width": width }));
        }
        for f in sheet["formats"].as_array().into_iter().flatten() {
            let mut f = f.clone();
            f["op"] = json!("format");
            f["sheet"] = json!(name);
            ops.push(f);
        }
        for ch in sheet["charts"].as_array().into_iter().flatten() {
            let mut ch = ch.clone();
            ch["op"] = json!("add_chart");
            if ch.get("sheet").is_none() {
                ch["sheet"] = json!(name);
            }
            charts.push(ch);
        }
    }
    ops.extend(charts);
    if ops.is_empty() {
        return Ok((wb.save()?, names.len()));
    }
    let typed: Vec<xlsx::SheetOp> = parse_ops(Value::Array(ops), "sheet")?;
    wb.apply(&typed)?;
    Ok((wb.save()?, names.len()))
}

fn create_xlsx(from: &str, out: &str) -> Out {
    let spec = if from.to_lowercase().ends_with(".csv") || from.to_lowercase().ends_with(".tsv") {
        let rows = csv_rows(&text_input(from)?);
        Value::Array(rows.into_iter().map(|r| Value::Array(r.into_iter().map(numberish).collect())).collect())
    } else {
        json_input(from)?
    };
    let (bytes, sheets) = build_xlsx(spec)?;
    write_atomic(out, &bytes)?;
    Ok(Done::ok(format!("已创建 {out}（{sheets} 个工作表）"), json!({ "path": out, "sheets": sheets })))
}

/// CSV 里的数字按数字写入
fn numberish(v: Value) -> Value {
    if let Value::String(s) = &v {
        let t = s.trim();
        if !t.is_empty() && !(t.len() > 1 && t.starts_with('0') && !t.starts_with("0.")) {
            if let Ok(n) = t.replace(',', "").parse::<f64>() {
                if !t.contains(',') || t.chars().filter(|c| *c == ',').count() * 4 <= t.len() {
                    return json!(n);
                }
            }
        }
    }
    v
}
