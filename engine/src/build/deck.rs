//! 从页面规格生成 PPT，并在不渲染的情况下检查版面。
//!
//! 一套 PPT = 风格表（尺寸、配色、字体、字号）+ 大纲（每页标题与要点）+ 每页一个页面规格。
//! 页面规格可以用版式（cover / bullets / two-column / metrics / table / chart / image-left …）
//! 只填内容，也可以用 elements 自由摆放元素（坐标单位为磅，16:9 页面为 960×540）。
//! 版式先算出每个元素的位置，检查和生成都基于同一份元素列表，所以检查结果与生成结果一致。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::markdown;
use crate::xml::{escape, escape_attr};
use crate::{EngineError, Result};

// ───────────────────────── 规格 ─────────────────────────

fn default_size() -> String {
    "16:9".into()
}
fn default_font() -> String {
    "Microsoft YaHei".into()
}
fn default_title_size() -> f64 {
    32.0
}
fn default_body_size() -> f64 {
    18.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Palette {
    #[serde(default = "Palette::bg")]
    pub background: String,
    /// 卡片、表格隔行等浅色面
    #[serde(default = "Palette::surface")]
    pub surface: String,
    #[serde(default = "Palette::text")]
    pub text: String,
    #[serde(default = "Palette::muted")]
    pub muted: String,
    #[serde(default = "Palette::accent")]
    pub accent: String,
    /// 图表系列等需要的更多颜色
    #[serde(default)]
    pub extra: Vec<String>,
}

impl Palette {
    fn bg() -> String {
        "#FFFFFF".into()
    }
    fn surface() -> String {
        "#F1F5F9".into()
    }
    fn text() -> String {
        "#1F2937".into()
    }
    fn muted() -> String {
        "#64748B".into()
    }
    fn accent() -> String {
        "#2563EB".into()
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self { background: Self::bg(), surface: Self::surface(), text: Self::text(), muted: Self::muted(), accent: Self::accent(), extra: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fonts {
    #[serde(default = "default_font")]
    pub heading: String,
    #[serde(default = "default_font")]
    pub body: String,
}

impl Default for Fonts {
    fn default() -> Self {
        Self { heading: default_font(), body: default_font() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Style {
    /// 16:9 或 4:3
    #[serde(default = "default_size")]
    pub size: String,
    #[serde(default)]
    pub palette: Palette,
    #[serde(default)]
    pub fonts: Fonts,
    #[serde(default = "default_title_size")]
    pub title_size: f64,
    #[serde(default = "default_body_size")]
    pub body_size: f64,
}

impl Default for Style {
    fn default() -> Self {
        Self { size: default_size(), palette: Palette::default(), fonts: Fonts::default(), title_size: default_title_size(), body_size: default_body_size() }
    }
}

impl Style {
    pub fn page_size(&self) -> (f64, f64) {
        if self.size.trim() == "4:3" { (720.0, 540.0) } else { (960.0, 540.0) }
    }

    /// 配色里的全部颜色（检查用）
    pub fn colors(&self) -> Vec<String> {
        let p = &self.palette;
        let mut all = vec![p.background.clone(), p.surface.clone(), p.text.clone(), p.muted.clone(), p.accent.clone(), "#FFFFFF".into(), "#000000".into()];
        all.extend(p.extra.iter().cloned());
        all.iter().filter_map(|c| hex(c)).collect()
    }

    pub fn series_colors(&self) -> Vec<String> {
        let mut out = vec![self.palette.accent.clone()];
        out.extend(self.palette.extra.iter().cloned());
        out.extend(["#F59E0B", "#10B981", "#8B5CF6", "#EF4444", "#06B6D4"].map(String::from));
        out.into_iter().filter_map(|c| hex(&c)).collect()
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        if !matches!(self.size.trim(), "16:9" | "4:3") {
            return Err("size 只能是 16:9 或 4:3".into());
        }
        let p = &self.palette;
        for (name, c) in [("background", &p.background), ("surface", &p.surface), ("text", &p.text), ("muted", &p.muted), ("accent", &p.accent)] {
            if hex(c).is_none() {
                return Err(format!("palette.{name}“{c}”不是 #RRGGBB 颜色"));
            }
        }
        for c in &p.extra {
            if hex(c).is_none() {
                return Err(format!("palette.extra 里的“{c}”不是 #RRGGBB 颜色"));
            }
        }
        if !(20.0..=60.0).contains(&self.title_size) || !(10.0..=32.0).contains(&self.body_size) {
            return Err("title_size 要在 20–60，body_size 要在 10–32".into());
        }
        Ok(())
    }
}

/// 归一化成大写 6 位十六进制
pub fn hex(c: &str) -> Option<String> {
    let h = c.trim().trim_start_matches('#');
    let h = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect::<String>() } else { h.to_string() };
    (h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{}", h.to_uppercase()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlinePage {
    pub title: String,
    /// cover / section / content / data / closing（提示用）
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub points: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outline {
    #[serde(default)]
    pub title: String,
    pub pages: Vec<OutlinePage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Bullet {
    Text(String),
    Item {
        text: String,
        #[serde(default)]
        level: u8,
    },
}

impl Bullet {
    fn parts(&self) -> (&str, u8) {
        match self {
            Bullet::Text(t) => {
                let level = ((t.len() - t.trim_start_matches(' ').len()) / 2) as u8;
                (t.trim(), level.min(4))
            }
            Bullet::Item { text, level } => (text.as_str(), (*level).min(4)),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Column {
    #[serde(default)]
    pub heading: Option<String>,
    #[serde(default)]
    pub bullets: Vec<Bullet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSpec {
    pub rows: Vec<Vec<serde_json::Value>>,
    #[serde(default = "yes")]
    pub header: bool,
    #[serde(default)]
    pub size: Option<f64>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub name: String,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartSpec {
    /// column / bar / line / pie / doughnut
    #[serde(rename = "type", default = "column")]
    pub kind: String,
    pub categories: Vec<String>,
    pub series: Vec<Series>,
    #[serde(default)]
    pub title: Option<String>,
    /// 显示数值标签
    #[serde(default)]
    pub labels: bool,
    #[serde(default)]
    pub number_format: Option<String>,
}

fn column() -> String {
    "column".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSpec {
    pub path: String,
    #[serde(default)]
    pub caption: Option<String>,
    /// cover（裁切铺满）/ contain（完整显示）
    #[serde(default)]
    pub fit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteSpec {
    pub text: String,
    #[serde(default)]
    pub by: Option<String>,
}

/// 一段文字
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Para {
    pub text: String,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub bullet: bool,
    #[serde(default)]
    pub level: u8,
    #[serde(default)]
    pub align: Option<String>,
    /// 段前间距（磅）
    #[serde(default)]
    pub space_before: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Element {
    Text {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        paragraphs: Vec<Para>,
        #[serde(default)]
        size: Option<f64>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        italic: bool,
        #[serde(default)]
        align: Option<String>,
        /// top / middle / bottom
        #[serde(default)]
        valign: Option<String>,
        #[serde(default)]
        fill: Option<String>,
        #[serde(default)]
        font: Option<String>,
        /// 内边距（磅）
        #[serde(default)]
        padding: Option<f64>,
        /// 内部用：标题框
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        title: bool,
    },
    Shape {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        /// rect / round / ellipse / line
        #[serde(default)]
        shape: Option<String>,
        #[serde(default)]
        fill: Option<String>,
        #[serde(default)]
        line: Option<String>,
        #[serde(default)]
        line_width: Option<f64>,
    },
    Image {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        path: String,
        #[serde(default)]
        fit: Option<String>,
    },
    Table {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        rows: Vec<Vec<serde_json::Value>>,
        #[serde(default = "yes")]
        header: bool,
        #[serde(default)]
        size: Option<f64>,
    },
    Chart {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        /// 图表内容：{"type":"bar","categories":[…],"series":[…]}
        chart: ChartSpec,
    },
}

impl Element {
    pub fn frame(&self) -> (f64, f64, f64, f64) {
        match self {
            Element::Text { x, y, w, h, .. } | Element::Shape { x, y, w, h, .. } | Element::Image { x, y, w, h, .. } | Element::Table { x, y, w, h, .. } | Element::Chart { x, y, w, h, .. } => (*x, *y, *w, *h),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Element::Text { .. } => "文字",
            Element::Shape { .. } => "形状",
            Element::Image { .. } => "图片",
            Element::Table { .. } => "表格",
            Element::Chart { .. } => "图表",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Page {
    /// cover / section / bullets / two-column / metrics / table / chart / image-left / image-right / quote / closing / blank
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub bullets: Vec<Bullet>,
    #[serde(default)]
    pub left: Option<Column>,
    #[serde(default)]
    pub right: Option<Column>,
    #[serde(default)]
    pub metrics: Vec<Metric>,
    #[serde(default)]
    pub table: Option<TableSpec>,
    #[serde(default)]
    pub chart: Option<ChartSpec>,
    #[serde(default)]
    pub image: Option<ImageSpec>,
    #[serde(default)]
    pub quote: Option<QuoteSpec>,
    #[serde(default)]
    pub elements: Vec<Element>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
}

pub const LAYOUTS: &[&str] = &["cover", "section", "bullets", "two-column", "metrics", "table", "chart", "image-left", "image-right", "quote", "closing", "blank"];

// ───────────────────────── 版式 ─────────────────────────

const M: f64 = 48.0;

fn text_el(x: f64, y: f64, w: f64, h: f64, paragraphs: Vec<Para>) -> Element {
    Element::Text { x, y, w, h, text: None, paragraphs, size: None, color: None, bold: false, italic: false, align: None, valign: None, fill: None, font: None, padding: None, title: false }
}

fn para(text: &str, size: f64, color: &str, bold: bool) -> Para {
    Para { text: text.to_string(), size: Some(size), color: Some(color.to_string()), bold, ..Default::default() }
}

fn bullet_paras(bullets: &[Bullet], style: &Style, size: f64) -> Vec<Para> {
    bullets
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let (text, level) = b.parts();
            let size = if level > 0 { (size - 2.0).max(12.0) } else { size };
            Para { text: text.into(), size: Some(size), color: Some(if level > 0 { style.palette.muted.clone() } else { style.palette.text.clone() }), bullet: true, level, space_before: Some(if i == 0 { 0.0 } else { size * 0.5 }), ..Default::default() }
        })
        .collect()
}

fn title_el(page_w: f64, title: &str, style: &Style) -> Vec<Element> {
    let mut t = text_el(M, 32.0, page_w - 2.0 * M, 64.0, vec![Para { text: title.into(), size: Some(style.title_size), color: Some(style.palette.text.clone()), bold: true, ..Default::default() }]);
    if let Element::Text { valign, font, title: is_title, .. } = &mut t {
        *valign = Some("bottom".into());
        *font = Some(style.fonts.heading.clone());
        *is_title = true;
    }
    vec![t, Element::Shape { x: M, y: 104.0, w: 56.0, h: 4.0, shape: Some("rect".into()), fill: Some(style.palette.accent.clone()), line: None, line_width: None }]
}

fn column_paras(col: &Column, style: &Style) -> Vec<Para> {
    let mut out = Vec::new();
    if let Some(h) = &col.heading {
        out.push(Para { text: h.clone(), size: Some(style.body_size + 2.0), color: Some(style.palette.accent.clone()), bold: true, ..Default::default() });
    }
    let mut bullets = bullet_paras(&col.bullets, style, style.body_size);
    if let (Some(first), false) = (bullets.first_mut(), out.is_empty()) {
        first.space_before = Some(style.body_size * 0.6);
    }
    out.extend(bullets);
    out
}

/// 把页面规格展开成带位置的元素
pub fn layout(page: &Page, style: &Style) -> Vec<Element> {
    let (pw, ph) = style.page_size();
    let p = &style.palette;
    let layout = page.layout.as_deref().unwrap_or(if page.chart.is_some() {
        "chart"
    } else if page.table.is_some() {
        "table"
    } else if !page.metrics.is_empty() {
        "metrics"
    } else if page.left.is_some() || page.right.is_some() {
        "two-column"
    } else if page.image.is_some() {
        "image-right"
    } else if page.quote.is_some() {
        "quote"
    } else if !page.bullets.is_empty() {
        "bullets"
    } else {
        "blank"
    });
    let title = page.title.clone().unwrap_or_default();
    let body_top = 128.0;
    let body_h = ph - body_top - 40.0;
    let mut els: Vec<Element> = Vec::new();
    match layout {
        "cover" | "closing" | "section" => {
            let (title_size, top) = match layout {
                "section" => (style.title_size + 6.0, ph * 0.36),
                _ => (style.title_size + 12.0, ph * 0.30),
            };
            els.push(Element::Shape { x: M, y: top, w: 8.0, h: 132.0, shape: Some("rect".into()), fill: Some(p.accent.clone()), line: None, line_width: None });
            let mut t = text_el(M + 32.0, top, pw - 2.0 * M - 32.0, 88.0, vec![Para { text: title.clone(), size: Some(title_size), color: Some(p.text.clone()), bold: true, ..Default::default() }]);
            if let Element::Text { valign, font, title: is_title, .. } = &mut t {
                *valign = Some("bottom".into());
                *font = Some(style.fonts.heading.clone());
                *is_title = true;
            }
            els.push(t);
            if let Some(sub) = &page.subtitle {
                els.push(text_el(M + 32.0, top + 96.0, pw - 2.0 * M - 32.0, 36.0, vec![para(sub, style.body_size + 2.0, &p.muted, false)]));
            }
            if !page.bullets.is_empty() {
                els.push(text_el(M + 32.0, top + 148.0, pw - 2.0 * M - 32.0, ph - top - 188.0, bullet_paras(&page.bullets, style, style.body_size)));
            }
        }
        "two-column" => {
            els.extend(title_el(pw, &title, style));
            let gap = 32.0;
            let cw = (pw - 2.0 * M - gap) / 2.0;
            if let Some(l) = &page.left {
                els.push(text_el(M, body_top, cw, body_h, column_paras(l, style)));
            }
            if let Some(r) = &page.right {
                els.push(text_el(M + cw + gap, body_top, cw, body_h, column_paras(r, style)));
            }
        }
        "metrics" => {
            els.extend(title_el(pw, &title, style));
            let n = page.metrics.len().clamp(1, 4) as f64;
            let gap = 24.0;
            let cw = (pw - 2.0 * M - gap * (n - 1.0)) / n;
            let card_h = if page.bullets.is_empty() { 220.0 } else { 180.0 };
            let top = body_top + 24.0;
            for (i, m) in page.metrics.iter().take(4).enumerate() {
                let x = M + i as f64 * (cw + gap);
                els.push(Element::Shape { x, y: top, w: cw, h: card_h, shape: Some("round".into()), fill: Some(p.surface.clone()), line: None, line_width: None });
                let mut paras = vec![para(&m.value, 40.0, &p.accent, true), Para { space_before: Some(8.0), ..para(&m.label, 16.0, &p.text, true) }];
                if let Some(note) = &m.note {
                    paras.push(Para { space_before: Some(6.0), ..para(note, 12.0, &p.muted, false) });
                }
                let mut t = text_el(x + 20.0, top + 20.0, cw - 40.0, card_h - 40.0, paras);
                if let Element::Text { valign, .. } = &mut t {
                    *valign = Some("middle".into());
                }
                els.push(t);
            }
            if !page.bullets.is_empty() {
                let y = top + card_h + 24.0;
                els.push(text_el(M, y, pw - 2.0 * M, ph - y - 32.0, bullet_paras(&page.bullets, style, style.body_size - 2.0)));
            }
        }
        "table" => {
            els.extend(title_el(pw, &title, style));
            if let Some(t) = &page.table {
                let size = t.size.unwrap_or((style.body_size - 4.0).max(11.0));
                let row_h = size * 2.0;
                let mut h = (row_h * t.rows.len() as f64).min(body_h);
                let mut top = body_top;
                if !page.bullets.is_empty() {
                    h = h.min(body_h - 90.0);
                    let y = top + h + 20.0;
                    els.push(text_el(M, y, pw - 2.0 * M, ph - y - 32.0, bullet_paras(&page.bullets, style, style.body_size - 2.0)));
                } else {
                    top = body_top + 8.0;
                }
                els.push(Element::Table { x: M, y: top, w: pw - 2.0 * M, h, rows: t.rows.clone(), header: t.header, size: Some(size) });
            }
        }
        "chart" => {
            els.extend(title_el(pw, &title, style));
            if let Some(c) = &page.chart {
                if page.bullets.is_empty() {
                    els.push(Element::Chart { x: M, y: body_top, w: pw - 2.0 * M, h: body_h, chart: c.clone() });
                } else {
                    let cw = (pw - 2.0 * M) * 0.62;
                    els.push(Element::Chart { x: M, y: body_top, w: cw, h: body_h, chart: c.clone() });
                    els.push(text_el(M + cw + 28.0, body_top + 12.0, pw - 2.0 * M - cw - 28.0, body_h - 12.0, bullet_paras(&page.bullets, style, style.body_size - 2.0)));
                }
            }
        }
        "image-left" | "image-right" => {
            let iw = (pw * 0.42).round();
            let image_left = layout == "image-left";
            let ix = if image_left { 0.0 } else { pw - iw };
            if let Some(img) = &page.image {
                els.push(Element::Image { x: ix, y: 0.0, w: iw, h: ph, path: img.path.clone(), fit: img.fit.clone().or(Some("cover".into())) });
            }
            let tx = if image_left { iw + 40.0 } else { M };
            let tw = pw - iw - 40.0 - M;
            let mut t = text_el(tx, 56.0, tw, 96.0, vec![Para { text: title.clone(), size: Some(style.title_size), color: Some(p.text.clone()), bold: true, ..Default::default() }]);
            if let Element::Text { valign, font, title: is_title, .. } = &mut t {
                *valign = Some("bottom".into());
                *font = Some(style.fonts.heading.clone());
                *is_title = true;
            }
            els.push(t);
            els.push(Element::Shape { x: tx, y: 164.0, w: 56.0, h: 4.0, shape: Some("rect".into()), fill: Some(p.accent.clone()), line: None, line_width: None });
            let mut paras = Vec::new();
            if let Some(sub) = &page.subtitle {
                paras.push(para(sub, style.body_size, &p.muted, false));
            }
            let mut b = bullet_paras(&page.bullets, style, style.body_size);
            if let (Some(first), false) = (b.first_mut(), paras.is_empty()) {
                first.space_before = Some(12.0);
            }
            paras.extend(b);
            if !paras.is_empty() {
                els.push(text_el(tx, 188.0, tw, ph - 188.0 - 48.0, paras));
            }
            if let Some(cap) = page.image.as_ref().and_then(|i| i.caption.as_ref()) {
                els.push(text_el(tx, ph - 40.0, tw, 24.0, vec![para(cap, 11.0, &p.muted, false)]));
            }
        }
        "quote" => {
            if let Some(q) = &page.quote {
                els.push(Element::Shape { x: M, y: ph * 0.28, w: 8.0, h: 160.0, shape: Some("rect".into()), fill: Some(p.accent.clone()), line: None, line_width: None });
                let mut paras = vec![Para { text: q.text.clone(), size: Some(28.0), color: Some(p.text.clone()), italic: true, ..Default::default() }];
                if let Some(by) = &q.by {
                    paras.push(Para { text: format!("—— {by}"), size: Some(16.0), color: Some(p.muted.clone()), space_before: Some(18.0), ..Default::default() });
                }
                let mut t = text_el(M + 36.0, ph * 0.28, pw - 2.0 * M - 36.0, 200.0, paras);
                if let Element::Text { valign, .. } = &mut t {
                    *valign = Some("middle".into());
                }
                els.push(t);
            }
            if !title.is_empty() {
                els.extend(title_el(pw, &title, style));
            }
        }
        "blank" => {
            if !title.is_empty() {
                els.extend(title_el(pw, &title, style));
            }
        }
        _ => {
            // bullets（默认）
            els.extend(title_el(pw, &title, style));
            let mut paras = Vec::new();
            if let Some(sub) = &page.subtitle {
                paras.push(para(sub, style.body_size, &p.muted, false));
            }
            let mut b = bullet_paras(&page.bullets, style, style.body_size);
            if let (Some(first), false) = (b.first_mut(), paras.is_empty()) {
                first.space_before = Some(14.0);
            }
            paras.extend(b);
            els.push(text_el(M, body_top, pw - 2.0 * M, body_h, paras));
        }
    }
    els.extend(page.elements.iter().cloned());
    els
}

// ───────────────────────── 检查 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// error：必须修改；warning：建议修改
    pub level: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element: Option<usize>,
}

fn finding(level: &str, code: &str, message: String, element: Option<usize>) -> Finding {
    Finding { level: level.into(), code: code.into(), message, element }
}

/// 估算文字宽度（磅）：中日韩字符一个字宽，拉丁字符约半个
pub fn text_width(text: &str, size: f64) -> f64 {
    text.chars()
        .map(|c| {
            let u = c as u32;
            if u >= 0x2E80 || (0xFF00..=0xFFEF).contains(&u) {
                1.0
            } else if c == ' ' {
                0.28
            } else if c.is_ascii_uppercase() || c.is_ascii_digit() {
                0.62
            } else if c.is_ascii_punctuation() {
                0.36
            } else {
                0.52
            }
        })
        .sum::<f64>()
        * size
}

pub const LINE: f64 = 1.2;
pub const BULLET_INDENT: f64 = 22.0;
pub const LEVEL_INDENT: f64 = 24.0;

/// 段落在给定宽度下大约占几行
pub fn lines_for(text: &str, size: f64, width: f64) -> usize {
    if width <= size {
        return text.chars().count().max(1);
    }
    text.split('\n')
        .map(|line| {
            let w = text_width(line, size);
            ((w / width).ceil() as usize).max(1)
        })
        .sum()
}

/// 文字元素所需高度
fn text_height(paras: &[Para], default_size: f64, inner_w: f64) -> f64 {
    let mut h = 0.0;
    for (i, p) in paras.iter().enumerate() {
        let size = p.size.unwrap_or(default_size);
        let indent = if p.bullet { BULLET_INDENT + p.level as f64 * LEVEL_INDENT } else { p.level as f64 * LEVEL_INDENT };
        let plain: String = markdown::plain(&markdown::inline(&p.text));
        let lines = lines_for(&plain, size, inner_w - indent);
        if i > 0 {
            h += p.space_before.unwrap_or(size * 0.3);
        }
        h += lines as f64 * size * LINE;
    }
    h
}

fn text_paras(el: &Element, style: &Style) -> Vec<Para> {
    match el {
        Element::Text { text, paragraphs, size, color, bold, italic, align, .. } => {
            if !paragraphs.is_empty() {
                return paragraphs.clone();
            }
            text.as_deref()
                .unwrap_or("")
                .split('\n')
                .map(|t| Para { text: t.to_string(), size: Some(size.unwrap_or(style.body_size)), color: color.clone(), bold: *bold, italic: *italic, align: align.clone(), ..Default::default() })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn padding_of(el: &Element) -> f64 {
    match el {
        Element::Text { padding, fill, .. } => padding.unwrap_or(if fill.is_some() { 10.0 } else { 0.0 }),
        _ => 0.0,
    }
}

fn cell_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn overlap(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> f64 {
    let w = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
    let h = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
    if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
}

fn inside(inner: (f64, f64, f64, f64), outer: (f64, f64, f64, f64)) -> bool {
    inner.0 >= outer.0 - 1.0 && inner.1 >= outer.1 - 1.0 && inner.0 + inner.2 <= outer.0 + outer.2 + 1.0 && inner.1 + inner.3 <= outer.1 + outer.3 + 1.0
}

fn norm_title(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation() && !"，。：；！？、“”《》（）".contains(*c)).collect::<String>().to_lowercase()
}

/// 检查一页：返回发现的问题
pub fn check_page(page: &Page, style: &Style, outline: Option<(&Outline, usize)>, base_dir: Option<&std::path::Path>) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(l) = &page.layout {
        if !LAYOUTS.contains(&l.as_str()) {
            out.push(finding("error", "layout", format!("没有“{l}”版式，可选：{}", LAYOUTS.join(" / ")), None));
            return out;
        }
    }
    let els = layout(page, style);
    let (pw, ph) = style.page_size();
    let colors = style.colors();
    let check_color = |c: &Option<String>, i: usize, out: &mut Vec<Finding>| {
        if let Some(c) = c {
            match hex(c) {
                None => out.push(finding("error", "color", format!("元素 {i} 的颜色“{c}”不是 #RRGGBB"), Some(i))),
                Some(h) if !colors.contains(&h) => out.push(finding("warning", "palette", format!("元素 {i} 用了配色表之外的颜色 {h}"), Some(i))),
                _ => {}
            }
        }
    };
    let layout_name = page.layout.as_deref().unwrap_or("");
    if page.title.as_deref().map(str::trim).unwrap_or("").is_empty() && !matches!(layout_name, "blank" | "quote") {
        out.push(finding("error", "title", "缺少 title".into(), None));
    }
    if page.bullets.len() > 7 {
        out.push(finding("warning", "density", format!("要点有 {} 条，演示时太密，建议不超过 6 条或拆页", page.bullets.len()), None));
    }
    for b in &page.bullets {
        let (t, _) = b.parts();
        if t.chars().count() > 70 {
            out.push(finding("warning", "density", format!("要点“{}…”太长（{} 字），建议精简到 40 字以内", t.chars().take(12).collect::<String>(), t.chars().count()), None));
        }
    }
    if matches!(layout_name, "table") && page.table.is_none() {
        out.push(finding("error", "content", "table 版式需要 table".into(), None));
    }
    if matches!(layout_name, "chart") && page.chart.is_none() {
        out.push(finding("error", "content", "chart 版式需要 chart".into(), None));
    }
    if matches!(layout_name, "image-left" | "image-right") && page.image.is_none() {
        out.push(finding("error", "content", "图片版式需要 image".into(), None));
    }
    if matches!(layout_name, "metrics") && page.metrics.is_empty() {
        out.push(finding("error", "content", "metrics 版式需要 metrics".into(), None));
    }
    if page.metrics.len() > 4 {
        out.push(finding("warning", "density", "metrics 最多显示 4 个，多出的会被忽略".into(), None));
    }

    for (i, el) in els.iter().enumerate() {
        let (x, y, w, h) = el.frame();
        if w <= 0.0 || h <= 0.0 {
            out.push(finding("error", "frame", format!("元素 {i}（{}）宽高必须大于 0", el.kind()), Some(i)));
            continue;
        }
        if x < -0.5 || y < -0.5 || x + w > pw + 0.5 || y + h > ph + 0.5 {
            out.push(finding("error", "offslide", format!("元素 {i}（{}）超出页面：x={x}, y={y}, w={w}, h={h}，页面为 {pw}×{ph}", el.kind()), Some(i)));
        }
        match el {
            Element::Text { color, fill, .. } => {
                check_color(color, i, &mut out);
                check_color(fill, i, &mut out);
                let paras = text_paras(el, style);
                for p in &paras {
                    check_color(&p.color, i, &mut out);
                    if p.size.unwrap_or(style.body_size) < 10.0 {
                        out.push(finding("warning", "font", format!("元素 {i} 有 {} 磅的文字，投影时看不清（建议 ≥ 12）", p.size.unwrap_or(style.body_size)), Some(i)));
                    }
                }
                let pad = padding_of(el);
                let need = text_height(&paras, style.body_size, w - 2.0 * pad) + 2.0 * pad;
                if need > h * 1.12 {
                    out.push(finding("error", "overflow", format!("元素 {i} 的文字放不下：约需 {need:.0} 磅高，框只有 {h:.0} 磅。精简文字、减小字号或拆页"), Some(i)));
                } else if need > h {
                    out.push(finding("warning", "overflow", format!("元素 {i} 的文字可能贴边：约需 {need:.0} 磅，框高 {h:.0} 磅"), Some(i)));
                }
            }
            Element::Shape { fill, line, .. } => {
                check_color(fill, i, &mut out);
                check_color(line, i, &mut out);
            }
            Element::Table { rows, size, .. } => {
                let size = size.unwrap_or(style.body_size - 4.0);
                let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
                let cw = w / cols as f64 - 10.0;
                let need: f64 = rows.iter().map(|r| r.iter().map(|c| lines_for(&cell_text(c), size, cw)).max().unwrap_or(1) as f64 * size * LINE + 8.0).sum();
                if need > h * 1.1 {
                    out.push(finding("error", "overflow", format!("表格放不下：约需 {need:.0} 磅高，只有 {h:.0} 磅。减少行数或字号，或拆成两页"), Some(i)));
                }
                if rows.len() > 12 {
                    out.push(finding("warning", "density", format!("表格有 {} 行，演示时太密", rows.len()), Some(i)));
                }
            }
            Element::Chart { chart, .. } => {
                if !matches!(chart.kind.as_str(), "column" | "bar" | "line" | "pie" | "doughnut") {
                    out.push(finding("error", "chart", format!("图表类型“{}”不支持，可选 column / bar / line / pie / doughnut", chart.kind), Some(i)));
                }
                if chart.series.is_empty() || chart.categories.is_empty() {
                    out.push(finding("error", "chart", "图表需要 categories 和至少一个 series".into(), Some(i)));
                }
                for s in &chart.series {
                    if s.values.len() != chart.categories.len() {
                        out.push(finding("error", "chart", format!("系列“{}”有 {} 个值，categories 有 {} 个", s.name, s.values.len(), chart.categories.len()), Some(i)));
                    }
                }
                if matches!(chart.kind.as_str(), "pie" | "doughnut") && chart.series.len() > 1 {
                    out.push(finding("warning", "chart", "饼图只显示第一个系列".into(), Some(i)));
                }
            }
            Element::Image { path, .. } => {
                let full = match base_dir {
                    Some(d) if !std::path::Path::new(path).is_absolute() => d.join(path),
                    _ => std::path::PathBuf::from(path),
                };
                match std::fs::read(&full) {
                    Ok(bytes) if image_info(&bytes).is_some() => {}
                    Ok(_) => out.push(finding("error", "image", format!("图片 {path} 不是 PNG / JPEG / GIF"), Some(i))),
                    Err(_) => out.push(finding("error", "image", format!("找不到图片 {}", full.display()), Some(i))),
                }
            }
        }
    }
    // 重叠：内容元素之间（形状当作背景，文字放在形状里不算）
    let content: Vec<(usize, (f64, f64, f64, f64))> = els.iter().enumerate().filter(|(_, e)| !matches!(e, Element::Shape { .. })).map(|(i, e)| (i, e.frame())).collect();
    for (ai, (i, a)) in content.iter().enumerate() {
        for (j, b) in content.iter().skip(ai + 1) {
            let area = overlap(*a, *b);
            let smaller = (a.2 * a.3).min(b.2 * b.3).max(1.0);
            if area / smaller > 0.08 {
                let level = if area / smaller > 0.3 { "error" } else { "warning" };
                out.push(finding(level, "overlap", format!("元素 {i}（{}）与元素 {j}（{}）重叠 {:.0}%", els[*i].kind(), els[*j].kind(), area / smaller * 100.0), Some(*j)));
            }
        }
    }
    let _ = inside;
    if let Some((outline, n)) = outline {
        match outline.pages.get(n.saturating_sub(1)) {
            None => out.push(finding("error", "outline", format!("大纲只有 {} 页，没有第 {n} 页", outline.pages.len()), None)),
            Some(op) => {
                let t = page.title.as_deref().unwrap_or("");
                if !t.is_empty() && norm_title(t) != norm_title(&op.title) && !norm_title(t).contains(&norm_title(&op.title)) && !norm_title(&op.title).contains(&norm_title(t)) {
                    out.push(finding("warning", "outline", format!("标题“{t}”与大纲第 {n} 页“{}”不一致", op.title), None));
                }
            }
        }
    }
    out
}

/// 检查大纲
pub fn check_outline(outline: &Outline) -> Vec<Finding> {
    let mut out = Vec::new();
    if outline.pages.is_empty() {
        out.push(finding("error", "outline", "大纲没有页面".into(), None));
    }
    if outline.pages.len() > 60 {
        out.push(finding("warning", "outline", format!("大纲有 {} 页，篇幅偏长", outline.pages.len()), None));
    }
    for (i, p) in outline.pages.iter().enumerate() {
        if p.title.trim().is_empty() {
            out.push(finding("error", "outline", format!("大纲第 {} 页缺少标题", i + 1), None));
        }
    }
    out
}

/// 审查已有 .pptx：文字溢出、重叠、超出页面、字号过小（按形状位置和字号估算）
pub fn audit(slides: &[crate::pptx::Slide], size: (f64, f64)) -> Vec<(usize, Finding)> {
    let (pw, ph) = size;
    let mut out = Vec::new();
    for s in slides {
        let shapes: Vec<&crate::pptx::Shape> = s.shapes.iter().filter(|x| x.frame.is_some()).collect();
        for sh in &shapes {
            let [x, y, w, h] = sh.frame.unwrap();
            let label = format!("「{}」", sh.name);
            if x < -1.0 || y < -1.0 || x + w > pw + 1.0 || y + h > ph + 1.0 {
                if sh.kind != "picture" {
                    out.push((s.index, finding("error", "offslide", format!("{label} 超出页面"), None)));
                }
            }
            let size = sh.font_size.unwrap_or(18.0);
            if !sh.text.is_empty() && sh.kind != "picture" {
                if size < 10.0 {
                    out.push((s.index, finding("warning", "font", format!("{label} 字号 {size} 磅，投影时看不清"), None)));
                }
                let need = match &sh.rows {
                    Some(rows) => {
                        let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
                        rows.iter().map(|r| r.iter().map(|c| lines_for(c, size, w / cols as f64 - 10.0)).max().unwrap_or(1) as f64 * size * LINE + 8.0).sum::<f64>()
                    }
                    // 按每段自己的字号估算（同一个框里大数字和小标签字号不同）
                    None if !sh.paragraphs.is_empty() => sh.paragraphs.iter().map(|(t, sz)| {
                        let sz = sz.unwrap_or(size);
                        lines_for(t, sz, w - 14.4) as f64 * sz * LINE
                    }).sum::<f64>() + 7.2,
                    None => sh.text.split('\n').map(|l| lines_for(l, size, w - 14.4) as f64 * size * LINE).sum::<f64>() + 7.2,
                };
                if need > h * 1.15 && sh.kind != "title" {
                    out.push((s.index, finding("error", "overflow", format!("{label} 的文字可能放不下：约需 {need:.0} 磅高，框高 {h:.0} 磅"), None)));
                } else if need > h * 1.15 {
                    out.push((s.index, finding("warning", "overflow", format!("{label} 标题可能换行过多"), None)));
                }
            }
        }
        let content: Vec<&&crate::pptx::Shape> = shapes.iter().filter(|x| matches!(x.kind.as_str(), "title" | "subtitle" | "body" | "text" | "table" | "chart" | "picture")).collect();
        for (i, a) in content.iter().enumerate() {
            for b in content.iter().skip(i + 1) {
                let fa = a.frame.unwrap();
                let fb = b.frame.unwrap();
                let area = overlap((fa[0], fa[1], fa[2], fa[3]), (fb[0], fb[1], fb[2], fb[3]));
                let smaller = (fa[2] * fa[3]).min(fb[2] * fb[3]).max(1.0);
                if area / smaller > 0.15 && !(a.kind == "picture" || b.kind == "picture") {
                    out.push((s.index, finding("warning", "overlap", format!("「{}」与「{}」重叠 {:.0}%", a.name, b.name, area / smaller * 100.0), None)));
                }
            }
        }
    }
    out
}

// ───────────────────────── 图片 ─────────────────────────

/// (扩展名, 宽, 高)
pub fn image_info(bytes: &[u8]) -> Option<(&'static str, u32, u32)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) && bytes.len() > 24 {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some(("png", w, h));
    }
    if bytes.starts_with(b"GIF8") && bytes.len() > 10 {
        return Some(("gif", u16::from_le_bytes([bytes[6], bytes[7]]) as u32, u16::from_le_bytes([bytes[8], bytes[9]]) as u32));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some(("jpeg", w, h));
            }
            i += 2 + len;
        }
    }
    None
}

// ───────────────────────── 整套 ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckSpec {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub style: Style,
    pub pages: Vec<Page>,
}

pub struct BuildResult {
    pub bytes: Vec<u8>,
    pub findings: Vec<(usize, Finding)>,
}

/// 生成 .pptx。base_dir 用来解析图片相对路径
pub fn build(spec: &DeckSpec, base_dir: Option<&std::path::Path>) -> Result<BuildResult> {
    spec.style.validate().map_err(EngineError::Invalid)?;
    if spec.pages.is_empty() {
        return Err(EngineError::Invalid("没有页面".into()));
    }
    let mut findings = Vec::new();
    for (i, page) in spec.pages.iter().enumerate() {
        for f in check_page(page, &spec.style, None, base_dir) {
            if f.level == "error" && matches!(f.code.as_str(), "image" | "layout" | "frame" | "chart") {
                return Err(EngineError::Invalid(format!("第 {} 页：{}", i + 1, f.message)));
            }
            findings.push((i + 1, f));
        }
    }
    let bytes = super::deck_render::render(spec, base_dir)?;
    Ok(BuildResult { bytes, findings })
}

/// 便于测试：所有元素
pub fn elements(spec: &DeckSpec) -> HashMap<usize, Vec<Element>> {
    spec.pages.iter().enumerate().map(|(i, p)| (i + 1, layout(p, &spec.style))).collect()
}

pub(crate) fn esc(s: &str) -> String {
    escape(s)
}

pub(crate) fn esc_attr(s: &str) -> String {
    escape_attr(s)
}
