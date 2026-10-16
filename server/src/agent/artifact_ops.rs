//! 产物局部编辑：把产物展开成带编号的视图交给模型，模型返回语义化操作，这里校验并应用。
//!
//! 约定：操作里的编号（章节 / 页 / 行 / 列 / 表）都从 1 开始，并且**一律指模型看到的原始编号**，
//! 由这里负责先后顺序（先改、再删、最后插入），模型不用关心编号偏移。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::agent::tools::doc_generate::{sections_to_markdown, DocOutput};
use crate::agent::tools::ppt_generate::{plan_to_slide, SlidePlan};
use crate::models::{Artifact, Slide};

/// 发给模型的视图最大长度（字符）
const MAX_VIEW_CHARS: usize = 24_000;

pub fn is_editable(kind: &str) -> bool {
    matches!(kind, "markdown" | "document" | "sheet" | "chart" | "ppt" | "drawio")
}

/// 编辑后的结果
#[derive(Debug, Clone, PartialEq)]
pub struct Edited {
    pub title: String,
    pub content: Value,
}

// ───────────────────────── 通用：按原始编号修改列表 ─────────────────────────

/// 针对原始列表的一组修改：替换、删除、在某项之后插入（0 表示最前面）
struct SlotEdits<T> {
    replace: BTreeMap<usize, T>,
    delete: BTreeSet<usize>,
    insert_after: BTreeMap<usize, Vec<T>>,
}

impl<T> SlotEdits<T> {
    fn new() -> Self {
        Self {
            replace: BTreeMap::new(),
            delete: BTreeSet::new(),
            insert_after: BTreeMap::new(),
        }
    }

    fn check(len: usize, n: usize, what: &str) -> Result<usize, String> {
        if n == 0 || n > len {
            Err(format!("{what} {n} 不存在（共 {len} 个）"))
        } else {
            Ok(n - 1)
        }
    }

    fn replace(&mut self, len: usize, n: usize, item: T, what: &str) -> Result<(), String> {
        let i = Self::check(len, n, what)?;
        self.replace.insert(i, item);
        Ok(())
    }

    fn delete(&mut self, len: usize, n: usize, what: &str) -> Result<(), String> {
        let i = Self::check(len, n, what)?;
        self.delete.insert(i);
        Ok(())
    }

    fn insert(&mut self, len: usize, after: usize, item: T, what: &str) -> Result<(), String> {
        if after > len {
            return Err(format!("{what} {after} 不存在（共 {len} 个），无法在其后插入"));
        }
        self.insert_after.entry(after).or_default().push(item);
        Ok(())
    }

    fn apply(mut self, items: Vec<T>) -> Vec<T> {
        let mut out = Vec::with_capacity(items.len());
        out.extend(self.insert_after.remove(&0).unwrap_or_default());
        for (i, item) in items.into_iter().enumerate() {
            if !self.delete.contains(&i) {
                out.push(self.replace.remove(&i).unwrap_or(item));
            }
            out.extend(self.insert_after.remove(&(i + 1)).unwrap_or_default());
        }
        out
    }
}

fn op_name(op: &Value) -> &str {
    op.get("op").and_then(Value::as_str).unwrap_or("")
}

fn usize_field(op: &Value, key: &str) -> Result<usize, String> {
    op.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .ok_or_else(|| format!("操作 {} 缺少数字字段 {key}", op_name(op)))
}

fn str_field<'a>(op: &'a Value, key: &str) -> Result<&'a str, String> {
    op.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("操作 {} 缺少文本字段 {key}", op_name(op)))
}

fn truncate(text: String) -> String {
    if text.chars().count() <= MAX_VIEW_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_VIEW_CHARS).collect();
    cut.push_str("\n…（内容过长，后续部分省略；需要修改省略部分时请使用整体替换类操作）");
    cut
}

// ───────────────────────── Markdown ─────────────────────────

/// 按标题切分章节（代码块里的 # 不算标题）。标题前的内容作为第一个"前言"章节
pub fn split_markdown(markdown: &str) -> Vec<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        let is_heading = !in_fence
            && trimmed.starts_with('#')
            && trimmed.trim_start_matches('#').starts_with(' ')
            && trimmed.chars().take_while(|c| *c == '#').count() <= 6;
        if is_heading && !current.trim().is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        sections.push(current);
    }
    sections
}

fn join_sections(sections: &[String]) -> String {
    sections
        .iter()
        .map(|s| s.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n\n")
        + "\n"
}

fn section_heading(section: &str) -> String {
    let first = section.lines().next().unwrap_or("").trim();
    if first.starts_with('#') {
        first.trim_start_matches('#').trim().to_string()
    } else {
        "（前言）".to_string()
    }
}

fn markdown_view(markdown: &str) -> String {
    split_markdown(markdown)
        .iter()
        .enumerate()
        .map(|(i, s)| format!("=== 章节 {} ===\n{}", i + 1, s.trim_end()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn apply_markdown(markdown: &str, ops: &[Value]) -> Result<Option<String>, String> {
    let sections = split_markdown(markdown);
    let len = sections.len();
    let mut edits = SlotEdits::new();
    let mut whole: Option<String> = None;
    for op in ops {
        match op_name(op) {
            "replace_section" => edits.replace(len, usize_field(op, "section")?, str_field(op, "markdown")?.to_string(), "章节")?,
            "delete_section" => edits.delete(len, usize_field(op, "section")?, "章节")?,
            "insert_after" => edits.insert(len, usize_field(op, "section")?, str_field(op, "markdown")?.to_string(), "章节")?,
            "replace_all" => whole = Some(str_field(op, "markdown")?.to_string()),
            "set_title" => {}
            other => return Err(format!("Markdown 不支持操作 {other}")),
        }
    }
    if let Some(markdown) = whole {
        return Ok(Some(markdown));
    }
    Ok(Some(join_sections(&edits.apply(sections))))
}

// ───────────────────────── Word 文档（结构化章节） ─────────────────────────

fn doc_sections(content: &Value) -> Option<&Vec<Value>> {
    content.get("sections").and_then(Value::as_array).filter(|s| !s.is_empty())
}

fn document_view(content: &Value) -> String {
    match doc_sections(content) {
        Some(sections) => sections
            .iter()
            .enumerate()
            .map(|(i, s)| format!("=== 章节 {} ===\n{}", i + 1, serde_json::to_string_pretty(s).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        None => markdown_view(content.get("markdown").and_then(Value::as_str).unwrap_or("")),
    }
}

fn apply_document(title: &str, content: &Value, ops: &[Value]) -> Result<Value, String> {
    let Some(sections) = doc_sections(content) else {
        // 没有结构化章节的旧文档按 Markdown 处理
        let markdown = content.get("markdown").and_then(Value::as_str).unwrap_or("");
        let mut next = content.clone();
        next["markdown"] = json!(apply_markdown(markdown, ops)?.unwrap_or_default());
        return Ok(next);
    };
    let len = sections.len();
    let mut edits = SlotEdits::new();
    let mut whole: Option<Vec<Value>> = None;
    let section_value = |op: &Value| -> Result<Value, String> {
        let value = op.get("value").cloned().ok_or("操作缺少 value（章节对象）")?;
        if value.get("heading").and_then(Value::as_str).is_none() {
            return Err("章节对象缺少 heading".to_string());
        }
        Ok(value)
    };
    for op in ops {
        match op_name(op) {
            "replace_section" => edits.replace(len, usize_field(op, "section")?, section_value(op)?, "章节")?,
            "delete_section" => edits.delete(len, usize_field(op, "section")?, "章节")?,
            "insert_after" => edits.insert(len, usize_field(op, "section")?, section_value(op)?, "章节")?,
            "replace_all" => {
                whole = Some(
                    op.get("sections")
                        .and_then(Value::as_array)
                        .cloned()
                        .ok_or("replace_all 需要 sections 数组")?,
                )
            }
            "set_title" => {}
            other => return Err(format!("Word 文档不支持操作 {other}")),
        }
    }
    let new_sections = whole.unwrap_or_else(|| edits.apply(sections.clone()));
    let doc: DocOutput = serde_json::from_value(json!({
        "title": title,
        "sections": new_sections,
        "summary": content.get("summary"),
    }))
    .map_err(|e| format!("章节格式不正确: {e}"))?;
    let mut next = content.clone();
    next["title"] = json!(title);
    next["sections"] = json!(new_sections);
    next["markdown"] = json!(sections_to_markdown(&doc));
    Ok(next)
}

// ───────────────────────── 表格 ─────────────────────────

fn sheet_tables(content: &Value) -> Vec<Value> {
    content.get("tables").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn str_vec(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn sheet_view(content: &Value) -> String {
    sheet_tables(content)
        .iter()
        .enumerate()
        .map(|(t, table)| {
            let headers = str_vec(table.get("headers"));
            let mut out = format!(
                "=== 表 {}《{}》 ===\n列：{}",
                t + 1,
                table.get("title").and_then(Value::as_str).unwrap_or(""),
                headers
                    .iter()
                    .enumerate()
                    .map(|(c, h)| format!("{}={h}", c + 1))
                    .collect::<Vec<_>>()
                    .join("，")
            );
            for (r, row) in table.get("rows").and_then(Value::as_array).into_iter().flatten().enumerate() {
                out.push_str(&format!("\n行 {}：{}", r + 1, str_vec(Some(row)).join(" | ")));
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 列可以用编号（从 1 开始）或表头名定位
fn column_index(headers: &[String], op: &Value, key: &str) -> Result<usize, String> {
    match op.get(key) {
        Some(Value::Number(n)) => {
            let n = n.as_u64().unwrap_or(0) as usize;
            if n == 0 || n > headers.len() {
                Err(format!("列 {n} 不存在（共 {} 列）", headers.len()))
            } else {
                Ok(n - 1)
            }
        }
        Some(Value::String(name)) => headers
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| format!("找不到列「{name}」")),
        _ => Err(format!("操作 {} 缺少 {key}（列号或列名）", op_name(op))),
    }
}

fn apply_sheet(content: &Value, ops: &[Value]) -> Result<Value, String> {
    let mut tables = sheet_tables(content);
    let original_len = tables.len();
    let mut per_table: BTreeMap<usize, Vec<&Value>> = BTreeMap::new();
    let mut new_tables = Vec::new();
    for op in ops {
        match op_name(op) {
            "add_table" => new_tables.push(op.get("value").cloned().ok_or("add_table 缺少 value")?),
            "set_title" => {}
            _ => {
                let t = usize_field(op, "table")?;
                if t == 0 || t > original_len {
                    return Err(format!("表 {t} 不存在（共 {original_len} 个）"));
                }
                per_table.entry(t - 1).or_default().push(op);
            }
        }
    }

    for (t, ops) in per_table {
        let table = &mut tables[t];
        if let Some(replace) = ops.iter().find(|op| op_name(op) == "replace_table") {
            *table = replace.get("value").cloned().ok_or("replace_table 缺少 value")?;
            continue;
        }
        let mut headers = str_vec(table.get("headers"));
        let mut rows: Vec<Vec<String>> = table
            .get("rows")
            .and_then(Value::as_array)
            .map(|rows| rows.iter().map(|r| str_vec(Some(r))).collect())
            .unwrap_or_default();
        let row_count = rows.len();
        let row_index = |n: usize| -> Result<usize, String> {
            if n == 0 || n > row_count {
                Err(format!("行 {n} 不存在（共 {row_count} 行）"))
            } else {
                Ok(n - 1)
            }
        };

        // 1. 修改单元格、重命名列（原始编号此时仍然有效）
        for op in &ops {
            match op_name(op) {
                "set_cells" => {
                    for cell in op.get("cells").and_then(Value::as_array).ok_or("set_cells 需要 cells 数组")? {
                        let r = row_index(usize_field(cell, "row")?)?;
                        let c = column_index(&headers, cell, "column")?;
                        let value = cell.get("value").map(|v| str_vec(Some(&json!([v])))[0].clone()).unwrap_or_default();
                        let row = &mut rows[r];
                        if row.len() <= c {
                            row.resize(c + 1, String::new());
                        }
                        row[c] = value;
                    }
                }
                "rename_column" => {
                    let c = column_index(&headers, op, "column")?;
                    headers[c] = str_field(op, "header")?.to_string();
                }
                "delete_rows" | "delete_column" | "add_column" | "append_rows" => {}
                other => return Err(format!("表格不支持操作 {other}")),
            }
        }
        // 2. 删除行、列（从后往前）
        let mut drop_rows = BTreeSet::new();
        let mut drop_cols = BTreeSet::new();
        for op in &ops {
            match op_name(op) {
                "delete_rows" => {
                    for n in op.get("rows").and_then(Value::as_array).ok_or("delete_rows 需要 rows 数组")? {
                        drop_rows.insert(row_index(n.as_u64().unwrap_or(0) as usize)?);
                    }
                }
                "delete_column" => {
                    drop_cols.insert(column_index(&headers, op, "column")?);
                }
                _ => {}
            }
        }
        rows = rows
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !drop_rows.contains(i))
            .map(|(_, row)| row)
            .collect();
        for c in drop_cols.iter().rev() {
            headers.remove(*c);
            for row in rows.iter_mut() {
                if row.len() > *c {
                    row.remove(*c);
                }
            }
        }
        // 3. 追加列、行
        for op in &ops {
            match op_name(op) {
                "add_column" => {
                    headers.push(str_field(op, "header")?.to_string());
                    let values = str_vec(op.get("values"));
                    let width = headers.len();
                    for (i, row) in rows.iter_mut().enumerate() {
                        row.resize(width - 1, String::new());
                        row.push(values.get(i).cloned().unwrap_or_default());
                    }
                }
                "append_rows" => {
                    for row in op.get("rows").and_then(Value::as_array).ok_or("append_rows 需要 rows 数组")? {
                        rows.push(str_vec(Some(row)));
                    }
                }
                _ => {}
            }
        }
        table["headers"] = json!(headers);
        table["rows"] = json!(rows);
    }

    tables.extend(new_tables);
    let mut next = content.clone();
    next["tables"] = json!(tables);
    Ok(next)
}

// ───────────────────────── 图表 ─────────────────────────

/// RFC 7396 JSON Merge Patch
pub fn merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(map) => {
            if !target.is_object() {
                *target = json!({});
            }
            let obj = target.as_object_mut().expect("object");
            for (key, value) in map {
                if value.is_null() {
                    obj.remove(key);
                } else {
                    merge_patch(obj.entry(key.clone()).or_insert(Value::Null), value);
                }
            }
        }
        other => *target = other.clone(),
    }
}

fn apply_chart(content: &Value, ops: &[Value]) -> Result<Value, String> {
    let mut next = content.clone();
    for op in ops {
        match op_name(op) {
            "merge" => merge_patch(&mut next, op.get("patch").ok_or("merge 缺少 patch")?),
            "set_title" => {}
            other => return Err(format!("图表不支持操作 {other}")),
        }
    }
    // chart_type 与 type 保持一致
    if let Some(chart_type) = next.get("chart_type").cloned() {
        next["type"] = chart_type;
    }
    let labels = next.pointer("/chart_data/labels").and_then(Value::as_array).map(Vec::len);
    let values = next.pointer("/chart_data/values").and_then(Value::as_array).map(Vec::len);
    if labels != values {
        return Err(format!("图表 labels 与 values 数量不一致（{labels:?} vs {values:?}）"));
    }
    Ok(next)
}

// ───────────────────────── PPT ─────────────────────────

fn slide_text(slide: &Slide) -> Vec<String> {
    slide
        .elements
        .iter()
        .filter_map(|e| e.text.clone())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn ppt_slides(content: &Value) -> Result<Vec<Slide>, String> {
    serde_json::from_value(content.get("slides").cloned().unwrap_or(json!([])))
        .map_err(|e| format!("幻灯片数据无法解析: {e}"))
}

fn ppt_view(content: &Value) -> String {
    ppt_slides(content)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, slide)| {
            format!(
                "=== 第 {} 页 ===\n标题：{}\n文字：{}",
                i + 1,
                slide.title.clone().unwrap_or_default(),
                slide_text(slide).join(" / ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn slide_plan(op: &Value) -> Result<SlidePlan, String> {
    let plan = op.get("plan").cloned().ok_or("操作缺少 plan（页面规划）")?;
    serde_json::from_value(plan).map_err(|e| format!("页面规划格式不正确: {e}"))
}

fn apply_ppt(title: &str, content: &Value, ops: &[Value]) -> Result<Value, String> {
    let slides = ppt_slides(content)?;
    let len = slides.len();
    let theme = content.get("theme").and_then(Value::as_str).unwrap_or("default").to_string();
    let mut edits = SlotEdits::new();
    for op in ops {
        match op_name(op) {
            "update_slide" => {
                let n = usize_field(op, "slide")?;
                let mut slide = plan_to_slide(title, &slide_plan(op)?, n.saturating_sub(1), len, &theme);
                if let Some(old) = n.checked_sub(1).and_then(|i| slides.get(i)) {
                    slide.id = old.id.clone();
                }
                edits.replace(len, n, slide, "页")?;
            }
            "delete_slide" => edits.delete(len, usize_field(op, "slide")?, "页")?,
            "insert_after" => {
                let after = usize_field(op, "slide")?;
                let slide = plan_to_slide(title, &slide_plan(op)?, after.max(1), len + 1, &theme);
                edits.insert(len, after, slide, "页")?;
            }
            "set_title" => {}
            other => return Err(format!("PPT 不支持操作 {other}")),
        }
    }
    let new_slides = edits.apply(slides);
    if new_slides.is_empty() {
        return Err("PPT 至少需要保留一页".to_string());
    }
    let mut next = content.clone();
    next["title"] = json!(title);
    next["slide_count"] = json!(new_slides.len());
    next["total_slides"] = json!(new_slides.len());
    next["slides"] = json!(new_slides);
    Ok(next)
}

// ───────────────────────── draw.io ─────────────────────────

fn apply_drawio(content: &Value, ops: &[Value]) -> Result<Value, String> {
    let mut next = content.clone();
    for op in ops {
        match op_name(op) {
            "replace_xml" => {
                let xml = str_field(op, "xml")?;
                if !(xml.contains("<mxGraphModel") || xml.contains("<mxfile")) {
                    return Err("xml 不是有效的 draw.io 内容（缺少 mxGraphModel / mxfile）".to_string());
                }
                next["xml"] = json!(xml);
            }
            "set_title" => {}
            other => return Err(format!("draw.io 不支持操作 {other}")),
        }
    }
    Ok(next)
}

// ───────────────────────── 对外接口 ─────────────────────────

/// 带编号的完整视图，供模型定位要改的位置
pub fn edit_view(artifact: &Artifact) -> String {
    let c = &artifact.content;
    let body = match artifact.kind.as_str() {
        "markdown" => markdown_view(c.get("markdown").and_then(Value::as_str).unwrap_or("")),
        "document" => document_view(c),
        "sheet" => sheet_view(c),
        "ppt" => ppt_view(c),
        "chart" => serde_json::to_string_pretty(c).unwrap_or_default(),
        "drawio" => c.get("xml").and_then(Value::as_str).unwrap_or("").to_string(),
        _ => String::new(),
    };
    truncate(format!("标题：{}\n\n{body}", artifact.title))
}

/// 各类型可用的操作说明（拼进编辑提示词）
pub fn op_guide(kind: &str) -> &'static str {
    match kind {
        "markdown" => r###"可用操作（section 为章节编号）：
{"op":"replace_section","section":2,"markdown":"## 标题\n新内容（包含标题行）"}
{"op":"insert_after","section":2,"markdown":"## 新章节\n内容"}   // section 为 0 表示插到最前面
{"op":"delete_section","section":3}
{"op":"replace_all","markdown":"整篇新内容"}   // 只在需要整体改写（如翻译全文、整体改风格）时使用
{"op":"set_title","title":"新标题"}"###,
        "document" => r###"可用操作（section 为章节编号；value 为章节对象 {"heading","heading_level","paragraphs":[],"bullets":[],"table":{"headers":[],"rows":[[]]}}，只写需要的字段）：
{"op":"replace_section","section":2,"value":{...}}
{"op":"insert_after","section":2,"value":{...}}   // section 为 0 表示插到最前面
{"op":"delete_section","section":3}
{"op":"replace_all","sections":[{...}]}   // 只在需要整体改写时使用
{"op":"set_title","title":"新标题"}"###,
        "sheet" => r###"可用操作（table 为表编号；row 为数据行编号；column 可以是列号或列名）：
{"op":"set_cells","table":1,"cells":[{"row":2,"column":"预算","value":"5000"}]}
{"op":"append_rows","table":1,"rows":[["a","b","c"]]}
{"op":"delete_rows","table":1,"rows":[3,5]}
{"op":"add_column","table":1,"header":"备注","values":["第1行的值","第2行的值"]}
{"op":"delete_column","table":1,"column":"备注"}
{"op":"rename_column","table":1,"column":2,"header":"新列名"}
{"op":"replace_table","table":1,"value":{"title":"","headers":[],"rows":[[]]}}   // 只在需要整体重做某张表时使用
{"op":"add_table","value":{"title":"","headers":[],"rows":[[]]}}
{"op":"set_title","title":"新标题"}"###,
        "ppt" => r###"可用操作（slide 为页码；plan 为页面规划 {"title","layout":"title|section|content","points":["要点（最多 4 条）"],"goal":"本页目的","visual":"视觉建议"}）：
{"op":"update_slide","slide":3,"plan":{...}}   // 重新生成这一页
{"op":"insert_after","slide":3,"plan":{...}}   // slide 为 0 表示插到最前面
{"op":"delete_slide","slide":5}
{"op":"set_title","title":"新标题"}"###,
        "chart" => r###"可用操作（patch 是 JSON Merge Patch，作用于你看到的图表 JSON）：
{"op":"merge","patch":{"chart_type":"line","chart_data":{"labels":[],"values":[]}}}   // labels 与 values 数量必须一致；数组会被整体替换
{"op":"set_title","title":"新标题"}"###,
        "drawio" => r###"可用操作：
{"op":"replace_xml","xml":"<mxfile>…完整的新 XML…</mxfile>"}   // 在原 XML 基础上修改，保留未要求改动的节点和布局
{"op":"set_title","title":"新标题"}"###,
        _ => "",
    }
}

/// 应用一组操作；任何一个操作不合法都整体失败（不产生半成品）
pub fn apply_ops(artifact: &Artifact, ops: &[Value]) -> Result<Edited, String> {
    if ops.is_empty() {
        return Err("没有任何修改操作".to_string());
    }
    let title = ops
        .iter()
        .rev()
        .find(|op| op_name(op) == "set_title")
        .and_then(|op| op.get("title").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| artifact.title.clone());
    let c = &artifact.content;
    let content = match artifact.kind.as_str() {
        "markdown" => {
            let markdown = c.get("markdown").and_then(Value::as_str).unwrap_or("");
            let only_title = ops.iter().all(|op| op_name(op) == "set_title");
            let mut next = c.clone();
            if !only_title {
                next["markdown"] = json!(apply_markdown(markdown, ops)?.unwrap_or_default());
            }
            next["title"] = json!(title);
            next
        }
        "document" => apply_document(&title, c, ops)?,
        "sheet" => {
            let mut next = apply_sheet(c, ops)?;
            next["title"] = json!(title);
            next
        }
        "chart" => {
            let mut next = apply_chart(c, ops)?;
            next["title"] = json!(title);
            next
        }
        "ppt" => apply_ppt(&title, c, ops)?,
        "drawio" => {
            let mut next = apply_drawio(c, ops)?;
            next["title"] = json!(title);
            next
        }
        other => return Err(format!("{other} 类型的产物不支持局部编辑")),
    };
    Ok(Edited { title, content })
}

/// 一行大纲，用于会话产物清单
pub fn outline(artifact: &Artifact) -> String {
    let c = &artifact.content;
    let items: Vec<String> = match artifact.kind.as_str() {
        "markdown" => split_markdown(c.get("markdown").and_then(Value::as_str).unwrap_or(""))
            .iter()
            .map(|s| section_heading(s))
            .collect(),
        "document" => match doc_sections(c) {
            Some(sections) => sections
                .iter()
                .map(|s| s.get("heading").and_then(Value::as_str).unwrap_or("").to_string())
                .collect(),
            None => split_markdown(c.get("markdown").and_then(Value::as_str).unwrap_or(""))
                .iter()
                .map(|s| section_heading(s))
                .collect(),
        },
        "sheet" => sheet_tables(c)
            .iter()
            .map(|t| {
                format!(
                    "{}（列：{}；{} 行）",
                    t.get("title").and_then(Value::as_str).unwrap_or(""),
                    str_vec(t.get("headers")).join("/"),
                    t.get("rows").and_then(Value::as_array).map_or(0, Vec::len)
                )
            })
            .collect(),
        "ppt" => ppt_slides(c)
            .unwrap_or_default()
            .iter()
            .map(|s| s.title.clone().unwrap_or_default())
            .collect(),
        "chart" => vec![c.get("chart_type").and_then(Value::as_str).unwrap_or("").to_string()],
        _ => vec![],
    };
    let label = match artifact.kind.as_str() {
        "ppt" => "页",
        "sheet" => "表",
        "chart" => "类型",
        _ => "章节",
    };
    if items.is_empty() {
        return String::new();
    }
    let joined = items
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}.{s}", i + 1))
        .collect::<Vec<_>>()
        .join(" ");
    let short: String = joined.chars().take(240).collect();
    let ellipsis = if joined.chars().count() > 240 { "…" } else { "" };
    format!("{label}：{short}{ellipsis}")
}

/// 会话产物清单（拼进系统提示词），让模型能把"第三页""那份周报"对应到具体产物
pub fn manifest(artifacts: &[Artifact]) -> String {
    if artifacts.is_empty() {
        return String::new();
    }
    let mut out = String::from("## 当前会话已有的产物\n用户要求修改已有产物（改某一页、某一章、某些单元格、换图表类型等）时，调用 artifact_edit 局部修改，不要重新生成整份产物。\n");
    for artifact in artifacts.iter().take(12) {
        let editable = if is_editable(&artifact.kind) { "" } else { "（不支持局部编辑）" };
        out.push_str(&format!(
            "- id={} · {} · 《{}》 · v{}{editable}",
            artifact.id, artifact.kind, artifact.title, artifact.version
        ));
        let outline = outline(artifact);
        if !outline.is_empty() {
            out.push_str(&format!(" · {outline}"));
        }
        out.push('\n');
    }
    if artifacts.len() > 12 {
        out.push_str(&format!("- …另有 {} 个更早的产物\n", artifacts.len() - 12));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(kind: &str, content: Value) -> Artifact {
        Artifact {
            id: "a1".into(),
            kind: kind.into(),
            tool_kind: "doc".into(),
            title: "原标题".into(),
            status: "ready".into(),
            content,
            version: 1,
            created_at: String::new(),
            updated_at: String::new(),
            change_summary: None,
        }
    }

    const MD: &str = "前言一句\n\n# 概览\n内容A\n\n## 细节\n```\n# 不是标题\n```\n内容B\n\n# 结论\n内容C\n";

    #[test]
    fn markdown_split_ignores_code_fences() {
        let sections = split_markdown(MD);
        assert_eq!(sections.len(), 4);
        assert!(sections[2].contains("# 不是标题"));
        assert_eq!(section_heading(&sections[0]), "（前言）");
        assert_eq!(section_heading(&sections[3]), "结论");
    }

    #[test]
    fn markdown_ops_use_original_numbering() {
        let a = artifact("markdown", json!({ "markdown": MD }));
        let ops = vec![
            json!({"op":"delete_section","section":1}),
            json!({"op":"insert_after","section":2,"markdown":"# 新增\n内容N"}),
            json!({"op":"replace_section","section":4,"markdown":"# 结论\n改写后的结论"}),
            json!({"op":"insert_after","section":0,"markdown":"# 开头\n最前"}),
            json!({"op":"set_title","title":"新标题"}),
        ];
        let edited = apply_ops(&a, &ops).unwrap();
        let md = edited.content["markdown"].as_str().unwrap();
        let headings: Vec<String> = split_markdown(md).iter().map(|s| section_heading(s)).collect();
        assert_eq!(headings, ["开头", "概览", "新增", "细节", "结论"]);
        assert!(md.contains("改写后的结论"));
        assert!(!md.contains("前言一句"));
        assert_eq!(edited.title, "新标题");
    }

    #[test]
    fn invalid_ops_fail_without_partial_changes() {
        let a = artifact("markdown", json!({ "markdown": MD }));
        assert!(apply_ops(&a, &[json!({"op":"delete_section","section":9})]).is_err());
        assert!(apply_ops(&a, &[json!({"op":"explode"})]).is_err());
        assert!(apply_ops(&a, &[]).is_err());
        assert!(apply_ops(&artifact("image", json!({})), &[json!({"op":"set_title","title":"x"})]).is_err());
    }

    #[test]
    fn document_sections_regenerate_markdown() {
        let a = artifact(
            "document",
            json!({
                "type": "structured",
                "sections": [
                    {"heading": "背景", "heading_level": 1, "paragraphs": ["旧背景"]},
                    {"heading": "方案", "heading_level": 1, "bullets": ["要点"]}
                ],
                "markdown": "# 原标题\n"
            }),
        );
        let edited = apply_ops(
            &a,
            &[
                json!({"op":"replace_section","section":1,"value":{"heading":"背景","paragraphs":["新背景"]}}),
                json!({"op":"insert_after","section":2,"value":{"heading":"风险","bullets":["预算超支"]}}),
            ],
        )
        .unwrap();
        let md = edited.content["markdown"].as_str().unwrap();
        assert!(md.contains("新背景") && !md.contains("旧背景"));
        assert!(md.contains("# 风险") && md.contains("- 预算超支"));
        assert_eq!(edited.content["sections"].as_array().unwrap().len(), 3);
        assert!(apply_ops(&a, &[json!({"op":"replace_section","section":1,"value":{"paragraphs":["无标题"]}})]).is_err());
    }

    #[test]
    fn sheet_ops_apply_in_safe_order() {
        let a = artifact(
            "sheet",
            json!({"tables": [{
                "title": "预算",
                "headers": ["项目", "金额", "备注"],
                "rows": [["场地", "1000", ""], ["餐饮", "2000", ""], ["交通", "500", ""]]
            }]}),
        );
        let edited = apply_ops(
            &a,
            &[
                json!({"op":"delete_rows","table":1,"rows":[1]}),
                json!({"op":"set_cells","table":1,"cells":[{"row":3,"column":"金额","value":800}]}),
                json!({"op":"delete_column","table":1,"column":3}),
                json!({"op":"add_column","table":1,"header":"负责人","values":["张三","李四"]}),
                json!({"op":"append_rows","table":1,"rows":[["礼品","300","王五"]]}),
                json!({"op":"rename_column","table":1,"column":1,"header":"支出项"}),
            ],
        )
        .unwrap();
        let table = &edited.content["tables"][0];
        assert_eq!(table["headers"], json!(["支出项", "金额", "负责人"]));
        assert_eq!(
            table["rows"],
            json!([["餐饮", "2000", "张三"], ["交通", "800", "李四"], ["礼品", "300", "王五"]])
        );
        assert!(apply_ops(&a, &[json!({"op":"set_cells","table":2,"cells":[]})]).is_err());
        assert!(apply_ops(&a, &[json!({"op":"set_cells","table":1,"cells":[{"row":1,"column":"不存在","value":"x"}]})]).is_err());
    }

    #[test]
    fn chart_merge_patch_keeps_type_in_sync_and_validates() {
        let a = artifact(
            "chart",
            json!({"type":"bar","chart_type":"bar","title":"t","chart_data":{"labels":["a","b"],"values":[1,2],"seriesName":"x"}}),
        );
        let edited = apply_ops(&a, &[json!({"op":"merge","patch":{"chart_type":"line","summary":null}})]).unwrap();
        assert_eq!(edited.content["type"], "line");
        assert_eq!(edited.content["chart_data"]["seriesName"], "x");
        assert!(apply_ops(&a, &[json!({"op":"merge","patch":{"chart_data":{"labels":["a"]}}})]).is_err());
    }

    #[test]
    fn ppt_ops_regenerate_only_touched_slides() {
        let plan = |t: &str| -> SlidePlan { serde_json::from_value(json!({"title": t, "points": ["要点"]})).unwrap() };
        let slides: Vec<Slide> = ["封面", "现状", "计划"]
            .iter()
            .enumerate()
            .map(|(i, t)| plan_to_slide("汇报", &plan(t), i, 3, "default"))
            .collect();
        let untouched_id = slides[0].id.clone();
        let updated_id = slides[1].id.clone();
        let a = artifact("ppt", json!({"project_id":"p1","theme":"default","slides":slides,"slide_count":3}));
        let edited = apply_ops(
            &a,
            &[
                json!({"op":"update_slide","slide":2,"plan":{"title":"现状（改）","points":["新要点"]}}),
                json!({"op":"delete_slide","slide":3}),
                json!({"op":"insert_after","slide":3,"plan":{"title":"下一步"}}),
            ],
        )
        .unwrap();
        let out: Vec<Slide> = serde_json::from_value(edited.content["slides"].clone()).unwrap();
        let titles: Vec<_> = out.iter().map(|s| s.title.clone().unwrap()).collect();
        assert_eq!(titles, ["封面", "现状（改）", "下一步"]);
        assert_eq!(out[0].id, untouched_id);
        assert_eq!(out[1].id, updated_id, "改写的页面保留原 id");
        assert_eq!(edited.content["slide_count"], 3);
        assert!(apply_ops(&a, &[json!({"op":"update_slide","slide":2,"plan":{"points":[]}})]).is_err());
    }

    #[test]
    fn drawio_requires_valid_xml() {
        let a = artifact("drawio", json!({"xml":"<mxfile/>"}));
        assert!(apply_ops(&a, &[json!({"op":"replace_xml","xml":"<svg/>"})]).is_err());
        let ok = apply_ops(&a, &[json!({"op":"replace_xml","xml":"<mxGraphModel><root/></mxGraphModel>"})]).unwrap();
        assert!(ok.content["xml"].as_str().unwrap().contains("mxGraphModel"));
    }

    #[test]
    fn manifest_lists_ids_and_outlines() {
        let md = artifact("markdown", json!({ "markdown": MD }));
        let image = Artifact { id: "img".into(), kind: "image".into(), ..md.clone() };
        let text = manifest(&[md, image]);
        assert!(text.contains("id=a1 · markdown · 《原标题》 · v1 · 章节：1.（前言） 2.概览"));
        assert!(text.contains("id=img · image"));
        assert!(text.contains("不支持局部编辑"));
        assert_eq!(manifest(&[]), "");
    }
}
