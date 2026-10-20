//! Word（.docx）：把正文解析成块（段落、标题、列表项、表格），按块编辑，只改写涉及的块。
//!
//! 块的序号以读取时为准：同一批操作里的序号都指读取时的文档，插入、删除不会让后面的序号错位。
//! 打开修订模式时，改动写成 Word 修订（w:ins / w:del / w:pPrChange），用户可以在 Word 里逐条接受或拒绝。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::markdown::{self, Span};
use crate::package::{insert_before_close, Package};
use crate::text_edit::{self, WORD};
use crate::xml::{self, Splice, Xml};
use crate::{clip, op_err, revision_of, ApplyReport, Change, EngineError, Result};

const REL_STYLES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
const REL_NUMBERING: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const REL_COMMENTS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const REL_HYPERLINK: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
const CT_STYLES: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";
const CT_NUMBERING: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
const CT_COMMENTS: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml";
const NS_W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

// ───────────────────────── 样式表 ─────────────────────────

#[derive(Debug, Clone)]
struct StyleInfo {
    name: String,
    kind: String,
    outline: Option<u8>,
    based_on: Option<String>,
    default: bool,
    /// 样式自带编号（如 List Bullet）
    list: bool,
}

#[derive(Debug, Clone, Default)]
struct Styles {
    by_id: HashMap<String, StyleInfo>,
}

impl Styles {
    fn parse(text: &str) -> Result<Self> {
        let xml = Xml::parse(text)?;
        let mut by_id = HashMap::new();
        let Some(root) = xml.document_element() else { return Ok(Self::default()) };
        for s in xml.children_named(root, "w:style") {
            let Some(id) = xml.attr(s, "w:styleId") else { continue };
            let name = xml.child(s, "w:name").and_then(|n| xml.attr(n, "w:val")).unwrap_or_else(|| id.clone());
            let outline = xml
                .child(s, "w:pPr")
                .and_then(|p| xml.child(p, "w:outlineLvl"))
                .and_then(|o| xml.attr(o, "w:val"))
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|&v| v < 9)
                .map(|v| v + 1);
            by_id.insert(
                id,
                StyleInfo {
                    name,
                    kind: xml.attr(s, "w:type").unwrap_or_default(),
                    outline,
                    based_on: xml.child(s, "w:basedOn").and_then(|b| xml.attr(b, "w:val")),
                    default: matches!(xml.attr(s, "w:default").as_deref(), Some("1") | Some("true")),
                    list: xml
                        .child(s, "w:pPr")
                        .and_then(|p| xml.child(p, "w:numPr"))
                        .map(|n| xml.child(n, "w:numId").and_then(|i| xml.attr(i, "w:val")).map(|v| v != "0").unwrap_or(true))
                        .unwrap_or(false),
                },
            );
        }
        Ok(Self { by_id })
    }

    fn name(&self, id: &str) -> String {
        self.by_id.get(id).map(|s| s.name.clone()).unwrap_or_else(|| id.to_string())
    }

    /// 标题级别：样式名为 heading N / 标题 N，或样式（及其基础样式）带大纲级别
    fn heading_level(&self, id: &str) -> Option<u8> {
        let mut cur = id.to_string();
        for _ in 0..6 {
            let info = self.by_id.get(&cur);
            let name = info.map(|i| i.name.to_lowercase()).unwrap_or_else(|| cur.to_lowercase());
            if let Some(n) = name.strip_prefix("heading").map(str::trim).and_then(|n| n.parse::<u8>().ok()) {
                return Some(n);
            }
            if name == "title" {
                return Some(1);
            }
            let info = info?;
            if let Some(level) = info.outline {
                return Some(level);
            }
            cur = info.based_on.clone()?;
        }
        None
    }

    fn is_list(&self, id: &str) -> bool {
        let mut cur = id.to_string();
        for _ in 0..6 {
            let Some(info) = self.by_id.get(&cur) else { return false };
            if info.list {
                return true;
            }
            match &info.based_on {
                Some(b) => cur = b.clone(),
                None => return false,
            }
        }
        false
    }

    fn find_by_name(&self, name: &str, kind: &str) -> Option<String> {
        let wanted = name.to_lowercase().replace(' ', "");
        self.by_id
            .iter()
            .filter(|(_, s)| s.kind == kind)
            .find(|(id, s)| s.name.to_lowercase().replace(' ', "") == wanted || id.to_lowercase() == wanted)
            .map(|(id, _)| id.clone())
    }

    fn normal(&self) -> String {
        self.by_id
            .iter()
            .find(|(_, s)| s.kind == "paragraph" && s.default)
            .map(|(id, _)| id.clone())
            .or_else(|| self.find_by_name("normal", "paragraph"))
            .unwrap_or_else(|| "Normal".into())
    }
}

// ───────────────────────── 读取 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub index: usize,
    /// heading / paragraph / list / table / other
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<Vec<String>>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_image: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub has_revisions: bool,
    #[serde(skip)]
    start: usize,
    #[serde(skip)]
    end: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outline {
    pub index: usize,
    pub level: u8,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocInfo {
    pub blocks: usize,
    pub paragraphs: usize,
    pub tables: usize,
    pub images: usize,
    pub characters: usize,
    pub headings: Vec<Outline>,
    pub has_revisions: bool,
    pub comments: usize,
    pub revision: String,
}

pub struct Document {
    pkg: Package,
    main: String,
    text: String,
    styles: Styles,
    styles_part: Option<String>,
}

fn table_rows(xml: &Xml, tbl: usize) -> Vec<Vec<usize>> {
    xml.children_named(tbl, "w:tr").map(|tr| xml.children_named(tr, "w:tc").collect()).collect()
}

fn cell_text(xml: &Xml, tc: usize) -> String {
    text_edit::paragraphs(xml, tc, &WORD).into_iter().map(|p| text_edit::paragraph_text(xml, p, &WORD)).collect::<Vec<_>>().join("\n")
}

impl Document {
    pub fn open(bytes: Vec<u8>) -> Result<Self> {
        let pkg = Package::open(bytes)?;
        let main = pkg.main_part()?;
        if !main.starts_with("word/") {
            return Err(EngineError::Format("不是 Word 文档".into()));
        }
        let text = pkg.read_str(&main)?;
        let styles_part = pkg.target_of(&main, "/styles")?;
        let styles = match &styles_part {
            Some(part) if pkg.has(part) => Styles::parse(&pkg.read_str(part)?)?,
            _ => Styles::default(),
        };
        Ok(Self { pkg, main, text, styles, styles_part })
    }

    pub fn revision(&self) -> String {
        revision_of(&self.text)
    }

    pub fn blocks(&self) -> Result<Vec<Block>> {
        parse_blocks(&self.text, &self.styles)
    }

    pub fn info(&self) -> Result<DocInfo> {
        let blocks = self.blocks()?;
        let comments = match self.pkg.target_of(&self.main, "/comments")? {
            Some(part) if self.pkg.has(&part) => {
                let text = self.pkg.read_str(&part)?;
                let xml = Xml::parse(&text)?;
                xml.document_element().map(|r| xml.children_named(r, "w:comment").count()).unwrap_or(0)
            }
            _ => 0,
        };
        Ok(DocInfo {
            blocks: blocks.len(),
            paragraphs: blocks.iter().filter(|b| b.kind != "table").count(),
            tables: blocks.iter().filter(|b| b.kind == "table").count(),
            images: blocks.iter().filter(|b| b.has_image).count(),
            characters: blocks.iter().map(|b| b.text.chars().count()).sum(),
            headings: blocks.iter().filter(|b| b.kind == "heading").map(|b| Outline { index: b.index, level: b.level.unwrap_or(1), text: clip(&b.text, 80) }).collect(),
            has_revisions: blocks.iter().any(|b| b.has_revisions),
            comments,
            revision: self.revision(),
        })
    }
}

fn body_of(xml: &Xml) -> Result<usize> {
    let root = xml.document_element().ok_or_else(|| EngineError::Format("document.xml 为空".into()))?;
    xml.child(root, "w:body").ok_or_else(|| EngineError::Format("document.xml 缺少 w:body".into()))
}

fn parse_blocks(text: &str, styles: &Styles) -> Result<Vec<Block>> {
    let xml = Xml::parse(text)?;
    let body = body_of(&xml)?;
    let mut blocks = Vec::new();
    for &child in xml.children(body) {
        let name = xml.name(child);
        if !matches!(name, "w:p" | "w:tbl" | "w:sdt" | "w:customXml") {
            continue;
        }
        let n = xml.node(child);
        let descendants = xml.descendants(child);
        let has_image = descendants.iter().any(|&d| matches!(xml.name(d), "w:drawing" | "w:pict"));
        let has_revisions = descendants.iter().any(|&d| matches!(xml.name(d), "w:ins" | "w:del" | "w:pPrChange" | "w:rPrChange"));
        let mut block = Block {
            index: blocks.len(),
            kind: "paragraph".into(),
            level: None,
            style: None,
            text: String::new(),
            rows: None,
            has_image,
            has_revisions,
            start: n.start,
            end: n.end,
        };
        match name {
            "w:p" => {
                block.text = text_edit::paragraph_text(&xml, child, &WORD);
                let ppr = xml.child(child, "w:pPr");
                let style_id = ppr.and_then(|p| xml.child(p, "w:pStyle")).and_then(|s| xml.attr(s, "w:val"));
                let direct_outline = ppr.and_then(|p| xml.child(p, "w:outlineLvl")).and_then(|o| xml.attr(o, "w:val")).and_then(|v| v.parse::<u8>().ok()).filter(|&v| v < 9);
                if let Some(id) = &style_id {
                    block.style = Some(styles.name(id));
                }
                let numpr = ppr.and_then(|p| xml.child(p, "w:numPr"));
                if let Some(level) = style_id.as_deref().and_then(|id| styles.heading_level(id)).or(direct_outline.map(|v| v + 1)) {
                    block.kind = "heading".into();
                    block.level = Some(level);
                } else if let Some(numpr) = numpr.filter(|&n| xml.child(n, "w:numId").and_then(|i| xml.attr(i, "w:val")).as_deref() != Some("0")) {
                    block.kind = "list".into();
                    block.level = Some(xml.child(numpr, "w:ilvl").and_then(|l| xml.attr(l, "w:val")).and_then(|v| v.parse::<u8>().ok()).unwrap_or(0) + 1);
                } else if numpr.is_none() && style_id.as_deref().map(|id| styles.is_list(id)).unwrap_or(false) {
                    block.kind = "list".into();
                    block.level = Some(1);
                }
            }
            "w:tbl" => {
                block.kind = "table".into();
                let rows: Vec<Vec<String>> = table_rows(&xml, child).into_iter().map(|cells| cells.into_iter().map(|tc| cell_text(&xml, tc)).collect()).collect();
                block.text = rows.iter().map(|r| r.join(" | ")).collect::<Vec<_>>().join("\n");
                block.rows = Some(rows);
            }
            _ => {
                block.kind = "other".into();
                block.text = text_edit::paragraphs(&xml, child, &WORD).into_iter().map(|p| text_edit::paragraph_text(&xml, p, &WORD)).collect::<Vec<_>>().join("\n");
            }
        }
        blocks.push(block);
    }
    Ok(blocks)
}

/// 给模型看的块列表：每块一行，表格展开成 Markdown 表格
pub fn format_blocks(blocks: &[Block], max_chars_per_block: usize) -> String {
    let mut out = String::new();
    for b in blocks {
        let label = match b.kind.as_str() {
            "heading" => format!("标题{}", b.level.unwrap_or(1)),
            "list" => format!("列表·{}", b.level.unwrap_or(1)),
            "table" => {
                let rows = b.rows.as_ref().map(Vec::len).unwrap_or(0);
                let cols = b.rows.as_ref().and_then(|r| r.first()).map(Vec::len).unwrap_or(0);
                format!("表格 {rows}×{cols}")
            }
            "other" => "内容控件".into(),
            _ => "段落".into(),
        };
        let mut flags = String::new();
        if b.has_image {
            flags.push_str(" [含图片]");
        }
        if b.has_revisions {
            flags.push_str(" [含修订]");
        }
        if b.kind == "table" {
            out.push_str(&format!("[{}] {label}{flags}\n", b.index));
            if let Some(rows) = &b.rows {
                for row in rows {
                    let cells: Vec<String> = row.iter().map(|c| clip(&c.replace('\n', " / ").replace('|', "\\|"), 60)).collect();
                    out.push_str(&format!("    | {} |\n", cells.join(" | ")));
                }
            }
        } else {
            let text = if b.text.is_empty() { "（空）".to_string() } else { clip(&b.text.replace('\n', "↵"), max_chars_per_block) };
            out.push_str(&format!("[{}] {label}{flags} | {text}\n", b.index));
        }
    }
    out
}

// ───────────────────────── 编辑操作 ─────────────────────────

fn default_true() -> bool {
    true
}

/// 一个编辑操作。块序号都指读取时（本批操作之前）的文档。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum DocOp {
    /// 查找替换；不给 block 时在全文查找
    ReplaceText {
        #[serde(default)]
        block: Option<usize>,
        find: String,
        replace: String,
        #[serde(default = "default_true")]
        all: bool,
    },
    /// 重写整个段落（保留段落格式与首个文字片段的字体），text 支持行内 Markdown
    ReplaceBlock {
        block: usize,
        text: String,
        #[serde(default)]
        style: Option<String>,
    },
    /// 插入 Markdown 内容：after / before 某块，或 at = "start" / "end"
    Insert {
        #[serde(default)]
        after: Option<usize>,
        #[serde(default)]
        before: Option<usize>,
        #[serde(default)]
        at: Option<String>,
        markdown: String,
    },
    /// 删除一块，或 from..=to 一段
    Delete {
        #[serde(default)]
        block: Option<usize>,
        #[serde(default)]
        from: Option<usize>,
        #[serde(default)]
        to: Option<usize>,
    },
    Move {
        block: usize,
        #[serde(default)]
        after: Option<usize>,
        #[serde(default)]
        before: Option<usize>,
    },
    /// 改段落样式：heading 1–6 / title / normal / quote，或文档里已有的样式名
    SetStyle { block: usize, style: String },
    /// 改表格单元格（行列从 0 开始）
    SetCell { block: usize, row: usize, col: usize, text: String },
    /// 给一块加批注
    Comment { block: usize, text: String },
}

impl DocOp {
    fn name(&self) -> &'static str {
        match self {
            Self::ReplaceText { .. } => "replace_text",
            Self::ReplaceBlock { .. } => "replace_block",
            Self::Insert { .. } => "insert",
            Self::Delete { .. } => "delete",
            Self::Move { .. } => "move",
            Self::SetStyle { .. } => "set_style",
            Self::SetCell { .. } => "set_cell",
            Self::Comment { .. } => "comment",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    /// 写成 Word 修订
    pub track_changes: bool,
    pub author: String,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self { track_changes: false, author: "OfficePilot AI".into() }
    }
}

const RPR_ORDER: &[&str] = &[
    "w:rStyle", "w:rFonts", "w:b", "w:bCs", "w:i", "w:iCs", "w:caps", "w:smallCaps", "w:strike", "w:dstrike", "w:outline", "w:shadow", "w:emboss", "w:imprint",
    "w:noProof", "w:snapToGrid", "w:vanish", "w:webHidden", "w:color", "w:spacing", "w:w", "w:kern", "w:position", "w:sz", "w:szCs", "w:highlight", "w:u",
    "w:effect", "w:bdr", "w:shd", "w:fitText", "w:vertAlign", "w:rtl", "w:cs", "w:em", "w:lang", "w:eastAsianLayout", "w:specVanish", "w:oMath",
];

/// 子元素 (名字, 原文)
fn children_of_fragment(inner: &str) -> Result<Vec<(String, String)>> {
    let wrapped = format!("<x>{inner}</x>");
    let xml = Xml::parse(&wrapped)?;
    let root = xml.document_element().unwrap();
    Ok(xml.children(root).iter().map(|&c| (xml.name(c).to_string(), xml.outer(c).to_string())).collect())
}

#[derive(Default, Clone, Copy)]
struct RunStyle {
    bold: bool,
    italic: bool,
    code: bool,
    link: bool,
}

/// 在基础字符格式上叠加加粗 / 斜体 / 代码字体，按架构顺序排列
fn build_rpr(base_inner: &str, style: RunStyle, link_style: Option<&str>) -> Result<String> {
    let mut children: Vec<(String, String)> = children_of_fragment(base_inner)?.into_iter().filter(|(n, _)| n != "w:rPrChange" && n != "w:ins" && n != "w:del").collect();
    let set = |name: &str, outer: String, children: &mut Vec<(String, String)>| {
        children.retain(|(n, _)| n != name);
        children.push((name.to_string(), outer));
    };
    if style.bold {
        set("w:b", "<w:b/>".into(), &mut children);
        set("w:bCs", "<w:bCs/>".into(), &mut children);
    }
    if style.italic {
        set("w:i", "<w:i/>".into(), &mut children);
        set("w:iCs", "<w:iCs/>".into(), &mut children);
    }
    if style.code {
        set("w:rFonts", r#"<w:rFonts w:ascii="Consolas" w:hAnsi="Consolas" w:cs="Consolas"/>"#.into(), &mut children);
    }
    if style.link {
        match link_style {
            Some(id) => set("w:rStyle", format!(r#"<w:rStyle w:val="{}"/>"#, xml::escape_attr(id)), &mut children),
            None => {
                set("w:color", r#"<w:color w:val="0563C1"/>"#.into(), &mut children);
                set("w:u", r#"<w:u w:val="single"/>"#.into(), &mut children);
            }
        }
    }
    if children.is_empty() {
        return Ok(String::new());
    }
    let rank = |n: &str| RPR_ORDER.iter().position(|o| *o == n).unwrap_or(RPR_ORDER.len());
    children.sort_by_key(|(n, _)| rank(n));
    Ok(format!("<w:rPr>{}</w:rPr>", children.into_iter().map(|(_, o)| o).collect::<String>()))
}

fn text_run(rpr: &str, text: &str) -> String {
    // 换行写成 w:br，制表符写成 w:tab
    let mut body = String::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            body.push_str("<w:br/>");
        }
        for (j, part) in line.split('\t').enumerate() {
            if j > 0 {
                body.push_str("<w:tab/>");
            }
            if !part.is_empty() {
                body.push_str(&format!(r#"<w:t xml:space="preserve">{}</w:t>"#, xml::escape(part)));
            }
        }
    }
    format!("<w:r>{rpr}{body}</w:r>")
}

/// 片段（块）的纯文字，用于修改记录
fn fragment_text(src: &str) -> String {
    let Ok(xml) = Xml::parse(src) else { return String::new() };
    let Some(root) = xml.document_element() else { return String::new() };
    text_edit::paragraphs(&xml, root, &WORD).into_iter().map(|p| text_edit::paragraph_text(&xml, p, &WORD)).collect::<Vec<_>>().join("\n")
}

fn max_w_id(text: &str) -> u64 {
    let mut max = 0;
    let mut rest = text;
    while let Some(pos) = rest.find("w:id=\"") {
        rest = &rest[pos + 6..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u64>() {
            max = max.max(n);
        }
    }
    max
}

enum StyleTarget {
    Normal,
    Id(String),
}

struct Ctx<'d> {
    tracked: bool,
    author: String,
    date: String,
    next_id: u64,
    styles: &'d mut Styles,
    styles_part: &'d mut Option<String>,
    pkg: &'d mut Package,
    main: String,
    bullet_num: Option<String>,
    text_width: u32,
}

impl<'d> Ctx<'d> {
    fn rev(&mut self) -> String {
        self.next_id += 1;
        format!(r#"w:id="{}" w:author="{}" w:date="{}""#, self.next_id, xml::escape_attr(&self.author), self.date)
    }

    // ───── 样式 ─────

    fn ensure_styles_part(&mut self) -> Result<String> {
        if let Some(part) = self.styles_part.clone() {
            if self.pkg.has(&part) {
                return Ok(part);
            }
        }
        let part = "word/styles.xml".to_string();
        self.pkg.write(&part, format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{NS_W}"></w:styles>"#));
        let target = Package::relative(&self.main, &part);
        self.pkg.add_rel(&self.main, REL_STYLES, &target, false)?;
        self.pkg.content_type_override(&part, CT_STYLES)?;
        *self.styles_part = Some(part.clone());
        Ok(part)
    }

    fn add_style(&mut self, id: &str, xml_text: String, info: StyleInfo) -> Result<()> {
        let part = self.ensure_styles_part()?;
        let text = self.pkg.read_str(&part)?;
        self.pkg.write(&part, insert_before_close(&text, "w:styles", &xml_text)?);
        self.styles.by_id.insert(id.to_string(), info);
        Ok(())
    }

    fn heading_style(&mut self, level: u8) -> Result<String> {
        let level = level.clamp(1, 6);
        if let Some(id) = self.styles.find_by_name(&format!("heading {level}"), "paragraph") {
            return Ok(id);
        }
        if let Some((id, _)) = self.styles.by_id.iter().find(|(id, s)| s.kind == "paragraph" && self.styles.heading_level(id) == Some(level) && s.name.to_lowercase() != "title") {
            return Ok(id.clone());
        }
        let id = format!("Heading{level}");
        let normal = self.styles.normal();
        let size = [32, 28, 26, 24, 22, 22][level as usize - 1];
        let xml_text = format!(
            r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="heading {level}"/><w:basedOn w:val="{normal}"/><w:next w:val="{normal}"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before="240" w:after="120"/><w:outlineLvl w:val="{}"/></w:pPr><w:rPr><w:b/><w:bCs/><w:sz w:val="{size}"/><w:szCs w:val="{size}"/></w:rPr></w:style>"#,
            level - 1
        );
        self.add_style(&id, xml_text, StyleInfo { name: format!("heading {level}"), kind: "paragraph".into(), outline: Some(level), based_on: Some(normal), default: false, list: false })?;
        Ok(id)
    }

    fn resolve_style(&mut self, wanted: &str) -> Result<StyleTarget> {
        let lower = wanted.trim().to_lowercase();
        if matches!(lower.as_str(), "normal" | "正文" | "body" | "paragraph" | "段落" | "body text") {
            return Ok(StyleTarget::Normal);
        }
        let compact = lower.replace(' ', "");
        for prefix in ["heading", "标题", "h"] {
            if let Some(n) = compact.strip_prefix(prefix).and_then(|n| n.parse::<u8>().ok()) {
                if (1..=6).contains(&n) {
                    return Ok(StyleTarget::Id(self.heading_style(n)?));
                }
            }
        }
        if matches!(compact.as_str(), "title" | "标题" | "文档标题") {
            return Ok(StyleTarget::Id(match self.styles.find_by_name("title", "paragraph") {
                Some(id) => id,
                None => self.heading_style(1)?,
            }));
        }
        if self.styles.by_id.get(wanted).map(|s| s.kind == "paragraph").unwrap_or(false) {
            return Ok(StyleTarget::Id(wanted.to_string()));
        }
        if let Some(id) = self.styles.find_by_name(wanted, "paragraph") {
            return Ok(StyleTarget::Id(id));
        }
        if matches!(compact.as_str(), "quote" | "引用") {
            return Ok(StyleTarget::Id(self.quote_style()?));
        }
        let mut names: Vec<String> = self.styles.by_id.values().filter(|s| s.kind == "paragraph").map(|s| s.name.clone()).collect();
        names.sort();
        names.truncate(20);
        Err(EngineError::Invalid(format!("找不到段落样式“{wanted}”。可用：heading 1–6、title、normal、quote，或文档已有样式：{}", names.join("、"))))
    }

    fn quote_style(&mut self) -> Result<String> {
        if let Some(id) = self.styles.find_by_name("quote", "paragraph") {
            return Ok(id);
        }
        let normal = self.styles.normal();
        let xml_text = format!(
            r#"<w:style w:type="paragraph" w:styleId="Quote"><w:name w:val="Quote"/><w:basedOn w:val="{normal}"/><w:next w:val="{normal}"/><w:uiPriority w:val="29"/><w:qFormat/><w:pPr><w:ind w:left="720" w:right="720"/></w:pPr><w:rPr><w:i/><w:iCs/><w:color w:val="595959"/></w:rPr></w:style>"#
        );
        self.add_style("Quote", xml_text, StyleInfo { name: "Quote".into(), kind: "paragraph".into(), outline: None, based_on: Some(normal), default: false, list: false })?;
        Ok("Quote".into())
    }

    fn optional_style(&self, name: &str, kind: &str) -> Option<String> {
        self.styles.find_by_name(name, kind)
    }

    // ───── 编号（列表） ─────

    fn numbering_part(&mut self) -> Result<String> {
        if let Some(part) = self.pkg.target_of(&self.main, "/numbering")? {
            if self.pkg.has(&part) {
                return Ok(part);
            }
        }
        let part = "word/numbering.xml".to_string();
        self.pkg.write(&part, format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="{NS_W}"></w:numbering>"#));
        let target = Package::relative(&self.main, &part);
        self.pkg.add_rel(&self.main, REL_NUMBERING, &target, false)?;
        self.pkg.content_type_override(&part, CT_NUMBERING)?;
        Ok(part)
    }

    /// 返回 (bullet 编号, 文档里第一种 bullet / decimal 的 abstractNumId, 最大 abstractNumId, 最大 numId)
    fn scan_numbering(&mut self) -> Result<(String, Option<String>, Option<String>, Option<String>, u64, u64)> {
        let part = self.numbering_part()?;
        let text = self.pkg.read_str(&part)?;
        let xml = Xml::parse(&text)?;
        let root = xml.document_element().ok_or_else(|| EngineError::Format("numbering.xml 为空".into()))?;
        let mut fmt_of: HashMap<String, String> = HashMap::new();
        let mut max_abs = 0;
        for a in xml.children_named(root, "w:abstractNum") {
            let id = xml.attr(a, "w:abstractNumId").unwrap_or_default();
            max_abs = max_abs.max(id.parse::<u64>().unwrap_or(0));
            let fmt = xml
                .children_named(a, "w:lvl")
                .find(|&l| xml.attr(l, "w:ilvl").as_deref() == Some("0"))
                .and_then(|l| xml.child(l, "w:numFmt"))
                .and_then(|f| xml.attr(f, "w:val"))
                .unwrap_or_default();
            fmt_of.insert(id, fmt);
        }
        let mut max_num = 0;
        let mut bullet_num = None;
        for n in xml.children_named(root, "w:num") {
            let id = xml.attr(n, "w:numId").unwrap_or_default();
            max_num = max_num.max(id.parse::<u64>().unwrap_or(0));
            let abs = xml.child(n, "w:abstractNumId").and_then(|a| xml.attr(a, "w:val")).unwrap_or_default();
            if bullet_num.is_none() && fmt_of.get(&abs).map(|f| f == "bullet").unwrap_or(false) {
                bullet_num = Some(id);
            }
        }
        let bullet_abs = fmt_of.iter().find(|(_, f)| *f == "bullet").map(|(id, _)| id.clone());
        let decimal_abs = fmt_of.iter().find(|(_, f)| *f == "decimal").map(|(id, _)| id.clone());
        Ok((part, bullet_num, bullet_abs, decimal_abs, max_abs, max_num))
    }

    fn add_numbering(&mut self, part: &str, abstract_xml: Option<String>, num_xml: String) -> Result<()> {
        let text = self.pkg.read_str(part)?;
        let xml = Xml::parse(&text)?;
        let root = xml.document_element().unwrap();
        let mut splice = Splice::new();
        let first_num = xml.children(root).iter().copied().find(|&c| xml.name(c) != "w:abstractNum");
        let n = xml.node(root);
        if n.self_closing {
            let updated = format!("{}{}{}</w:numbering>", xml.open_tag_expanded(root), abstract_xml.unwrap_or_default(), num_xml);
            splice.replace(n.start, n.end, updated);
        } else {
            if let Some(a) = abstract_xml {
                splice.insert(first_num.map(|f| xml.node(f).start).unwrap_or(n.close_start), a);
            }
            let after_nums = xml.children(root).iter().copied().find(|&c| !matches!(xml.name(c), "w:abstractNum" | "w:num"));
            splice.insert(after_nums.map(|f| xml.node(f).start).unwrap_or(n.close_start), num_xml);
        }
        let updated = splice.apply(&text)?;
        self.pkg.write(part, updated);
        Ok(())
    }

    fn list_num(&mut self, numbered: bool) -> Result<String> {
        if !numbered {
            if let Some(id) = &self.bullet_num {
                return Ok(id.clone());
            }
        }
        let (part, bullet_num, bullet_abs, decimal_abs, max_abs, max_num) = self.scan_numbering()?;
        if !numbered {
            if let Some(id) = bullet_num {
                self.bullet_num = Some(id.clone());
                return Ok(id);
            }
        }
        let existing_abs = if numbered { decimal_abs } else { bullet_abs };
        let (abs_id, abstract_xml) = match existing_abs {
            Some(id) => (id, None),
            None => {
                let id = (max_abs + 1).to_string();
                (id.clone(), Some(abstract_num_xml(&id, numbered)))
            }
        };
        let num_id = (max_num + 1).to_string();
        let restart = if numbered { r#"<w:lvlOverride w:ilvl="0"><w:startOverride w:val="1"/></w:lvlOverride>"# } else { "" };
        let num_xml = format!(r#"<w:num w:numId="{num_id}"><w:abstractNumId w:val="{abs_id}"/>{restart}</w:num>"#);
        self.add_numbering(&part, abstract_xml, num_xml)?;
        if !numbered {
            self.bullet_num = Some(num_id.clone());
        }
        Ok(num_id)
    }

    // ───── 生成内容 ─────

    fn runs(&mut self, spans: &[Span], base_rpr: &str, extra: RunStyle) -> Result<String> {
        let mut out = String::new();
        let link_style = self.optional_style("hyperlink", "character");
        for span in spans {
            if span.text.is_empty() {
                continue;
            }
            let style = RunStyle { bold: span.bold || extra.bold, italic: span.italic || extra.italic, code: span.code || extra.code, link: span.link.is_some() };
            let rpr = build_rpr(base_rpr, style, link_style.as_deref())?;
            let mut run = text_run(&rpr, &span.text);
            if self.tracked {
                run = format!("<w:ins {}>{run}</w:ins>", self.rev());
            }
            match &span.link {
                Some(url) if !url.trim().is_empty() => {
                    let id = self.pkg.add_rel(&self.main, REL_HYPERLINK, url.trim(), true)?;
                    out.push_str(&format!(r#"<w:hyperlink r:id="{id}" w:history="1">{run}</w:hyperlink>"#));
                }
                _ => out.push_str(&run),
            }
        }
        Ok(out)
    }

    fn mark_ins(&mut self) -> String {
        if self.tracked {
            format!("<w:rPr><w:ins {}/></w:rPr>", self.rev())
        } else {
            String::new()
        }
    }

    fn paragraph(&mut self, ppr_inner: &str, spans: &[Span], extra: RunStyle) -> Result<String> {
        let runs = self.runs(spans, "", extra)?;
        let mark = self.mark_ins();
        let ppr = format!("{ppr_inner}{mark}");
        Ok(if ppr.is_empty() { format!("<w:p>{runs}</w:p>") } else { format!("<w:p><w:pPr>{ppr}</w:pPr>{runs}</w:p>") })
    }

    fn table(&mut self, rows: &[Vec<Vec<Span>>]) -> Result<String> {
        let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let width = self.text_width / cols as u32;
        let style = match self.optional_style("table grid", "table") {
            Some(id) => format!(r#"<w:tblStyle w:val="{id}"/>"#),
            None => String::new(),
        };
        let borders = if style.is_empty() {
            let b = |side: &str| format!(r#"<w:{side} w:val="single" w:sz="4" w:space="0" w:color="BFBFBF"/>"#);
            format!("<w:tblBorders>{}{}{}{}{}{}</w:tblBorders>", b("top"), b("left"), b("bottom"), b("right"), b("insideH"), b("insideV"))
        } else {
            String::new()
        };
        let grid: String = (0..cols).map(|_| format!(r#"<w:gridCol w:w="{width}"/>"#)).collect();
        let mut out = format!(r#"<w:tbl><w:tblPr>{style}<w:tblW w:w="0" w:type="auto"/>{borders}<w:tblLook w:val="04A0" w:firstRow="1" w:lastRow="0" w:firstColumn="1" w:lastColumn="0" w:noHBand="0" w:noVBand="1"/></w:tblPr><w:tblGrid>{grid}</w:tblGrid>"#);
        for (r, row) in rows.iter().enumerate() {
            let mut trpr = String::new();
            if r == 0 {
                trpr.push_str("<w:tblHeader/>");
            }
            if self.tracked {
                trpr.push_str(&format!("<w:ins {}/>", self.rev()));
            }
            out.push_str("<w:tr>");
            if !trpr.is_empty() {
                out.push_str(&format!("<w:trPr>{trpr}</w:trPr>"));
            }
            for c in 0..cols {
                let spans = row.get(c).cloned().unwrap_or_default();
                let p = self.paragraph("", &spans, RunStyle { bold: r == 0, ..Default::default() })?;
                out.push_str(&format!(r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/></w:tcPr>{p}</w:tc>"#));
            }
            out.push_str("</w:tr>");
        }
        out.push_str("</w:tbl>");
        Ok(out)
    }

    fn markdown(&mut self, md: &str) -> Result<String> {
        let blocks = markdown::parse(md);
        if blocks.is_empty() {
            return Err(EngineError::Invalid("markdown 内容为空".into()));
        }
        let list_style = self.optional_style("list paragraph", "paragraph").map(|id| format!(r#"<w:pStyle w:val="{id}"/>"#)).unwrap_or_default();
        let mut out = String::new();
        let mut numbered_list: Option<String> = None;
        for block in &blocks {
            if !matches!(block, markdown::Block::Numbered { .. }) {
                numbered_list = None;
            }
            let xml_text = match block {
                markdown::Block::Heading { level, spans } => {
                    let id = self.heading_style(*level)?;
                    self.paragraph(&format!(r#"<w:pStyle w:val="{id}"/>"#), spans, RunStyle::default())?
                }
                markdown::Block::Para(spans) => self.paragraph("", spans, RunStyle::default())?,
                markdown::Block::Bullet { level, spans } => {
                    let num = self.list_num(false)?;
                    self.paragraph(&format!(r#"{list_style}<w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{num}"/></w:numPr>"#), spans, RunStyle::default())?
                }
                markdown::Block::Numbered { level, spans } => {
                    let num = match &numbered_list {
                        Some(id) => id.clone(),
                        None => {
                            let id = self.list_num(true)?;
                            numbered_list = Some(id.clone());
                            id
                        }
                    };
                    self.paragraph(&format!(r#"{list_style}<w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{num}"/></w:numPr>"#), spans, RunStyle::default())?
                }
                markdown::Block::Quote(spans) => {
                    let id = self.quote_style()?;
                    self.paragraph(&format!(r#"<w:pStyle w:val="{id}"/>"#), spans, RunStyle::default())?
                }
                markdown::Block::Code(lines) => {
                    let mut s = String::new();
                    for line in lines {
                        let span = Span { text: line.clone(), ..Default::default() };
                        s.push_str(&self.paragraph(r#"<w:spacing w:before="0" w:after="0"/>"#, &[span], RunStyle { code: true, ..Default::default() })?);
                    }
                    s
                }
                markdown::Block::Table(rows) => self.table(rows)?,
            };
            out.push_str(&xml_text);
        }
        Ok(out)
    }

    // ───── 改写已有内容 ─────

    /// 修订模式：把片段里的文字片段都标成删除；mark_paragraphs 时段落标记也标成删除（整段删除）
    fn mark_deleted(&mut self, src: &str, mark_paragraphs: bool) -> Result<String> {
        let xml = Xml::parse(src)?;
        let root = xml.document_element().unwrap_or(xml::ROOT);
        if xml.descendants(root).iter().any(|&d| matches!(xml.name(d), "w:ins" | "w:del")) {
            return Err(EngineError::Invalid("这部分内容已有修订，请先在 Word 里接受或拒绝修订，或关闭修订模式再改".into()));
        }
        let mut splice = Splice::new();
        let paragraphs = text_edit::paragraphs(&xml, root, &WORD);
        for &p in &paragraphs {
            for d in xml.descendants(p) {
                if xml.name(d) != "w:r" || xml.has_ancestor(d, "w:r", p) || WORD.skip.iter().any(|s| xml.has_ancestor(d, s, p)) || xml.has_ancestor(d, "w:p", p) {
                    continue;
                }
                let rev = self.rev();
                splice.replace(xml.node(d).start, xml.node(d).end, format!("<w:del {rev}>{}</w:del>", to_deleted_run(&xml, d)?));
            }
            if mark_paragraphs {
                let rev = self.rev();
                insert_paragraph_mark(&xml, p, &format!("<w:del {rev}/>"), &mut splice);
            }
        }
        if mark_paragraphs {
            for d in xml.descendants(root).into_iter().chain(std::iter::once(root)) {
                if xml.name(d) != "w:tr" {
                    continue;
                }
                let rev = self.rev();
                let mark = format!("<w:del {rev}/>");
                match xml.child(d, "w:trPr") {
                    Some(trpr) if !xml.node(trpr).self_closing => {
                        let at = xml.child(trpr, "w:trPrChange").map(|c| xml.node(c).start).unwrap_or(xml.node(trpr).close_start);
                        splice.insert(at, mark);
                    }
                    Some(trpr) => splice.replace(xml.node(trpr).start, xml.node(trpr).end, format!("<w:trPr>{mark}</w:trPr>")),
                    None => {
                        let at = xml.child(d, "w:tblPrEx").map(|c| xml.node(c).end).unwrap_or(xml.node(d).open_end);
                        splice.insert(at, format!("<w:trPr>{mark}</w:trPr>"));
                    }
                }
            }
        }
        splice.apply(src)
    }

    /// 重写段落文字：普通模式直接替换；修订模式把旧文字标成删除、新文字标成插入
    fn rewrite_paragraph(&mut self, src: &str, spans: &[Span], style: Option<&StyleTarget>) -> Result<String> {
        let xml = Xml::parse(src)?;
        let p = xml.document_element().filter(|&p| xml.name(p) == "w:p").ok_or_else(|| EngineError::Invalid("这一块不是段落".into()))?;
        let base_rpr = text_edit::atoms(&xml, p, &WORD)
            .iter()
            .filter(|a| a.is_text)
            .find_map(|a| {
                let mut cur = xml.node(a.node).parent;
                while let Some(n) = cur {
                    if xml.name(n) == "w:r" {
                        return Some(xml.child(n, "w:rPr").map(|r| xml.inner(r).to_string()).unwrap_or_default());
                    }
                    cur = xml.node(n).parent;
                }
                None
            })
            .unwrap_or_default();
        let ppr = xml.child(p, "w:pPr");
        let ppr_text = match (ppr, style) {
            (Some(ppr), Some(style)) => self.restyle_ppr(xml.outer(ppr), style)?,
            (None, Some(style)) => self.restyle_ppr("<w:pPr></w:pPr>", style)?,
            (Some(ppr), None) => xml.outer(ppr).to_string(),
            (None, None) => String::new(),
        };
        if !self.tracked {
            let runs = self.runs(spans, &base_rpr, RunStyle::default())?;
            let ppr_text = if ppr_text == "<w:pPr></w:pPr>" { String::new() } else { ppr_text };
            return Ok(format!("{}{ppr_text}{runs}</w:p>", xml.open_tag_expanded(p)));
        }
        // 修订：旧文字全部删除，新文字插在段尾
        let deleted = self.mark_deleted(src, false)?;
        let dxml = Xml::parse(&deleted)?;
        let dp = dxml.document_element().unwrap();
        let runs = self.runs(spans, &base_rpr, RunStyle::default())?;
        let mut splice = Splice::new();
        if let Some(dppr) = dxml.child(dp, "w:pPr") {
            if style.is_some() {
                splice.replace(dxml.node(dppr).start, dxml.node(dppr).end, ppr_text.clone());
            }
        } else if style.is_some() {
            splice.insert(dxml.node(dp).open_end, ppr_text.clone());
        }
        let n = dxml.node(dp);
        if n.self_closing {
            return Ok(format!("{}{ppr_text}{runs}</w:p>", dxml.open_tag_expanded(dp)));
        }
        splice.insert(n.close_start, runs);
        splice.apply(&deleted)
    }

    /// 改段落属性里的样式；修订模式附上 w:pPrChange
    fn restyle_ppr(&mut self, ppr_outer: &str, style: &StyleTarget) -> Result<String> {
        let xml = Xml::parse(ppr_outer)?;
        let ppr = xml.document_element().unwrap();
        let mut splice = Splice::new();
        let open = xml.open_tag_expanded(ppr);
        let n = xml.node(ppr);
        let mut inner = String::new();
        let old_children: String = xml
            .children(ppr)
            .iter()
            .filter(|&&c| !matches!(xml.name(c), "w:rPr" | "w:sectPr" | "w:pPrChange"))
            .map(|&c| xml.outer(c))
            .collect();
        let is_heading = matches!(style, StyleTarget::Id(id) if self.styles.heading_level(id).is_some());
        let mut placed = false;
        for &c in xml.children(ppr) {
            let name = xml.name(c);
            match name {
                "w:pStyle" => {
                    if let StyleTarget::Id(id) = style {
                        inner.push_str(&format!(r#"<w:pStyle w:val="{}"/>"#, xml::escape_attr(id)));
                    }
                    placed = true;
                }
                "w:numPr" if is_heading => {}
                "w:pPrChange" if self.tracked => {}
                _ => {
                    if !placed {
                        if let StyleTarget::Id(id) = style {
                            inner.push_str(&format!(r#"<w:pStyle w:val="{}"/>"#, xml::escape_attr(id)));
                        }
                        placed = true;
                    }
                    inner.push_str(xml.outer(c));
                }
            }
        }
        if !placed {
            if let StyleTarget::Id(id) = style {
                inner.push_str(&format!(r#"<w:pStyle w:val="{}"/>"#, xml::escape_attr(id)));
            }
        }
        if self.tracked {
            let rev = self.rev();
            inner.push_str(&format!("<w:pPrChange {rev}><w:pPr>{old_children}</w:pPr></w:pPrChange>"));
        }
        splice.replace(n.start, n.end, if inner.is_empty() { String::new() } else { format!("{open}{inner}</w:pPr>") });
        splice.apply(ppr_outer)
    }
}

fn abstract_num_xml(id: &str, numbered: bool) -> String {
    let mut lvls = String::new();
    for l in 0..9u32 {
        let ind = 720 * (l + 1);
        let (fmt, text) = if numbered {
            (["decimal", "lowerLetter", "lowerRoman"][l as usize % 3], format!("%{}.", l + 1))
        } else {
            ("bullet", ["•", "◦", "▪"][l as usize % 3].to_string())
        };
        lvls.push_str(&format!(
            r#"<w:lvl w:ilvl="{l}"><w:start w:val="1"/><w:numFmt w:val="{fmt}"/><w:lvlText w:val="{text}"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="{ind}" w:hanging="360"/></w:pPr></w:lvl>"#
        ));
    }
    format!(r#"<w:abstractNum w:abstractNumId="{id}"><w:multiLevelType w:val="hybridMultilevel"/>{lvls}</w:abstractNum>"#)
}

/// 把文字片段改成删除形式：w:t → w:delText，w:instrText → w:delInstrText
fn to_deleted_run(xml: &Xml, run: usize) -> Result<String> {
    let src = xml.outer(run);
    let base = xml.node(run).start;
    let mut splice = Splice::new();
    for d in xml.descendants(run) {
        let (from, to) = match xml.name(d) {
            "w:t" => ("w:t", "w:delText"),
            "w:instrText" => ("w:instrText", "w:delInstrText"),
            _ => continue,
        };
        let n = xml.node(d);
        let open = xml.open_tag(d);
        splice.replace(n.start - base, n.open_end - base, format!("<{to}{}", &open[1 + from.len()..]));
        if !n.self_closing {
            splice.replace(n.close_start - base, n.end - base, format!("</{to}>"));
        }
    }
    splice.apply(src)
}

/// 在段落标记的字符属性里加一个修订标记（w:ins / w:del 必须放在最前）
fn insert_paragraph_mark(xml: &Xml, p: usize, mark: &str, splice: &mut Splice) {
    let pn = xml.node(p);
    match xml.child(p, "w:pPr") {
        Some(ppr) if !xml.node(ppr).self_closing => match xml.child(ppr, "w:rPr") {
            Some(rpr) if !xml.node(rpr).self_closing => splice.insert(xml.node(rpr).open_end, mark.to_string()),
            Some(rpr) => splice.replace(xml.node(rpr).start, xml.node(rpr).end, format!("<w:rPr>{mark}</w:rPr>")),
            None => {
                let at = xml.children(ppr).iter().copied().find(|&c| matches!(xml.name(c), "w:sectPr" | "w:pPrChange")).map(|c| xml.node(c).start).unwrap_or(xml.node(ppr).close_start);
                splice.insert(at, format!("<w:rPr>{mark}</w:rPr>"));
            }
        },
        Some(ppr) => splice.replace(xml.node(ppr).start, xml.node(ppr).end, format!("<w:pPr><w:rPr>{mark}</w:rPr></w:pPr>")),
        None if pn.self_closing => splice.replace(pn.start, pn.end, format!("{}<w:pPr><w:rPr>{mark}</w:rPr></w:pPr></w:p>", xml.open_tag_expanded(p))),
        None => splice.insert(pn.open_end, format!("<w:pPr><w:rPr>{mark}</w:rPr></w:pPr>")),
    }
}

impl<'d> Ctx<'d> {
    /// 修订模式的查找替换：把涉及的文字片段拆开，命中部分写成 w:del，替换内容写成 w:ins
    fn tracked_replace(&mut self, src: &str, find: &str, replace: &str, all: bool) -> Result<(String, usize)> {
        let xml = Xml::parse(src)?;
        let root = xml.document_element().unwrap_or(xml::ROOT);
        let mut splice = Splice::new();
        let mut count = 0;
        for para in text_edit::paragraphs(&xml, root, &WORD) {
            if count > 0 && !all {
                break;
            }
            let atoms = text_edit::atoms(&xml, para, &WORD);
            let text: String = atoms.iter().map(|a| a.text.as_str()).collect();
            let hits = text_edit::find_hits(&atoms, &text, find, all);
            if hits.is_empty() {
                continue;
            }
            count += hits.len();
            // 每个原子所在的文字片段
            let run_of = |node: usize| -> Option<usize> {
                let mut cur = xml.node(node).parent;
                while let Some(n) = cur {
                    if xml.name(n) == "w:r" {
                        return Some(n);
                    }
                    if n == para {
                        return None;
                    }
                    cur = xml.node(n).parent;
                }
                None
            };
            let mut touched: Vec<usize> = Vec::new();
            for atom in &atoms {
                let (a, b) = (atom.offset, atom.offset + atom.text.len());
                let hit = hits.iter().any(|h| h.start < b && h.end > a || (h.anchor >= a && h.anchor < b));
                if !hit {
                    continue;
                }
                let run = run_of(atom.node).ok_or_else(|| EngineError::Invalid("命中的文字不在普通文字片段里，无法以修订方式替换".into()))?;
                if xml.has_ancestor(run, "w:ins", para) || xml.has_ancestor(run, "w:del", para) {
                    return Err(EngineError::Invalid("命中的文字已有修订，请先在 Word 里处理修订或关闭修订模式".into()));
                }
                if !touched.contains(&run) {
                    touched.push(run);
                }
            }
            for run in touched {
                let rpr = xml.child(run, "w:rPr").map(|r| xml.outer(r)).unwrap_or("");
                let open = xml.open_tag_expanded(run);
                let mut pieces: Vec<(u8, String)> = Vec::new(); // 0 保留 / 1 删除 / 2 插入
                let push = |kind: u8, content: String, pieces: &mut Vec<(u8, String)>| match pieces.last_mut() {
                    Some((k, c)) if *k == kind && kind != 2 => c.push_str(&content),
                    _ => pieces.push((kind, content)),
                };
                for &child in xml.children(run) {
                    if xml.name(child) == "w:rPr" {
                        continue;
                    }
                    let atom = atoms.iter().find(|a| a.node == child);
                    match atom {
                        Some(atom) if atom.is_text => {
                            for (i, ch) in atom.text.char_indices() {
                                let pos = atom.offset + i;
                                for _ in hits.iter().filter(|h| h.anchor == pos) {
                                    push(2, replace.to_string(), &mut pieces);
                                }
                                let deleted = hits.iter().any(|h| pos >= h.start && pos < h.end);
                                push(if deleted { 1 } else { 0 }, format!("\u{0}{ch}"), &mut pieces);
                            }
                        }
                        Some(atom) => {
                            let deleted = hits.iter().any(|h| atom.offset >= h.start && atom.offset < h.end);
                            push(if deleted { 1 } else { 0 }, xml.outer(child).to_string(), &mut pieces);
                        }
                        None => push(0, xml.outer(child).to_string(), &mut pieces),
                    }
                }
                let mut out = String::new();
                for (kind, content) in pieces {
                    match kind {
                        2 => {
                            let rev = self.rev();
                            out.push_str(&format!("<w:ins {rev}><w:r>{rpr}{}</w:r></w:ins>", text_body(&content, "w:t")));
                        }
                        1 => {
                            let rev = self.rev();
                            out.push_str(&format!("<w:del {rev}><w:r>{rpr}{}</w:r></w:del>", marked_body(&content, "w:delText")));
                        }
                        _ => out.push_str(&format!("{open}{rpr}{}</w:r>", marked_body(&content, "w:t"))),
                    }
                }
                splice.replace(xml.node(run).start, xml.node(run).end, out);
            }
        }
        Ok((splice.apply(src)?, count))
    }
}

/// 片段内容里 \0 标记的单个字符合并成文本节点，其余是原样的子元素
fn marked_body(content: &str, tag: &str) -> String {
    let mut out = String::new();
    let mut text = String::new();
    let mut chars = content.chars().peekable();
    let mut raw = String::new();
    while let Some(c) = chars.next() {
        if c == '\u{0}' {
            if !raw.is_empty() {
                out.push_str(&raw);
                raw.clear();
            }
            if let Some(ch) = chars.next() {
                text.push(ch);
            }
        } else {
            if !text.is_empty() {
                out.push_str(&format!(r#"<{tag} xml:space="preserve">{}</{tag}>"#, xml::escape(&text)));
                text.clear();
            }
            raw.push(c);
        }
    }
    if !text.is_empty() {
        out.push_str(&format!(r#"<{tag} xml:space="preserve">{}</{tag}>"#, xml::escape(&text)));
    }
    out.push_str(&raw);
    out
}

fn text_body(text: &str, tag: &str) -> String {
    let marked: String = text.chars().map(|c| format!("\u{0}{c}")).collect();
    marked_body(&marked, tag)
}

struct Slot {
    xml: Option<String>,
    before: Vec<String>,
    after: Vec<String>,
}

impl Document {
    pub fn apply(&mut self, ops: &[DocOp], opts: &ApplyOptions) -> Result<ApplyReport> {
        if ops.is_empty() {
            return Err(EngineError::Invalid("没有要执行的操作".into()));
        }
        let blocks = self.blocks()?;
        let n = blocks.len();
        let mut slots: Vec<Slot> = blocks.iter().map(|b| Slot { xml: Some(self.text[b.start..b.end].to_string()), before: Vec::new(), after: Vec::new() }).collect();
        let mut end_inserts: Vec<String> = Vec::new();
        let mut changes: Vec<Change> = Vec::new();

        let comments_part = self.pkg.target_of(&self.main, "/comments")?;
        let comments_text = match &comments_part {
            Some(p) if self.pkg.has(p) => self.pkg.read_str(p)?,
            _ => String::new(),
        };
        let text_width = section_text_width(&self.text);
        let mut ctx = Ctx {
            tracked: opts.track_changes,
            author: if opts.author.trim().is_empty() { "OfficePilot AI".into() } else { opts.author.clone() },
            date: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            next_id: max_w_id(&self.text).max(max_w_id(&comments_text)),
            styles: &mut self.styles,
            styles_part: &mut self.styles_part,
            pkg: &mut self.pkg,
            main: self.main.clone(),
            bullet_num: None,
            text_width,
        };

        let check = |i: usize, op: &str, idx: usize| -> Result<()> {
            if idx >= n {
                return Err(op_err(i, op, format!("块序号 {idx} 超出范围（文档共 {n} 块，序号 0–{}）", n.saturating_sub(1))));
            }
            Ok(())
        };

        for (i, op) in ops.iter().enumerate() {
            let name = op.name();
            let wrap = |e: EngineError| match e {
                EngineError::Op { .. } => e,
                other => op_err(i, name, other.to_string()),
            };
            match op {
                DocOp::ReplaceText { block, find, replace, all } => {
                    if find.is_empty() {
                        return Err(op_err(i, name, "find 不能为空"));
                    }
                    let targets: Vec<usize> = match block {
                        Some(b) => {
                            check(i, name, *b)?;
                            vec![*b]
                        }
                        None => (0..n).collect(),
                    };
                    let mut total = 0;
                    for b in targets {
                        let Some(src) = slots[b].xml.clone() else {
                            if block.is_some() {
                                return Err(op_err(i, name, format!("块 {b} 已在本批前面的操作里删除或移动")));
                            }
                            continue;
                        };
                        let remaining_all = *all || total == 0;
                        if !remaining_all {
                            break;
                        }
                        let (out, count) = if ctx.tracked { ctx.tracked_replace(&src, find, replace, *all).map_err(wrap)? } else { text_edit::replace_plain(&src, &WORD, find, replace, *all).map_err(wrap)? };
                        if count > 0 {
                            changes.push(Change { op: i, kind: name.into(), target: format!("块 {b}"), before: Some(clip(&fragment_text(&src), 300)), after: Some(clip(&fragment_text(&out), 300)) });
                            slots[b].xml = Some(out);
                            total += count;
                        }
                    }
                    if total == 0 {
                        let hint = match block {
                            Some(b) => format!("块 {b} 的原文是：“{}”", clip(&fragment_text(slots[*b].xml.as_deref().unwrap_or("")), 200)),
                            None => "全文都没有命中".into(),
                        };
                        return Err(op_err(i, name, format!("没有找到“{find}”，find 要与原文逐字一致（包括标点、空格）。{hint}")));
                    }
                }
                DocOp::ReplaceBlock { block, text, style } => {
                    check(i, name, *block)?;
                    let src = slots[*block].xml.clone().ok_or_else(|| op_err(i, name, format!("块 {block} 已在本批前面的操作里删除或移动")))?;
                    if !src.starts_with("<w:p") || src.starts_with("<w:pPr") {
                        return Err(op_err(i, name, format!("块 {block} 不是段落；表格请用 set_cell")));
                    }
                    let target = match style {
                        Some(s) => Some(ctx.resolve_style(s).map_err(wrap)?),
                        None => None,
                    };
                    let spans = markdown::inline(text.trim_end_matches('\n'));
                    let out = ctx.rewrite_paragraph(&src, &spans, target.as_ref()).map_err(wrap)?;
                    changes.push(Change { op: i, kind: name.into(), target: format!("块 {block}"), before: Some(clip(&fragment_text(&src), 300)), after: Some(clip(&markdown::plain(&spans), 300)) });
                    slots[*block].xml = Some(out);
                }
                DocOp::Insert { after, before, at, markdown: md } => {
                    let generated = ctx.markdown(md).map_err(wrap)?;
                    let target = match (after, before, at.as_deref()) {
                        (Some(a), None, None) => {
                            check(i, name, *a)?;
                            slots[*a].after.push(generated);
                            format!("块 {a} 之后")
                        }
                        (None, Some(b), None) => {
                            check(i, name, *b)?;
                            slots[*b].before.push(generated);
                            format!("块 {b} 之前")
                        }
                        (None, None, Some("start")) if n > 0 => {
                            slots[0].before.push(generated);
                            "文档开头".into()
                        }
                        (None, None, Some("end")) | (None, None, None) | (None, None, Some("start")) => {
                            end_inserts.push(generated);
                            "文档末尾".into()
                        }
                        _ => return Err(op_err(i, name, "after、before、at 只能给一个；at 只能是 start 或 end")),
                    };
                    changes.push(Change { op: i, kind: name.into(), target, before: None, after: Some(clip(md, 300)) });
                }
                DocOp::Delete { block, from, to } => {
                    let (a, b) = match (block, from, to) {
                        (Some(b), None, None) => (*b, *b),
                        (None, Some(f), Some(t)) if f <= t => (*f, *t),
                        (None, Some(f), None) => (*f, *f),
                        _ => return Err(op_err(i, name, "给 block，或给 from 和 to（from ≤ to）")),
                    };
                    check(i, name, b)?;
                    for idx in a..=b {
                        let src = slots[idx].xml.clone().ok_or_else(|| op_err(i, name, format!("块 {idx} 已在本批前面的操作里删除或移动")))?;
                        if src.contains("<w:sectPr") {
                            return Err(op_err(i, name, format!("块 {idx} 带分节符（页面设置），删除会改变版面，请改用 replace_block 清空文字")));
                        }
                        let out = if ctx.tracked { ctx.mark_deleted(&src, true).map_err(wrap)? } else { String::new() };
                        changes.push(Change { op: i, kind: name.into(), target: format!("块 {idx}"), before: Some(clip(&fragment_text(&src), 300)), after: None });
                        slots[idx].xml = Some(out);
                    }
                }
                DocOp::Move { block, after, before } => {
                    if ctx.tracked {
                        return Err(op_err(i, name, "修订模式下不支持移动，请用 delete 加 insert"));
                    }
                    check(i, name, *block)?;
                    let src = slots[*block].xml.take().ok_or_else(|| op_err(i, name, format!("块 {block} 已在本批前面的操作里删除或移动")))?;
                    let target = match (after, before) {
                        (Some(a), None) if a != block => {
                            check(i, name, *a)?;
                            slots[*a].after.push(src.clone());
                            format!("移到块 {a} 之后")
                        }
                        (None, Some(b)) if b != block => {
                            check(i, name, *b)?;
                            slots[*b].before.push(src.clone());
                            format!("移到块 {b} 之前")
                        }
                        _ => {
                            slots[*block].xml = Some(src);
                            return Err(op_err(i, name, "after 和 before 只能给一个，且不能是它自己"));
                        }
                    };
                    changes.push(Change { op: i, kind: name.into(), target: format!("块 {block} {target}"), before: Some(clip(&fragment_text(&src), 120)), after: None });
                }
                DocOp::SetStyle { block, style } => {
                    check(i, name, *block)?;
                    let src = slots[*block].xml.clone().ok_or_else(|| op_err(i, name, format!("块 {block} 已在本批前面的操作里删除或移动")))?;
                    if !src.starts_with("<w:p") {
                        return Err(op_err(i, name, format!("块 {block} 不是段落")));
                    }
                    let target = ctx.resolve_style(style).map_err(wrap)?;
                    let out = {
                        let xml = Xml::parse(&src)?;
                        let p = xml.document_element().unwrap();
                        let mut splice = Splice::new();
                        match xml.child(p, "w:pPr") {
                            Some(ppr) => {
                                let restyled = ctx.restyle_ppr(xml.outer(ppr), &target).map_err(wrap)?;
                                splice.replace(xml.node(ppr).start, xml.node(ppr).end, restyled);
                            }
                            None => {
                                let restyled = ctx.restyle_ppr("<w:pPr></w:pPr>", &target).map_err(wrap)?;
                                if xml.node(p).self_closing {
                                    splice.replace(xml.node(p).start, xml.node(p).end, format!("{}{restyled}</w:p>", xml.open_tag_expanded(p)));
                                } else {
                                    splice.insert(xml.node(p).open_end, restyled);
                                }
                            }
                        }
                        splice.apply(&src)?
                    };
                    changes.push(Change { op: i, kind: name.into(), target: format!("块 {block}"), before: None, after: Some(format!("样式 {style}")) });
                    slots[*block].xml = Some(out);
                }
                DocOp::SetCell { block, row, col, text } => {
                    check(i, name, *block)?;
                    let src = slots[*block].xml.clone().ok_or_else(|| op_err(i, name, format!("块 {block} 已在本批前面的操作里删除或移动")))?;
                    let xml = Xml::parse(&src)?;
                    let tbl = xml.document_element().filter(|&t| xml.name(t) == "w:tbl").ok_or_else(|| op_err(i, name, format!("块 {block} 不是表格")))?;
                    let rows = table_rows(&xml, tbl);
                    let cells = rows.get(*row).ok_or_else(|| op_err(i, name, format!("表格只有 {} 行（从 0 开始）", rows.len())))?;
                    let tc = *cells.get(*col).ok_or_else(|| op_err(i, name, format!("第 {row} 行只有 {} 列（从 0 开始）", cells.len())))?;
                    let paras: Vec<usize> = xml.children_named(tc, "w:p").collect();
                    let before_text = cell_text(&xml, tc);
                    let spans = markdown::inline(text);
                    let mut splice = Splice::new();
                    match paras.first() {
                        Some(&p) => {
                            let rewritten = ctx.rewrite_paragraph(xml.outer(p), &spans, None).map_err(wrap)?;
                            splice.replace(xml.node(p).start, xml.node(p).end, rewritten);
                            for &extra in &paras[1..] {
                                let replacement = if ctx.tracked { ctx.mark_deleted(xml.outer(extra), true).map_err(wrap)? } else { String::new() };
                                splice.replace(xml.node(extra).start, xml.node(extra).end, replacement);
                            }
                        }
                        None => {
                            let p = ctx.paragraph("", &spans, RunStyle::default()).map_err(wrap)?;
                            splice.insert(xml.node(tc).close_start, p);
                        }
                    }
                    let out = splice.apply(&src)?;
                    changes.push(Change { op: i, kind: name.into(), target: format!("块 {block} 第 {row} 行第 {col} 列"), before: Some(clip(&before_text, 200)), after: Some(clip(text, 200)) });
                    slots[*block].xml = Some(out);
                }
                DocOp::Comment { block, text } => {
                    check(i, name, *block)?;
                    let src = slots[*block].xml.clone().ok_or_else(|| op_err(i, name, format!("块 {block} 已在本批前面的操作里删除或移动")))?;
                    let out = ctx.add_comment(&src, text).map_err(wrap)?;
                    changes.push(Change { op: i, kind: name.into(), target: format!("块 {block}"), before: None, after: Some(clip(text, 200)) });
                    slots[*block].xml = Some(out);
                }
            }
        }

        // 拼回 document.xml：只替换各块的原文范围
        let new_text = {
            let xml = Xml::parse(&self.text)?;
            let body = body_of(&xml)?;
            let mut splice = Splice::new();
            for (b, slot) in blocks.iter().zip(slots.iter()) {
                let original = &self.text[b.start..b.end];
                let content = slot.xml.as_deref().unwrap_or("");
                if slot.before.is_empty() && slot.after.is_empty() && content == original {
                    continue;
                }
                splice.replace(b.start, b.end, format!("{}{content}{}", slot.before.concat(), slot.after.concat()));
            }
            if !end_inserts.is_empty() {
                let body_node = xml.node(body);
                let last_sect = xml.children(body).last().copied().filter(|&c| xml.name(c) == "w:sectPr");
                let at = last_sect.map(|s| xml.node(s).start).unwrap_or(body_node.close_start);
                if body_node.self_closing {
                    return Err(EngineError::Format("文档正文为空元素，无法插入".into()));
                }
                splice.insert(at, end_inserts.concat());
            }
            splice.apply(&self.text)?
        };
        let new_text = ensure_paragraph_after_table(&new_text)?;
        // 再解析一遍，确保结果是完整的 XML
        parse_blocks(&new_text, ctx.styles).map_err(|e| EngineError::Format(format!("内部错误：修改后的文档无法解析（{e}），文件未改动")))?;
        drop(ctx);
        self.text = new_text;
        self.pkg.write(&self.main, self.text.clone());
        Ok(ApplyReport { changes, revision: self.revision() })
    }

    pub fn save(&self) -> Result<Vec<u8>> {
        self.pkg.save()
    }
}

impl<'d> Ctx<'d> {
    fn add_comment(&mut self, src: &str, text: &str) -> Result<String> {
        let part = match self.pkg.target_of(&self.main, "/comments")? {
            Some(part) if self.pkg.has(&part) => part,
            _ => {
                let part = "word/comments.xml".to_string();
                self.pkg.write(&part, format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:comments xmlns:w="{NS_W}"></w:comments>"#));
                let target = Package::relative(&self.main, &part);
                self.pkg.add_rel(&self.main, REL_COMMENTS, &target, false)?;
                self.pkg.content_type_override(&part, CT_COMMENTS)?;
                part
            }
        };
        self.next_id += 1;
        let id = self.next_id;
        let initials: String = self.author.split_whitespace().filter_map(|w| w.chars().next()).collect();
        let paragraphs: String = text.lines().map(|l| format!(r#"<w:p>{}</w:p>"#, text_run("", l))).collect();
        let comment = format!(
            r#"<w:comment w:id="{id}" w:author="{}" w:date="{}" w:initials="{}">{paragraphs}</w:comment>"#,
            xml::escape_attr(&self.author),
            self.date,
            xml::escape_attr(&initials)
        );
        let existing = self.pkg.read_str(&part)?;
        self.pkg.write(&part, insert_before_close(&existing, "w:comments", &comment)?);

        let xml = Xml::parse(src)?;
        let root = xml.document_element().unwrap_or(xml::ROOT);
        let paras = text_edit::paragraphs(&xml, root, &WORD);
        let (Some(&first), Some(&last)) = (paras.first(), paras.last()) else {
            return Err(EngineError::Invalid("这一块里没有段落，无法加批注".into()));
        };
        let mut splice = Splice::new();
        let start_mark = format!(r#"<w:commentRangeStart w:id="{id}"/>"#);
        let end_mark = format!(r#"<w:commentRangeEnd w:id="{id}"/><w:r><w:commentReference w:id="{id}"/></w:r>"#);
        let fnode = xml.node(first);
        if first == last && fnode.self_closing {
            splice.replace(fnode.start, fnode.end, format!("{}{start_mark}{end_mark}</w:p>", xml.open_tag_expanded(first)));
        } else {
            let start_at = xml.child(first, "w:pPr").map(|p| xml.node(p).end).unwrap_or(fnode.open_end);
            if fnode.self_closing {
                splice.replace(fnode.start, fnode.end, format!("{}{start_mark}</w:p>", xml.open_tag_expanded(first)));
            } else {
                splice.insert(start_at, start_mark);
            }
            let lnode = xml.node(last);
            if lnode.self_closing {
                splice.replace(lnode.start, lnode.end, format!("{}{end_mark}</w:p>", xml.open_tag_expanded(last)));
            } else {
                splice.insert(lnode.close_start, end_mark);
            }
        }
        splice.apply(src)
    }
}

/// 正文可用宽度（twips）：页面宽度减左右边距
fn section_text_width(text: &str) -> u32 {
    let Ok(xml) = Xml::parse(text) else { return 9360 };
    let Ok(body) = body_of(&xml) else { return 9360 };
    let Some(sect) = xml.children(body).iter().rev().copied().find(|&c| xml.name(c) == "w:sectPr") else { return 9360 };
    let num = |node: Option<usize>, key: &str| node.and_then(|n| xml.attr(n, key)).and_then(|v| v.parse::<f64>().ok()).map(|v| v as i64);
    let width = num(xml.child(sect, "w:pgSz"), "w:w").unwrap_or(12240);
    let mar = xml.child(sect, "w:pgMar");
    let usable = width - num(mar, "w:left").unwrap_or(1440) - num(mar, "w:right").unwrap_or(1440);
    usable.clamp(2000, 30000) as u32
}

/// Word 要求表格后面至少有一个段落（文档结尾不能是表格）
fn ensure_paragraph_after_table(text: &str) -> Result<String> {
    let xml = Xml::parse(text)?;
    let body = body_of(&xml)?;
    let content: Vec<usize> = xml.children(body).iter().copied().filter(|&c| xml.name(c) != "w:sectPr").collect();
    match content.last() {
        Some(&last) if xml.name(last) == "w:tbl" => {
            let mut splice = Splice::new();
            splice.insert(xml.node(last).end, "<w:p/>");
            splice.apply(text)
        }
        _ => Ok(text.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    pub fn build_docx(body: &str, styles: &str) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        let files = [
            ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#.to_string()),
            ("_rels/.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_string()),
            ("word/_rels/document.xml.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_string()),
            ("word/document.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1800" w:bottom="1440" w:left="1800"/></w:sectPr></w:body></w:document>"#)),
            ("word/styles.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>{styles}</w:styles>"#)),
            ("word/media/untouched.bin", "这段内容不应被改动".repeat(50)),
        ];
        for (name, data) in files {
            w.start_file(name, o).unwrap();
            w.write_all(data.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    const H1: &str = r#"<w:style w:type="paragraph" w:styleId="1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style>"#;

    fn sample() -> Vec<u8> {
        build_docx(
            r#"<w:p><w:pPr><w:pStyle w:val="1"/></w:pPr><w:r><w:t>项目概述</w:t></w:r></w:p><w:p w:rsidR="001"><w:r><w:rPr><w:b/></w:rPr><w:t>本项目</w:t></w:r><w:r><w:t xml:space="preserve">计划在 2025 年上线。</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tr><w:tc><w:p><w:r><w:t>指标</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>数值</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>用户</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>100</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>结尾段落</w:t></w:r></w:p>"#,
            H1,
        )
    }

    fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        (0..a.len())
            .map(|i| {
                let mut f = a.by_index(i).unwrap();
                let mut v = Vec::new();
                std::io::Read::read_to_end(&mut f, &mut v).unwrap();
                (f.name().to_string(), v)
            })
            .collect()
    }

    fn reopen(bytes: Vec<u8>) -> Vec<Block> {
        Document::open(bytes).unwrap().blocks().unwrap()
    }

    #[test]
    fn reads_blocks() {
        let doc = Document::open(sample()).unwrap();
        let blocks = doc.blocks().unwrap();
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].kind, "heading");
        assert_eq!(blocks[0].level, Some(1));
        assert_eq!(blocks[1].text, "本项目计划在 2025 年上线。");
        assert_eq!(blocks[2].rows.as_ref().unwrap()[1][1], "100");
        let text = format_blocks(&blocks, 100);
        assert!(text.contains("[0] 标题1 | 项目概述"));
        assert!(text.contains("表格 2×2"));
    }

    #[test]
    fn edits_preserve_untouched_bytes() {
        let original = sample();
        let mut doc = Document::open(original.clone()).unwrap();
        let ops: Vec<DocOp> = serde_json::from_str(
            r#"[
                {"op":"replace_text","block":1,"find":"项目计划","replace":"系统预计"},
                {"op":"insert","after":0,"markdown":"新增 **重点** 说明\n\n- 第一条\n- 第二条\n\n1. 步骤一\n2. 步骤二\n\n| A | B |\n|---|---|\n| 1 | 2 |"},
                {"op":"set_cell","block":2,"row":1,"col":1,"text":"250"},
                {"op":"delete","block":3},
                {"op":"set_style","block":1,"style":"heading 2"},
                {"op":"comment","block":0,"text":"请确认标题"}
            ]"#,
        )
        .unwrap();
        let report = doc.apply(&ops, &ApplyOptions::default()).unwrap();
        assert_eq!(report.changes.len(), 6);
        let saved = doc.save().unwrap();
        let blocks = reopen(saved.clone());
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts[0], "项目概述");
        assert_eq!(texts[1], "新增 重点 说明");
        assert_eq!(blocks[2].kind, "list");
        assert!(blocks.iter().any(|b| b.text == "本系统预计在 2025 年上线。" && b.kind == "heading" && b.level == Some(2)));
        assert!(blocks.iter().any(|b| b.rows.as_ref().map(|r| r[1][1] == "250").unwrap_or(false)));
        assert!(!texts.contains(&"结尾段落"));
        // 加粗片段保留
        let doc_xml = String::from_utf8(entries(&saved).into_iter().find(|(n, _)| n == "word/document.xml").unwrap().1).unwrap();
        assert!(doc_xml.contains(r#"<w:r><w:rPr><w:b/></w:rPr><w:t>本</w:t></w:r>"#) || doc_xml.contains("<w:b/></w:rPr><w:t xml:space=\"preserve\">本系统预计</w:t>"));
        // 新建了编号、批注；新增标题样式
        let names: Vec<String> = entries(&saved).into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&"word/numbering.xml".to_string()));
        assert!(names.contains(&"word/comments.xml".to_string()));
        // 没改的条目逐字节一致（含压缩数据）
        let before = raw_entry(&original, "word/media/untouched.bin");
        let after = raw_entry(&saved, "word/media/untouched.bin");
        assert_eq!(before, after);
    }

    fn raw_entry(bytes: &[u8], name: &str) -> (u64, u32) {
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let f = a.by_name(name).unwrap();
        (f.compressed_size(), f.crc32())
    }

    #[test]
    fn tracked_changes() {
        let mut doc = Document::open(sample()).unwrap();
        let ops: Vec<DocOp> = serde_json::from_str(
            r###"[
                {"op":"replace_text","block":1,"find":"2025","replace":"2026"},
                {"op":"replace_block","block":3,"text":"新的 *结尾*"},
                {"op":"insert","at":"end","markdown":"## 附录\n补充内容"},
                {"op":"delete","block":2}
            ]"###,
        )
        .unwrap();
        doc.apply(&ops, &ApplyOptions { track_changes: true, author: "AI".into() }).unwrap();
        let saved = doc.save().unwrap();
        let xml = String::from_utf8(entries(&saved).into_iter().find(|(n, _)| n == "word/document.xml").unwrap().1).unwrap();
        assert!(xml.contains(r#"<w:delText xml:space="preserve">2025</w:delText>"#));
        assert!(xml.contains(r#"<w:t xml:space="preserve">2026</w:t>"#));
        assert!(xml.contains("w:author=\"AI\""));
        assert!(xml.contains("<w:trPr><w:del "));
        let blocks = reopen(saved);
        // 读取时显示修订后的文字（删除的不算）
        assert_eq!(blocks[1].text, "本项目计划在 2026 年上线。");
        assert!(blocks[1].has_revisions);
        assert!(blocks.iter().any(|b| b.text == "附录" && b.kind == "heading"));
        assert!(blocks.iter().any(|b| b.text == "新的 结尾"));
    }

    #[test]
    fn errors_are_guided_and_atomic() {
        let original = sample();
        let mut doc = Document::open(original.clone()).unwrap();
        let ops: Vec<DocOp> = serde_json::from_str(r#"[{"op":"replace_text","block":1,"find":"不存在","replace":"x"}]"#).unwrap();
        let err = doc.apply(&ops, &ApplyOptions::default()).unwrap_err().to_string();
        assert!(err.contains("第 0 个操作（replace_text）"), "{err}");
        assert!(err.contains("本项目计划在 2025 年上线"), "{err}");
        let ops: Vec<DocOp> = serde_json::from_str(r#"[{"op":"delete","block":9}]"#).unwrap();
        assert!(doc.apply(&ops, &ApplyOptions::default()).unwrap_err().to_string().contains("超出范围"));
        assert!(!doc.pkg.is_modified());
    }
}
