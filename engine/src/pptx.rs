//! PowerPoint（.pptx）：读取每页的形状与文字、备注；改文字、改表格单元格、改备注，
//! 复制、删除、调整页面顺序。页码从 1 开始，同一批操作里的页码都指读取时的顺序。

use serde::{Deserialize, Serialize};

use crate::markdown;
use crate::package::Package;
use crate::text_edit::{self, DRAWING};
use crate::xml::{self, Splice, Xml};
use crate::{clip, op_err, revision_of, ApplyReport, Change, EngineError, Result};

const REL_SLIDE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide";
const CT_SLIDE: &str = "application/vnd.openxmlformats-officedocument.presentationml.slide+xml";
const EMU_PER_PT: f64 = 12700.0;

#[derive(Debug, Clone)]
struct SlideRef {
    part: String,
    rid: String,
    /// sldIdLst 里这一页的元素原文
    entry: String,
}

pub struct Deck {
    pkg: Package,
    pres_part: String,
    pres_text: String,
    slides: Vec<SlideRef>,
    /// 页面尺寸（磅）
    pub size: (f64, f64),
}

#[derive(Debug, Clone, Serialize)]
pub struct Shape {
    pub id: String,
    pub name: String,
    /// title / subtitle / body / text / picture / table / chart / shape / footer
    pub kind: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<Vec<String>>>,
    /// 位置与大小（磅）：x, y, 宽, 高
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<[f64; 4]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Slide {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    pub shapes: Vec<Shape>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeckInfo {
    pub slides: usize,
    pub width_pt: f64,
    pub height_pt: f64,
    pub titles: Vec<String>,
    pub revision: String,
}

fn shape_text(xml: &Xml, tx: usize) -> String {
    text_edit::paragraphs(xml, tx, &DRAWING).into_iter().map(|p| text_edit::paragraph_text(xml, p, &DRAWING)).collect::<Vec<_>>().join("\n")
}

fn frame_of(xml: &Xml, node: usize) -> Option<[f64; 4]> {
    let xfrm = xml.descendants(node).into_iter().find(|&d| matches!(xml.name(d), "a:xfrm" | "p:xfrm"))?;
    let off = xml.child(xfrm, "a:off")?;
    let ext = xml.child(xfrm, "a:ext")?;
    let n = |node: usize, k: &str| xml.attr(node, k).and_then(|v| v.parse::<f64>().ok()).map(|v| (v / EMU_PER_PT * 10.0).round() / 10.0);
    Some([n(off, "x")?, n(off, "y")?, n(ext, "cx")?, n(ext, "cy")?])
}

fn table_rows(xml: &Xml, tbl: usize) -> Vec<Vec<usize>> {
    xml.children_named(tbl, "a:tr").map(|tr| xml.children_named(tr, "a:tc").collect()).collect()
}

/// 形状树里的形状（组合形状展开）
fn shapes_of(xml: &Xml, tree: usize, out: &mut Vec<Shape>) {
    for &node in xml.children(tree) {
        let name = xml.name(node);
        if name == "p:grpSp" {
            shapes_of(xml, node, out);
            continue;
        }
        if !matches!(name, "p:sp" | "p:pic" | "p:graphicFrame" | "p:cxnSp") || name == "p:cxnSp" {
            continue;
        }
        let nv = xml.children(node).first().copied();
        let cnv = nv.and_then(|nv| xml.child(nv, "p:cNvPr"));
        let id = cnv.and_then(|c| xml.attr(c, "id")).unwrap_or_default();
        let shape_name = cnv.and_then(|c| xml.attr(c, "name")).unwrap_or_default();
        let ph = nv.and_then(|nv| xml.child(nv, "p:nvPr")).and_then(|p| xml.child(p, "p:ph"));
        let ph_type = ph.map(|ph| xml.attr(ph, "type").unwrap_or_else(|| "body".into()));
        let mut shape = Shape { id, name: shape_name, kind: "shape".into(), text: String::new(), rows: None, frame: frame_of(xml, node) };
        match name {
            "p:sp" => {
                if let Some(tx) = xml.child(node, "p:txBody") {
                    shape.text = shape_text(xml, tx);
                }
                shape.kind = match ph_type.as_deref() {
                    Some("title") | Some("ctrTitle") => "title",
                    Some("subTitle") => "subtitle",
                    Some("body") | Some("obj") => "body",
                    Some("dt") | Some("ftr") | Some("sldNum") | Some("hdr") => "footer",
                    Some(_) => "body",
                    None if !shape.text.is_empty() => "text",
                    None => "shape",
                }
                .into();
            }
            "p:pic" => {
                shape.kind = "picture".into();
                shape.text = cnv.and_then(|c| xml.attr(c, "descr")).unwrap_or_default();
            }
            _ => {
                if let Some(tbl) = xml.find_desc(node, "a:tbl") {
                    shape.kind = "table".into();
                    let rows: Vec<Vec<String>> = table_rows(xml, tbl).into_iter().map(|cells| cells.into_iter().map(|tc| xml.child(tc, "a:txBody").map(|t| shape_text(xml, t)).unwrap_or_default()).collect()).collect();
                    shape.text = rows.iter().map(|r| r.join(" | ")).collect::<Vec<_>>().join("\n");
                    shape.rows = Some(rows);
                } else if xml.descendants(node).iter().any(|&d| xml.name(d) == "c:chart") {
                    shape.kind = "chart".into();
                } else {
                    shape.kind = "other".into();
                }
            }
        }
        out.push(shape);
    }
}

impl Deck {
    pub fn open(bytes: Vec<u8>) -> Result<Self> {
        let pkg = Package::open(bytes)?;
        let pres_part = pkg.main_part()?;
        if !pres_part.starts_with("ppt/") {
            return Err(EngineError::Format("不是 PowerPoint 演示文稿".into()));
        }
        let pres_text = pkg.read_str(&pres_part)?;
        let rels = pkg.rels(&pres_part)?;
        let (slides, size) = {
            let xml = Xml::parse(&pres_text)?;
            let root = xml.document_element().ok_or_else(|| EngineError::Format("presentation.xml 为空".into()))?;
            let mut slides = Vec::new();
            if let Some(list) = xml.child(root, "p:sldIdLst") {
                for s in xml.children_named(list, "p:sldId") {
                    let Some(rid) = xml.attr(s, "r:id") else { continue };
                    let Some(rel) = rels.iter().find(|r| r.id == rid) else { continue };
                    slides.push(SlideRef { part: Package::resolve(&pres_part, &rel.target), rid, entry: xml.outer(s).to_string() });
                }
            }
            let size = xml
                .child(root, "p:sldSz")
                .map(|s| {
                    let n = |k: &str| xml.attr(s, k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) / EMU_PER_PT;
                    (n("cx"), n("cy"))
                })
                .unwrap_or((720.0, 405.0));
            (slides, size)
        };
        Ok(Self { pkg, pres_part, pres_text, slides, size })
    }

    pub fn revision(&self) -> Result<String> {
        let mut all = self.pres_text.clone();
        for s in &self.slides {
            all.push_str(&self.pkg.read_str(&s.part)?);
        }
        Ok(revision_of(&all))
    }

    fn notes_part(&self, slide_part: &str) -> Result<Option<String>> {
        Ok(self.pkg.target_of(slide_part, "/notesSlide")?.filter(|p| self.pkg.has(p)))
    }

    pub fn slides(&self) -> Result<Vec<Slide>> {
        let mut out = Vec::new();
        for (i, s) in self.slides.iter().enumerate() {
            let text = self.pkg.read_str(&s.part)?;
            let xml = Xml::parse(&text)?;
            let root = xml.document_element().ok_or_else(|| EngineError::Format(format!("{} 为空", s.part)))?;
            let mut shapes = Vec::new();
            if let Some(tree) = xml.child(root, "p:cSld").and_then(|c| xml.child(c, "p:spTree")) {
                shapes_of(&xml, tree, &mut shapes);
            }
            let layout = match self.pkg.target_of(&s.part, "/slideLayout")? {
                Some(lp) if self.pkg.has(&lp) => {
                    let lt = self.pkg.read_str(&lp)?;
                    let lx = Xml::parse(&lt)?;
                    lx.document_element().and_then(|r| lx.child(r, "p:cSld")).and_then(|c| lx.attr(c, "name"))
                }
                _ => None,
            };
            let notes = match self.notes_part(&s.part)? {
                Some(np) => {
                    let nt = self.pkg.read_str(&np)?;
                    let nx = Xml::parse(&nt)?;
                    let mut ns = Vec::new();
                    if let Some(tree) = nx.document_element().and_then(|r| nx.child(r, "p:cSld")).and_then(|c| nx.child(c, "p:spTree")) {
                        shapes_of(&nx, tree, &mut ns);
                    }
                    ns.into_iter().find(|s| s.kind == "body").map(|s| s.text).filter(|t| !t.trim().is_empty())
                }
                None => None,
            };
            let hidden = xml.attr(root, "show").as_deref() == Some("0");
            out.push(Slide { index: i + 1, layout, shapes, notes, hidden });
        }
        Ok(out)
    }

    pub fn info(&self) -> Result<DeckInfo> {
        let slides = self.slides()?;
        Ok(DeckInfo {
            slides: slides.len(),
            width_pt: (self.size.0 * 10.0).round() / 10.0,
            height_pt: (self.size.1 * 10.0).round() / 10.0,
            titles: slides.iter().map(|s| s.shapes.iter().find(|x| x.kind == "title").map(|x| clip(&x.text.replace('\n', " "), 60)).unwrap_or_default()).collect(),
            revision: self.revision()?,
        })
    }

    pub fn save(&self) -> Result<Vec<u8>> {
        self.pkg.save()
    }
}

/// 给模型看的页面列表
pub fn format_slides(slides: &[Slide], max_chars: usize) -> String {
    let mut out = String::new();
    for s in slides {
        out.push_str(&format!("── 第 {} 页{}{} ──\n", s.index, s.layout.as_ref().map(|l| format!("（版式：{l}）")).unwrap_or_default(), if s.hidden { " [隐藏]" } else { "" }));
        for sh in &s.shapes {
            let label = match sh.kind.as_str() {
                "title" => "标题",
                "subtitle" => "副标题",
                "body" => "正文",
                "text" => "文本框",
                "picture" => "图片",
                "table" => "表格",
                "chart" => "图表",
                "footer" => "页脚",
                _ => "形状",
            };
            let body = match (&sh.rows, sh.text.is_empty()) {
                (Some(rows), _) => rows.iter().map(|r| format!("| {} |", r.iter().map(|c| clip(&c.replace('\n', " / "), 40)).collect::<Vec<_>>().join(" | "))).collect::<Vec<_>>().join(" "),
                (None, true) => String::new(),
                (None, false) => clip(&sh.text.replace('\n', "↵"), max_chars),
            };
            out.push_str(&format!("  [{}] {label}「{}」{}\n", sh.id, sh.name, if body.is_empty() { String::new() } else { format!(" | {body}") }));
        }
        if let Some(n) = &s.notes {
            out.push_str(&format!("  备注 | {}\n", clip(&n.replace('\n', "↵"), max_chars)));
        }
    }
    out
}

// ───────────────────────── 编辑 ─────────────────────────

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SlideOp {
    /// 查找替换；不给 slide 时在所有页查找
    ReplaceText {
        #[serde(default)]
        slide: Option<usize>,
        find: String,
        replace: String,
        #[serde(default = "default_true")]
        all: bool,
    },
    /// 重写形状里的文字（每行一段，行首每两个空格缩进一级，支持 **加粗** *斜体*）。
    /// shape 可以是形状 id、名称，或 title / subtitle / body
    SetText { slide: usize, shape: String, text: String },
    SetCell { slide: usize, shape: String, row: usize, col: usize, text: String },
    SetNotes { slide: usize, text: String },
    /// 复制一页，默认放在原页后面
    DuplicateSlide {
        slide: usize,
        #[serde(default)]
        after: Option<usize>,
    },
    DeleteSlide { slide: usize },
    /// 移到第 to 位（按当前顺序）
    MoveSlide { slide: usize, to: usize },
}

impl SlideOp {
    fn name(&self) -> &'static str {
        match self {
            Self::ReplaceText { .. } => "replace_text",
            Self::SetText { .. } => "set_text",
            Self::SetCell { .. } => "set_cell",
            Self::SetNotes { .. } => "set_notes",
            Self::DuplicateSlide { .. } => "duplicate_slide",
            Self::DeleteSlide { .. } => "delete_slide",
            Self::MoveSlide { .. } => "move_slide",
        }
    }
}

fn find_shape(xml: &Xml, root: usize, key: &str) -> Option<usize> {
    let key_l = key.trim().to_lowercase();
    let candidates: Vec<usize> = xml.descendants(root).into_iter().filter(|&d| matches!(xml.name(d), "p:sp" | "p:graphicFrame" | "p:pic")).collect();
    let info = |node: usize| {
        let nv = xml.children(node).first().copied();
        let cnv = nv.and_then(|nv| xml.child(nv, "p:cNvPr"));
        let ph = nv.and_then(|nv| xml.child(nv, "p:nvPr")).and_then(|p| xml.child(p, "p:ph"));
        (cnv.and_then(|c| xml.attr(c, "id")).unwrap_or_default(), cnv.and_then(|c| xml.attr(c, "name")).unwrap_or_default(), ph.map(|ph| xml.attr(ph, "type").unwrap_or_else(|| "body".into())))
    };
    if let Some(&n) = candidates.iter().find(|&&n| info(n).0 == key.trim()) {
        return Some(n);
    }
    if let Some(&n) = candidates.iter().find(|&&n| info(n).1.to_lowercase() == key_l) {
        return Some(n);
    }
    let wanted: &[&str] = match key_l.as_str() {
        "title" | "标题" => &["title", "ctrTitle"],
        "subtitle" | "副标题" => &["subTitle"],
        "body" | "正文" | "content" => &["body", "obj"],
        _ => return None,
    };
    candidates.into_iter().find(|&n| info(n).2.map(|t| wanted.contains(&t.as_str())).unwrap_or(false))
}

fn describe_shapes(xml: &Xml, root: usize) -> String {
    let mut shapes = Vec::new();
    if let Some(tree) = xml.child(root, "p:cSld").and_then(|c| xml.child(c, "p:spTree")) {
        shapes_of(xml, tree, &mut shapes);
    }
    shapes.iter().map(|s| format!("[{}]「{}」{}", s.id, s.name, s.kind)).collect::<Vec<_>>().join("、")
}

fn rpr_with(base: &str, bold: bool, italic: bool) -> String {
    let base = if base.is_empty() { r#"<a:rPr lang="zh-CN" dirty="0"/>"# } else { base };
    let open_end = base.find('>').map(|i| i + 1).unwrap_or(base.len());
    let mut open = base[..open_end].to_string();
    let rest = &base[open_end..];
    if bold {
        open = xml::set_attr(&open, "b", "1");
    }
    if italic {
        open = xml::set_attr(&open, "i", "1");
    }
    format!("{open}{rest}")
}

/// 用新文字重写一个文本框（a:txBody / p:txBody），保留 bodyPr、lstStyle 和首段格式
fn rewrite_text_body(xml: &Xml, body: usize, text: &str, strip_bullets: bool) -> String {
    let first_p = xml.children_named(body, "a:p").next();
    let ppr = first_p.and_then(|p| xml.child(p, "a:pPr")).map(|p| xml.outer(p).to_string()).unwrap_or_default();
    let rpr = first_p
        .and_then(|p| xml.descendants(p).into_iter().find(|&d| xml.name(d) == "a:rPr"))
        .map(|r| xml.outer(r).to_string())
        .unwrap_or_default();
    let end_rpr = first_p.and_then(|p| xml.child(p, "a:endParaRPr")).map(|e| xml.outer(e).to_string()).unwrap_or_default();
    let keep: String = xml.children(body).iter().filter(|&&c| matches!(xml.name(c), "a:bodyPr" | "a:lstStyle")).map(|&c| xml.outer(c)).collect();
    let mut paras = String::new();
    for line in text.split('\n') {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let level = (indent / 2).min(8);
        let mut content = line.trim();
        if strip_bullets {
            for marker in ["- ", "* ", "• "] {
                if let Some(rest) = content.strip_prefix(marker) {
                    content = rest;
                }
            }
        }
        let p_ppr = if level > 0 {
            if ppr.is_empty() { format!(r#"<a:pPr lvl="{level}"/>"#) } else { xml::set_attr(&ppr[..ppr.find('>').unwrap() + 1], "lvl", &level.to_string()) + &ppr[ppr.find('>').unwrap() + 1..] }
        } else {
            ppr.clone()
        };
        let runs: String = markdown::inline(content)
            .into_iter()
            .filter(|s| !s.text.is_empty())
            .map(|s| format!("<a:r>{}<a:t>{}</a:t></a:r>", rpr_with(&rpr, s.bold, s.italic), xml::escape(&s.text)))
            .collect();
        paras.push_str(&format!("<a:p>{p_ppr}{runs}{end_rpr}</a:p>"));
    }
    format!("{}{keep}{paras}</{}>", xml.open_tag_expanded(body), xml.name(body))
}

struct Entry {
    orig: Option<usize>,
    slide: SlideRef,
}

impl Deck {
    fn slide_xml(&self, part: &str) -> Result<String> {
        self.pkg.read_str(part)
    }

    pub fn apply(&mut self, ops: &[SlideOp]) -> Result<ApplyReport> {
        if ops.is_empty() {
            return Err(EngineError::Invalid("没有要执行的操作".into()));
        }
        let n = self.slides.len();
        let mut order: Vec<Entry> = self.slides.iter().cloned().enumerate().map(|(i, s)| Entry { orig: Some(i), slide: s }).collect();
        let mut deleted: Vec<SlideRef> = Vec::new();
        let mut changes = Vec::new();
        let mut structure = false;
        let mut next_sld_id = {
            let xml = Xml::parse(&self.pres_text)?;
            let root = xml.document_element().unwrap();
            xml.child(root, "p:sldIdLst").map(|l| xml.children_named(l, "p:sldId").filter_map(|s| xml.attr(s, "id")?.parse::<u32>().ok()).max().unwrap_or(255)).unwrap_or(255)
        };

        for (i, op) in ops.iter().enumerate() {
            let name = op.name();
            let slide_part = |no: usize, order: &Vec<Entry>| -> Result<String> {
                if no == 0 || no > n {
                    return Err(op_err(i, name, format!("页码 {no} 超出范围（共 {n} 页，从 1 开始）")));
                }
                order.iter().find(|e| e.orig == Some(no - 1)).map(|e| e.slide.part.clone()).ok_or_else(|| op_err(i, name, format!("第 {no} 页已在本批前面的操作里删除")))
            };
            match op {
                SlideOp::ReplaceText { slide, find, replace, all } => {
                    if find.is_empty() {
                        return Err(op_err(i, name, "find 不能为空"));
                    }
                    let targets: Vec<usize> = match slide {
                        Some(s) => vec![*s],
                        None => (1..=n).filter(|no| order.iter().any(|e| e.orig == Some(no - 1))).collect(),
                    };
                    let mut total = 0;
                    for no in targets {
                        if total > 0 && !all {
                            break;
                        }
                        let part = slide_part(no, &order)?;
                        let text = self.slide_xml(&part)?;
                        let (out, count) = text_edit::replace_plain(&text, &DRAWING, find, replace, *all)?;
                        if count > 0 {
                            total += count;
                            self.pkg.write(&part, out);
                            changes.push(Change { op: i, kind: name.into(), target: format!("第 {no} 页"), before: Some(find.clone()), after: Some(format!("{replace}（{count} 处）")) });
                        }
                    }
                    if total == 0 {
                        return Err(op_err(i, name, format!("没有找到“{find}”，find 要与原文逐字一致；可以先读取页面确认原文")));
                    }
                }
                SlideOp::SetText { slide, shape, text } => {
                    let part = slide_part(*slide, &order)?;
                    let src = self.slide_xml(&part)?;
                    let (out, before) = {
                        let xml = Xml::parse(&src)?;
                        let root = xml.document_element().unwrap();
                        let node = find_shape(&xml, root, shape).ok_or_else(|| op_err(i, name, format!("第 {slide} 页没有形状“{shape}”。现有：{}", describe_shapes(&xml, root))))?;
                        let body = xml.child(node, "p:txBody").ok_or_else(|| op_err(i, name, format!("形状“{shape}”不能放文字（可能是图片、表格或图表）")))?;
                        let is_body_ph = xml.children(node).first().and_then(|&nv| xml.child(nv, "p:nvPr")).and_then(|p| xml.child(p, "p:ph")).map(|ph| matches!(xml.attr(ph, "type").as_deref(), None | Some("body") | Some("obj"))).unwrap_or(false);
                        let before = shape_text(&xml, body);
                        let mut splice = Splice::new();
                        splice.replace(xml.node(body).start, xml.node(body).end, rewrite_text_body(&xml, body, text, is_body_ph));
                        (splice.apply(&src)?, before)
                    };
                    self.pkg.write(&part, out);
                    changes.push(Change { op: i, kind: name.into(), target: format!("第 {slide} 页「{shape}」"), before: Some(clip(&before, 300)), after: Some(clip(text, 300)) });
                }
                SlideOp::SetCell { slide, shape, row, col, text } => {
                    let part = slide_part(*slide, &order)?;
                    let src = self.slide_xml(&part)?;
                    let (out, before) = {
                        let xml = Xml::parse(&src)?;
                        let root = xml.document_element().unwrap();
                        let node = find_shape(&xml, root, shape).ok_or_else(|| op_err(i, name, format!("第 {slide} 页没有形状“{shape}”。现有：{}", describe_shapes(&xml, root))))?;
                        let tbl = xml.find_desc(node, "a:tbl").ok_or_else(|| op_err(i, name, format!("形状“{shape}”不是表格")))?;
                        let rows = table_rows(&xml, tbl);
                        let cells = rows.get(*row).ok_or_else(|| op_err(i, name, format!("表格只有 {} 行（从 0 开始）", rows.len())))?;
                        let tc = *cells.get(*col).ok_or_else(|| op_err(i, name, format!("第 {row} 行只有 {} 列（从 0 开始）", cells.len())))?;
                        let mut splice = Splice::new();
                        let before = match xml.child(tc, "a:txBody") {
                            Some(body) => {
                                let before = shape_text(&xml, body);
                                splice.replace(xml.node(body).start, xml.node(body).end, rewrite_text_body(&xml, body, text, false));
                                before
                            }
                            None => {
                                let runs: String = text.split('\n').map(|l| format!(r#"<a:p><a:r><a:rPr lang="zh-CN" dirty="0"/><a:t>{}</a:t></a:r></a:p>"#, xml::escape(l))).collect();
                                splice.insert(xml.node(tc).open_end, format!("<a:txBody><a:bodyPr/><a:lstStyle/>{runs}</a:txBody>"));
                                String::new()
                            }
                        };
                        (splice.apply(&src)?, before)
                    };
                    self.pkg.write(&part, out);
                    changes.push(Change { op: i, kind: name.into(), target: format!("第 {slide} 页「{shape}」第 {row} 行第 {col} 列"), before: Some(clip(&before, 200)), after: Some(clip(text, 200)) });
                }
                SlideOp::SetNotes { slide, text } => {
                    let part = slide_part(*slide, &order)?;
                    let notes = self.notes_part(&part)?.ok_or_else(|| op_err(i, name, format!("第 {slide} 页没有备注页，暂不支持新建备注")))?;
                    let src = self.pkg.read_str(&notes)?;
                    let (out, before) = {
                        let xml = Xml::parse(&src)?;
                        let root = xml.document_element().unwrap();
                        let node = find_shape(&xml, root, "body").ok_or_else(|| op_err(i, name, "备注页里没有正文占位符"))?;
                        let body = xml.child(node, "p:txBody").ok_or_else(|| op_err(i, name, "备注占位符没有文字区域"))?;
                        let before = shape_text(&xml, body);
                        let mut splice = Splice::new();
                        splice.replace(xml.node(body).start, xml.node(body).end, rewrite_text_body(&xml, body, text, false));
                        (splice.apply(&src)?, before)
                    };
                    self.pkg.write(&notes, out);
                    changes.push(Change { op: i, kind: name.into(), target: format!("第 {slide} 页备注"), before: Some(clip(&before, 200)), after: Some(clip(text, 200)) });
                }
                SlideOp::DuplicateSlide { slide, after } => {
                    let part = slide_part(*slide, &order)?;
                    let rels = self.pkg.rels(&part)?;
                    if rels.iter().any(|r| r.kind.ends_with("/chart") || r.kind.contains("/diagram")) {
                        return Err(op_err(i, name, format!("第 {slide} 页含图表或 SmartArt，暂不支持复制")));
                    }
                    let new_part = self.pkg.unused_name("ppt/slides/slide", "xml");
                    self.pkg.write(&new_part, self.pkg.read(&part)?);
                    self.pkg.content_type_override(&new_part, CT_SLIDE)?;
                    // 关系原样复制（同一目录，相对路径不变），去掉备注页：备注页只属于原页
                    let rels_path = Package::rels_path(&part);
                    if self.pkg.has(&rels_path) {
                        let text = self.pkg.read_str(&rels_path)?;
                        let copied = {
                            let xml = Xml::parse(&text)?;
                            let root = xml.document_element().unwrap();
                            let mut splice = Splice::new();
                            for r in xml.children_named(root, "Relationship") {
                                if xml.attr(r, "Type").map(|t| t.ends_with("/notesSlide")).unwrap_or(false) {
                                    splice.replace(xml.node(r).start, xml.node(r).end, "");
                                }
                            }
                            splice.apply(&text)?
                        };
                        self.pkg.write(&Package::rels_path(&new_part), copied);
                    }
                    let rid = self.pkg.add_rel(&self.pres_part.clone(), REL_SLIDE, &Package::relative(&self.pres_part, &new_part), false)?;
                    next_sld_id += 1;
                    let entry = Entry { orig: None, slide: SlideRef { part: new_part, rid: rid.clone(), entry: format!(r#"<p:sldId id="{next_sld_id}" r:id="{rid}"/>"#) } };
                    let anchor = after.unwrap_or(*slide);
                    slide_part(anchor, &order)?;
                    let pos = order.iter().position(|e| e.orig == Some(anchor - 1)).unwrap();
                    order.insert(pos + 1, entry);
                    structure = true;
                    changes.push(Change { op: i, kind: name.into(), target: format!("第 {slide} 页"), before: None, after: Some(format!("副本放在原第 {anchor} 页之后")) });
                }
                SlideOp::DeleteSlide { slide } => {
                    slide_part(*slide, &order)?;
                    if order.len() <= 1 {
                        return Err(op_err(i, name, "至少要保留一页"));
                    }
                    let pos = order.iter().position(|e| e.orig == Some(slide - 1)).unwrap();
                    let removed = order.remove(pos);
                    deleted.push(removed.slide);
                    structure = true;
                    changes.push(Change { op: i, kind: name.into(), target: format!("第 {slide} 页"), before: None, after: None });
                }
                SlideOp::MoveSlide { slide, to } => {
                    slide_part(*slide, &order)?;
                    let pos = order.iter().position(|e| e.orig == Some(slide - 1)).unwrap();
                    let entry = order.remove(pos);
                    let at = to.saturating_sub(1).min(order.len());
                    order.insert(at, entry);
                    structure = true;
                    changes.push(Change { op: i, kind: name.into(), target: format!("第 {slide} 页"), before: None, after: Some(format!("移到第 {} 位", at + 1)) });
                }
            }
        }

        if structure {
            for s in &deleted {
                if let Some(notes) = self.notes_part(&s.part)? {
                    self.pkg.remove(&notes);
                    self.pkg.remove(&Package::rels_path(&notes));
                    self.pkg.remove_content_type_override(&notes)?;
                }
                self.pkg.remove(&s.part);
                self.pkg.remove(&Package::rels_path(&s.part));
                self.pkg.remove_content_type_override(&s.part)?;
                self.pkg.remove_rel(&self.pres_part.clone(), &s.rid)?;
            }
            let updated = {
                let xml = Xml::parse(&self.pres_text)?;
                let root = xml.document_element().unwrap();
                let list = xml.child(root, "p:sldIdLst").ok_or_else(|| EngineError::Format("presentation.xml 缺少 sldIdLst".into()))?;
                let n = xml.node(list);
                let body: String = order.iter().map(|e| e.slide.entry.as_str()).collect();
                let mut splice = Splice::new();
                splice.replace(n.start, n.end, format!("{}{body}</p:sldIdLst>", xml.open_tag_expanded(list)));
                splice.apply(&self.pres_text)?
            };
            self.pres_text = updated;
            self.pkg.write(&self.pres_part.clone(), self.pres_text.clone());
            self.slides = order.into_iter().map(|e| e.slide).collect();
        }
        Ok(ApplyReport { changes, revision: self.revision()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

    fn sp(id: u32, name: &str, ph: &str, paras: &[&str]) -> String {
        let ph = if ph.is_empty() { String::new() } else { format!(r#"<p:ph type="{ph}"/>"#) };
        let ps: String = paras.iter().map(|t| format!(r#"<a:p><a:r><a:rPr lang="zh-CN" sz="2400"/><a:t>{t}</a:t></a:r></a:p>"#)).collect();
        format!(r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="{name}"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="12700" y="25400"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/>{ps}</p:txBody></p:sp>"#)
    }

    fn slide(shapes: &str) -> String {
        format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sld {NS}><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{shapes}</p:spTree></p:cSld></p:sld>"#)
    }

    fn sample() -> Vec<u8> {
        let table = r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="5" name="Table 1"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="100" cy="100"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tr h="10"><a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>Q1</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:bodyPr/><a:p><a:r><a:t>100</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#;
        let files: Vec<(&str, String)> = vec![
            ("[Content_Types].xml", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/notesSlides/notesSlide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml"/></Types>"#.into()),
            ("_rels/.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#.into()),
            ("ppt/presentation.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:presentation {NS}><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#)),
            ("ppt/_rels/presentation.xml.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>"#.into()),
            ("ppt/slides/slide1.xml", slide(&format!("{}{}", sp(2, "Title 1", "title", &["年度总结"]), sp(3, "Content 2", "body", &["收入增长", "成本下降"])))),
            ("ppt/slides/_rels/slide1.xml.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide1.xml"/></Relationships>"#.into()),
            ("ppt/slides/slide2.xml", slide(&format!("{}{table}", sp(2, "Title 1", "title", &["数据"])))),
            ("ppt/slideLayouts/slideLayout1.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:sldLayout {NS}><p:cSld name="标题和内容"><p:spTree/></p:cSld></p:sldLayout>"#)),
            ("ppt/notesSlides/notesSlide1.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><p:notes {NS}><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:notes>"#, sp(2, "Notes", "body", &["讲稿"]))),
        ];
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in files {
            w.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(data.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_slides() {
        let deck = Deck::open(sample()).unwrap();
        let slides = deck.slides().unwrap();
        assert_eq!(slides.len(), 2);
        assert_eq!(slides[0].layout.as_deref(), Some("标题和内容"));
        assert_eq!(slides[0].shapes[1].text, "收入增长\n成本下降");
        assert_eq!(slides[0].notes.as_deref(), Some("讲稿"));
        assert_eq!(slides[1].shapes[1].rows.as_ref().unwrap()[0][1], "100");
        assert_eq!(slides[0].shapes[0].frame, Some([1.0, 2.0, 100.0, 50.0]));
        let text = format_slides(&slides, 100);
        assert!(text.contains("[3] 正文「Content 2」 | 收入增长↵成本下降"), "{text}");
        assert_eq!(deck.info().unwrap().width_pt, 960.0);
    }

    #[test]
    fn edits_text_and_structure() {
        let mut deck = Deck::open(sample()).unwrap();
        let ops: Vec<SlideOp> = serde_json::from_str(
            r#"[
                {"op":"replace_text","find":"年度","replace":"2026 年度"},
                {"op":"set_text","slide":1,"shape":"body","text":"- **收入** 增长 30%\n  - 海外贡献过半\n- 成本下降"},
                {"op":"set_cell","slide":2,"shape":"Table 1","row":0,"col":1,"text":"120"},
                {"op":"set_notes","slide":1,"text":"先讲收入"},
                {"op":"duplicate_slide","slide":1},
                {"op":"move_slide","slide":2,"to":1}
            ]"#,
        )
        .unwrap();
        let report = deck.apply(&ops).unwrap();
        assert_eq!(report.changes.len(), 6);
        let deck2 = Deck::open(deck.save().unwrap()).unwrap();
        let slides = deck2.slides().unwrap();
        assert_eq!(slides.len(), 3);
        // 原第 2 页移到第 1 位，后面是原第 1 页和它的副本
        assert_eq!(slides[0].shapes[1].rows.as_ref().unwrap()[0][1], "120");
        assert_eq!(slides[1].shapes[0].text, "2026 年度总结");
        assert_eq!(slides[1].shapes[1].text, "收入 增长 30%\n海外贡献过半\n成本下降");
        assert_eq!(slides[1].notes.as_deref(), Some("先讲收入"));
        assert_eq!(slides[2].shapes[0].text, "2026 年度总结");
        assert_eq!(slides[2].notes, None);
        assert_eq!(slides[2].layout.as_deref(), Some("标题和内容"));

        let mut deck3 = Deck::open(deck2.save().unwrap()).unwrap();
        deck3.apply(&serde_json::from_str::<Vec<SlideOp>>(r#"[{"op":"delete_slide","slide":2}]"#).unwrap()).unwrap();
        let saved = deck3.save().unwrap();
        let slides = Deck::open(saved.clone()).unwrap().slides().unwrap();
        assert_eq!(slides.len(), 2);
        let names = crate::package::Package::open(saved).unwrap().names();
        assert!(!names.iter().any(|n| n == "ppt/notesSlides/notesSlide1.xml"));
    }

    #[test]
    fn guided_errors() {
        let mut deck = Deck::open(sample()).unwrap();
        let err = deck.apply(&serde_json::from_str::<Vec<SlideOp>>(r#"[{"op":"set_text","slide":1,"shape":"不存在","text":"x"}]"#).unwrap()).unwrap_err().to_string();
        assert!(err.contains("[2]「Title 1」title"), "{err}");
        let err = deck.apply(&serde_json::from_str::<Vec<SlideOp>>(r#"[{"op":"delete_slide","slide":5}]"#).unwrap()).unwrap_err().to_string();
        assert!(err.contains("共 2 页"), "{err}");
    }
}
