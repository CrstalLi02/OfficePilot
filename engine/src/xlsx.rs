//! Excel（.xlsx）：读取单元格（值与公式），写入值和公式（同时算出缓存值），
//! 新增 / 重命名工作表，插入图表。只改写涉及的行，其余 XML 原样保留。

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::formula::{self, cell_name, index_to_col, parse_cell, parse_range, quote_sheet, split_sheet, Lookup, Value};
use crate::package::{insert_before_close, Package};
use crate::xml::{self, Splice, Xml};
use crate::{clip, op_err, revision_of, ApplyReport, Change, EngineError, Result};

const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const REL_WORKSHEET: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
const REL_DRAWING: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing";
const REL_CHART: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart";
const CT_WORKSHEET: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml";
const CT_DRAWING: &str = "application/vnd.openxmlformats-officedocument.drawing+xml";
const CT_CHART: &str = "application/vnd.openxmlformats-officedocument.drawingml.chart+xml";

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn child_local(xml: &Xml, node: usize, name: &str) -> Option<usize> {
    xml.children(node).iter().copied().find(|&c| local(xml.name(c)) == name)
}

fn children_local<'a>(xml: &'a Xml, node: usize, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    xml.children(node).iter().copied().filter(move |&c| local(xml.name(c)) == name)
}

fn prefix_of(name: &str) -> String {
    match name.find(':') {
        Some(p) => name[..=p].to_string(),
        None => String::new(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SheetMeta {
    pub name: String,
    #[serde(skip)]
    pub part: String,
    #[serde(skip)]
    rid: String,
    #[serde(skip)]
    sheet_id: u32,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub value: Value,
    pub formula: Option<String>,
    pub style: Option<u32>,
}

pub struct Workbook {
    pkg: Package,
    wb_part: String,
    wb_text: String,
    pub sheets: Vec<SheetMeta>,
    shared: Vec<String>,
    date_xfs: HashSet<u32>,
    cache: RefCell<HashMap<String, BTreeMap<(u32, u32), Cell>>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SheetInfo {
    pub name: String,
    pub dimension: String,
    pub rows: u32,
    pub cols: u32,
    pub cells: usize,
    pub formulas: usize,
    pub charts: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkbookInfo {
    pub sheets: Vec<SheetInfo>,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CellOut {
    pub cell: String,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SheetView {
    pub sheet: String,
    pub range: String,
    pub dimension: String,
    pub total_rows: u32,
    /// 每行：(行号, 单元格)
    pub rows: Vec<(u32, Vec<CellOut>)>,
    #[serde(skip)]
    first_col: u32,
    #[serde(skip)]
    last_col: u32,
}

fn shared_strings(text: &str) -> Result<Vec<String>> {
    let xml = Xml::parse(text)?;
    let Some(root) = xml.document_element() else { return Ok(Vec::new()) };
    Ok(children_local(&xml, root, "si")
        .map(|si| {
            xml.descendants(si)
                .into_iter()
                .filter(|&d| local(xml.name(d)) == "t" && !xml.descendants(si).iter().any(|&p| local(xml.name(p)) == "rPh" && xml.descendants(p).contains(&d)))
                .map(|d| xml.text(d))
                .collect()
        })
        .collect())
}

fn date_styles(text: &str) -> Result<HashSet<u32>> {
    let xml = Xml::parse(text)?;
    let Some(root) = xml.document_element() else { return Ok(HashSet::new()) };
    let mut custom: HashMap<u32, String> = HashMap::new();
    if let Some(fmts) = child_local(&xml, root, "numFmts") {
        for f in children_local(&xml, fmts, "numFmt") {
            if let (Some(id), Some(code)) = (xml.attr(f, "numFmtId").and_then(|v| v.parse().ok()), xml.attr(f, "formatCode")) {
                custom.insert(id, code);
            }
        }
    }
    let is_date_code = |code: &str| {
        let mut plain = String::new();
        let mut quoted = false;
        let mut bracket = false;
        for c in code.chars() {
            match c {
                '"' => quoted = !quoted,
                '[' if !quoted => bracket = true,
                ']' if !quoted => bracket = false,
                c if !quoted && !bracket => plain.push(c.to_ascii_lowercase()),
                _ => {}
            }
        }
        plain.contains('y') || plain.contains('d') || (plain.contains('m') && (plain.contains('h') || plain.contains('s') || plain.contains('/') || plain.contains('-')))
    };
    let mut out = HashSet::new();
    if let Some(xfs) = child_local(&xml, root, "cellXfs") {
        for (i, xf) in children_local(&xml, xfs, "xf").enumerate() {
            let id: u32 = xml.attr(xf, "numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0);
            let date = matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58) || custom.get(&id).map(|c| is_date_code(c)).unwrap_or(false);
            if date {
                out.insert(i as u32);
            }
        }
    }
    Ok(out)
}

/// 解析工作表的单元格；共享公式展开成各自的公式文字
fn parse_cells(text: &str, shared: &[String]) -> Result<BTreeMap<(u32, u32), Cell>> {
    let xml = Xml::parse(text)?;
    let mut out = BTreeMap::new();
    let Some(root) = xml.document_element() else { return Ok(out) };
    let Some(data) = child_local(&xml, root, "sheetData") else { return Ok(out) };
    let mut masters: HashMap<String, (String, u32, u32)> = HashMap::new();
    let mut pending_shared: Vec<((u32, u32), String)> = Vec::new();
    let mut row_no = 0;
    for row in children_local(&xml, data, "row") {
        row_no = xml.attr(row, "r").and_then(|v| v.parse().ok()).unwrap_or(row_no + 1);
        let mut col = 0;
        for c in children_local(&xml, row, "c") {
            col = xml.attr(c, "r").and_then(|r| parse_cell(&r)).map(|(_, c)| c).unwrap_or(col + 1);
            let t = xml.attr(c, "t").unwrap_or_default();
            let v = child_local(&xml, c, "v").map(|v| xml.text(v));
            let value = match t.as_str() {
                "s" => v.and_then(|i| i.trim().parse::<usize>().ok()).and_then(|i| shared.get(i).cloned()).map(Value::Text).unwrap_or(Value::Empty),
                "inlineStr" => child_local(&xml, c, "is").map(|is| Value::Text(xml.descendants(is).into_iter().filter(|&d| local(xml.name(d)) == "t").map(|d| xml.text(d)).collect())).unwrap_or(Value::Empty),
                "str" => v.map(Value::Text).unwrap_or(Value::Empty),
                "b" => v.map(|v| Value::Bool(v.trim() == "1")).unwrap_or(Value::Empty),
                "e" => v.map(Value::Err).unwrap_or(Value::Empty),
                "d" => v.map(Value::Text).unwrap_or(Value::Empty),
                _ => match v.as_deref().map(str::trim) {
                    Some(s) if !s.is_empty() => s.parse::<f64>().map(Value::Num).unwrap_or(Value::Text(s.to_string())),
                    _ => Value::Empty,
                },
            };
            let mut formula = None;
            if let Some(f) = child_local(&xml, c, "f") {
                let ftext = xml.text(f);
                let kind = xml.attr(f, "t").unwrap_or_default();
                if kind == "shared" {
                    let si = xml.attr(f, "si").unwrap_or_default();
                    if !ftext.trim().is_empty() {
                        masters.insert(si, (ftext.clone(), row_no, col));
                        formula = Some(ftext);
                    } else {
                        pending_shared.push(((row_no, col), si));
                    }
                } else if !ftext.trim().is_empty() {
                    formula = Some(ftext);
                }
            }
            let style = xml.attr(c, "s").and_then(|s| s.parse().ok());
            if value != Value::Empty || formula.is_some() || style.is_some() {
                out.insert((row_no, col), Cell { value, formula, style });
            }
        }
    }
    for ((r, c), si) in pending_shared {
        if let Some((f, mr, mc)) = masters.get(&si) {
            if let Some(cell) = out.get_mut(&(r, c)) {
                cell.formula = formula::shift(f, r as i64 - *mr as i64, c as i64 - *mc as i64);
            }
        }
    }
    Ok(out)
}

fn excel_date(serial: f64) -> Option<String> {
    let base = chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?.and_hms_opt(0, 0, 0)?;
    let dt = base + chrono::Duration::milliseconds((serial * 86_400_000.0).round() as i64);
    Some(if serial.fract().abs() < 1e-9 { dt.format("%Y-%m-%d").to_string() } else { dt.format("%Y-%m-%d %H:%M").to_string() })
}

impl Workbook {
    pub fn open(bytes: Vec<u8>) -> Result<Self> {
        let pkg = Package::open(bytes)?;
        let wb_part = pkg.main_part()?;
        if !wb_part.starts_with("xl/") {
            return Err(EngineError::Format("不是 Excel 工作簿".into()));
        }
        let wb_text = pkg.read_str(&wb_part)?;
        let rels = pkg.rels(&wb_part)?;
        let sheets = {
            let xml = Xml::parse(&wb_text)?;
            let root = xml.document_element().ok_or_else(|| EngineError::Format("workbook.xml 为空".into()))?;
            let list = child_local(&xml, root, "sheets").ok_or_else(|| EngineError::Format("workbook.xml 缺少 sheets".into()))?;
            let mut sheets = Vec::new();
            for s in children_local(&xml, list, "sheet") {
                let rid = xml.attr(s, "r:id").or_else(|| {
                    // 关系命名空间可能用了别的前缀
                    let tag = xml.open_tag(s);
                    tag.split_whitespace().find(|a| a.contains(":id=")).and_then(|a| a.split('"').nth(1)).map(str::to_string)
                });
                let Some(rid) = rid else { continue };
                let Some(rel) = rels.iter().find(|r| r.id == rid) else { continue };
                // 图表工作表等非普通工作表跳过
                if !rel.kind.ends_with("/worksheet") {
                    continue;
                }
                sheets.push(SheetMeta {
                    name: xml.attr(s, "name").unwrap_or_default(),
                    part: Package::resolve(&wb_part, &rel.target),
                    rid,
                    sheet_id: xml.attr(s, "sheetId").and_then(|v| v.parse().ok()).unwrap_or(0),
                    hidden: matches!(xml.attr(s, "state").as_deref(), Some("hidden") | Some("veryHidden")),
                });
            }
            sheets
        };
        let shared = match pkg.target_of(&wb_part, "/sharedStrings")? {
            Some(p) if pkg.has(&p) => shared_strings(&pkg.read_str(&p)?)?,
            _ => Vec::new(),
        };
        let date_xfs = match pkg.target_of(&wb_part, "/styles")? {
            Some(p) if pkg.has(&p) => date_styles(&pkg.read_str(&p)?)?,
            _ => HashSet::new(),
        };
        Ok(Self { pkg, wb_part, wb_text, sheets, shared, date_xfs, cache: RefCell::new(HashMap::new()) })
    }

    pub fn revision(&self) -> Result<String> {
        let mut all = self.wb_text.clone();
        for s in &self.sheets {
            all.push_str(&self.pkg.read_str(&s.part)?);
        }
        Ok(revision_of(&all))
    }

    fn sheet_index(&self, name: Option<&str>) -> Result<usize> {
        match name.map(str::trim).filter(|n| !n.is_empty()) {
            None => self.sheets.iter().position(|s| !s.hidden).or(if self.sheets.is_empty() { None } else { Some(0) }).ok_or_else(|| EngineError::Invalid("工作簿里没有工作表".into())),
            Some(n) => self.sheets.iter().position(|s| s.name == n).or_else(|| self.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(n))).ok_or_else(|| {
                EngineError::Invalid(format!("找不到工作表“{n}”，现有：{}", self.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join("、")))
            }),
        }
    }

    pub fn cells(&self, idx: usize) -> Result<BTreeMap<(u32, u32), Cell>> {
        let part = &self.sheets[idx].part;
        if let Some(c) = self.cache.borrow().get(part) {
            return Ok(c.clone());
        }
        let cells = parse_cells(&self.pkg.read_str(part)?, &self.shared)?;
        self.cache.borrow_mut().insert(part.clone(), cells.clone());
        Ok(cells)
    }

    fn display(&self, cell: &Cell) -> serde_json::Value {
        match &cell.value {
            Value::Num(n) if cell.style.map(|s| self.date_xfs.contains(&s)).unwrap_or(false) => excel_date(*n).map(serde_json::Value::String).unwrap_or(serde_json::json!(n)),
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => serde_json::json!(*n as i64),
            Value::Num(n) => serde_json::json!(n),
            Value::Text(t) => serde_json::Value::String(t.clone()),
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Err(e) => serde_json::Value::String(e.clone()),
            Value::Empty => serde_json::Value::Null,
        }
    }

    fn charts_in(&self, idx: usize) -> usize {
        let part = &self.sheets[idx].part;
        let Ok(Some(drawing)) = self.pkg.target_of(part, "/drawing") else { return 0 };
        self.pkg.rels(&drawing).map(|r| r.iter().filter(|r| r.kind.ends_with("/chart")).count()).unwrap_or(0)
    }

    pub fn info(&self) -> Result<WorkbookInfo> {
        let mut sheets = Vec::new();
        for (i, s) in self.sheets.iter().enumerate() {
            let cells = self.cells(i)?;
            let filled: Vec<&(u32, u32)> = cells.iter().filter(|(_, c)| c.value != Value::Empty || c.formula.is_some()).map(|(k, _)| k).collect();
            let rows = filled.iter().map(|k| k.0).max().unwrap_or(0);
            let cols = filled.iter().map(|k| k.1).max().unwrap_or(0);
            let min_r = filled.iter().map(|k| k.0).min().unwrap_or(1);
            let min_c = filled.iter().map(|k| k.1).min().unwrap_or(1);
            sheets.push(SheetInfo {
                name: s.name.clone(),
                dimension: if filled.is_empty() { String::new() } else { format!("{}:{}", cell_name(min_r, min_c), cell_name(rows, cols)) },
                rows,
                cols,
                cells: filled.len(),
                formulas: cells.values().filter(|c| c.formula.is_some()).count(),
                charts: self.charts_in(i),
                hidden: s.hidden,
            });
        }
        Ok(WorkbookInfo { sheets, revision: self.revision()? })
    }

    /// 读取区域；不给区域时从已用区域开头读 max_rows 行
    pub fn read(&self, sheet: Option<&str>, range: Option<&str>, max_rows: u32) -> Result<SheetView> {
        let idx = self.sheet_index(sheet)?;
        let cells = self.cells(idx)?;
        let filled: Vec<(u32, u32)> = cells.iter().filter(|(_, c)| c.value != Value::Empty || c.formula.is_some()).map(|(k, _)| *k).collect();
        let dim = if filled.is_empty() {
            ((1, 1), (1, 1))
        } else {
            ((filled.iter().map(|k| k.0).min().unwrap(), filled.iter().map(|k| k.1).min().unwrap()), (filled.iter().map(|k| k.0).max().unwrap(), filled.iter().map(|k| k.1).max().unwrap()))
        };
        let ((r1, c1), (r2, c2)) = match range.map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) => parse_range(&split_sheet(r).1).ok_or_else(|| EngineError::Invalid(format!("区域“{r}”格式不对，示例：A1:D20")))?,
            None => (dim.0, (dim.1 .0.min(dim.0 .0 + max_rows.max(1) - 1), dim.1 .1)),
        };
        let r2 = r2.min(r1 + max_rows.max(1) - 1);
        // 没有缓存值的公式（有的程序写公式不写结果）现场算一下
        let empty = HashMap::new();
        let book = Book { wb: self, names: self.sheets.iter().map(|s| s.name.clone()).collect(), overlay: &empty, base: RefCell::new(HashMap::new()), computed: RefCell::new(HashMap::new()), visiting: RefCell::new(HashSet::new()), recalc: RefCell::new(HashSet::new()) };
        let mut cells = cells;
        for (&(r, c), cell) in cells.range_mut((r1, c1)..=(r2, c2)) {
            if c < c1 || c > c2 || cell.value != Value::Empty {
                continue;
            }
            if let Some(f) = cell.formula.clone() {
                book.recalc.borrow_mut().insert((idx, r, c));
                if let Some(v) = book.formula_value(idx, r, c, &f) {
                    cell.value = v;
                }
            }
        }
        drop(book);
        let mut rows = Vec::new();
        for r in r1..=r2 {
            let row: Vec<CellOut> = cells
                .range((r, c1)..=(r, c2))
                .filter(|(_, c)| c.value != Value::Empty || c.formula.is_some())
                .map(|(&(rr, cc), c)| CellOut { cell: cell_name(rr, cc), value: self.display(c), formula: c.formula.as_ref().map(|f| format!("={f}")) })
                .collect();
            rows.push((r, row));
        }
        Ok(SheetView {
            sheet: self.sheets[idx].name.clone(),
            range: format!("{}:{}", cell_name(r1, c1), cell_name(r2, c2)),
            dimension: format!("{}:{}", cell_name(dim.0 .0, dim.0 .1), cell_name(dim.1 .0, dim.1 .1)),
            total_rows: dim.1 .0,
            rows,
            first_col: c1,
            last_col: c2,
        })
    }
}

/// 给模型看的表格：列字母做表头，行号在左，公式单元格显示“值〔=公式〕”
pub fn format_view(view: &SheetView, max_cell_chars: usize) -> String {
    let mut out = format!("工作表「{}」区域 {}（已用区域 {}）\n", view.sheet, view.range, view.dimension);
    let cols: Vec<u32> = (view.first_col..=view.last_col).collect();
    out.push_str(&format!("行 | {}\n", cols.iter().map(|&c| index_to_col(c)).collect::<Vec<_>>().join(" | ")));
    for (r, cells) in &view.rows {
        let mut line = Vec::new();
        for &c in &cols {
            let name = cell_name(*r, c);
            let text = match cells.iter().find(|x| x.cell == name) {
                Some(cell) => {
                    let v = match &cell.value {
                        serde_json::Value::String(s) => s.replace('\n', " ").replace('|', "\\|"),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    let v = clip(&v, max_cell_chars);
                    match &cell.formula {
                        Some(f) => format!("{v}〔{f}〕"),
                        None => v,
                    }
                }
                None => String::new(),
            };
            line.push(text);
        }
        out.push_str(&format!("{r} | {}\n", line.join(" | ")));
    }
    out
}

// ───────────────────────── 编辑操作 ─────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct SeriesSpec {
    #[serde(default)]
    pub name: Option<String>,
    pub values: String,
}

fn default_chart() -> String {
    "column".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SheetOp {
    /// 写一个单元格：数字、文字、布尔，或以 = 开头的公式；null 清空
    Set {
        #[serde(default)]
        sheet: Option<String>,
        cell: String,
        value: serde_json::Value,
    },
    /// 从 start 开始写一块二维数据
    SetRange {
        #[serde(default)]
        sheet: Option<String>,
        start: String,
        values: Vec<Vec<serde_json::Value>>,
    },
    Clear {
        #[serde(default)]
        sheet: Option<String>,
        range: String,
    },
    AddSheet {
        name: String,
        #[serde(default)]
        values: Option<Vec<Vec<serde_json::Value>>>,
    },
    RenameSheet { sheet: String, name: String },
    AddChart {
        #[serde(default)]
        sheet: Option<String>,
        /// column / bar / line / pie
        #[serde(rename = "type", default = "default_chart")]
        chart_type: String,
        #[serde(default)]
        title: Option<String>,
        /// 分类（横轴）区域，如 Summary!A2:A5
        categories: String,
        series: Vec<SeriesSpec>,
        /// 左上角单元格，默认放在已用区域右侧
        #[serde(default)]
        anchor: Option<String>,
        /// 宽（列数）、高（行数）
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    },
}

impl SheetOp {
    fn name(&self) -> &'static str {
        match self {
            Self::Set { .. } => "set",
            Self::SetRange { .. } => "set_range",
            Self::Clear { .. } => "clear",
            Self::AddSheet { .. } => "add_sheet",
            Self::RenameSheet { .. } => "rename_sheet",
            Self::AddChart { .. } => "add_chart",
        }
    }
}

#[derive(Debug, Clone)]
enum Input {
    Value(Value),
    Formula(String),
}

fn to_input(v: &serde_json::Value) -> Option<Input> {
    Some(match v {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(b) => Input::Value(Value::Bool(*b)),
        serde_json::Value::Number(n) => Input::Value(Value::Num(n.as_f64().unwrap_or(0.0))),
        serde_json::Value::String(s) => {
            let t = s.trim();
            if let Some(f) = t.strip_prefix('=') {
                if !f.trim().is_empty() {
                    return Some(Input::Formula(f.trim().to_string()));
                }
            }
            // 纯数字的文字按数字写入（以 0 开头的编号、带 + 号的保持文字）
            let numeric = !t.is_empty() && t.parse::<f64>().is_ok() && !(t.len() > 1 && t.starts_with('0') && !t.starts_with("0.")) && !t.starts_with('+') && t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | 'e' | 'E'));
            if numeric {
                Input::Value(Value::Num(t.parse().unwrap()))
            } else {
                Input::Value(Value::Text(s.clone()))
            }
        }
        other => Input::Value(Value::Text(other.to_string())),
    })
}

/// 最终写入：新值（可带公式）、清空、只更新公式的缓存值
#[derive(Debug, Clone)]
enum Write {
    New { value: Option<Value>, formula: Option<String> },
    Clear,
    Recalc(Value),
}

struct ChartJob {
    op: usize,
    sheet: usize,
    kind: String,
    title: Option<String>,
    categories: (String, (u32, u32), (u32, u32)),
    series: Vec<(Option<String>, (String, (u32, u32), (u32, u32)))>,
    anchor: Option<(u32, u32)>,
    width: u32,
    height: u32,
}

fn valid_sheet_name(name: &str) -> std::result::Result<(), String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 31 {
        return Err("工作表名要 1–31 个字符".into());
    }
    if n.contains(['[', ']', ':', '*', '?', '/', '\\']) || n.starts_with('\'') || n.ends_with('\'') {
        return Err("工作表名不能包含 [ ] : * ? / \\，也不能以单引号开头或结尾".into());
    }
    if n.eq_ignore_ascii_case("history") {
        return Err("History 是 Excel 保留的名字".into());
    }
    Ok(())
}

/// 求值用的工作簿视图：原有单元格 + 本批写入（公式按需递归计算）
struct Book<'a> {
    wb: &'a Workbook,
    names: Vec<String>,
    overlay: &'a HashMap<usize, BTreeMap<(u32, u32), Option<Input>>>,
    base: RefCell<HashMap<usize, BTreeMap<(u32, u32), Cell>>>,
    /// 已算出的公式结果（None 表示含不支持的函数）
    computed: RefCell<HashMap<(usize, u32, u32), Option<Value>>>,
    visiting: RefCell<HashSet<(usize, u32, u32)>>,
    /// 需要重算的原有公式单元格
    recalc: RefCell<HashSet<(usize, u32, u32)>>,
}

impl<'a> Book<'a> {
    fn idx(&self, sheet: &str) -> Option<usize> {
        self.names.iter().position(|n| n.eq_ignore_ascii_case(sheet))
    }

    fn base_cell(&self, idx: usize, r: u32, c: u32) -> Option<Cell> {
        let mut base = self.base.borrow_mut();
        if !base.contains_key(&idx) {
            base.insert(idx, self.wb.cells(idx).unwrap_or_default());
        }
        base[&idx].get(&(r, c)).cloned()
    }

    fn formula_value(&self, idx: usize, r: u32, c: u32, f: &str) -> Option<Value> {
        let key = (idx, r, c);
        if let Some(v) = self.computed.borrow().get(&key) {
            return v.clone();
        }
        if !self.visiting.borrow_mut().insert(key) {
            // 循环引用：交给 Excel
            return None;
        }
        let v = formula::evaluate(f, &self.names[idx], self);
        self.visiting.borrow_mut().remove(&key);
        self.computed.borrow_mut().insert(key, v.clone());
        v
    }

    fn value_at(&self, idx: usize, r: u32, c: u32) -> Value {
        if let Some(entry) = self.overlay.get(&idx).and_then(|m| m.get(&(r, c))) {
            return match entry {
                None => Value::Empty,
                Some(Input::Value(v)) => v.clone(),
                Some(Input::Formula(f)) => self.formula_value(idx, r, c, f).unwrap_or(Value::Empty),
            };
        }
        match self.base_cell(idx, r, c) {
            Some(cell) => match (&cell.formula, self.recalc.borrow().contains(&(idx, r, c))) {
                (Some(f), true) => self.formula_value(idx, r, c, f).unwrap_or(cell.value),
                _ => cell.value,
            },
            None => Value::Empty,
        }
    }
}

impl<'a> Lookup for Book<'a> {
    fn cell(&self, sheet: &str, row: u32, col: u32) -> Value {
        match self.idx(sheet) {
            Some(i) => self.value_at(i, row, col),
            None => Value::Err("#REF!".into()),
        }
    }
    fn extent(&self, sheet: &str) -> (u32, u32) {
        let Some(i) = self.idx(sheet) else { return (0, 0) };
        let mut r = 0;
        let mut c = 0;
        if let Some(cells) = self.base.borrow_mut().entry(i).or_insert_with(|| self.wb.cells(i).unwrap_or_default()).keys().last() {
            r = cells.0;
        }
        for &(rr, cc) in self.base.borrow()[&i].keys() {
            c = c.max(cc);
            r = r.max(rr);
        }
        if let Some(m) = self.overlay.get(&i) {
            for &(rr, cc) in m.keys() {
                r = r.max(rr);
                c = c.max(cc);
            }
        }
        (r, c)
    }
    fn has_sheet(&self, sheet: &str) -> bool {
        self.idx(sheet).is_some()
    }
}

fn number_text(n: f64) -> String {
    formula::fmt_num(n)
}

fn cell_xml(p: &str, r: &str, style: Option<&str>, value: Option<&Value>, formula: Option<&str>) -> String {
    let s = style.map(|s| format!(r#" s="{s}""#)).unwrap_or_default();
    let f = formula.map(|f| format!("<{p}f>{}</{p}f>", xml::escape(f))).unwrap_or_default();
    match (value, formula.is_some()) {
        (None, false) | (Some(Value::Empty), false) => format!(r#"<{p}c r="{r}"{s}/>"#),
        (None, true) => format!(r#"<{p}c r="{r}"{s}>{f}</{p}c>"#),
        (Some(Value::Empty), true) => format!(r#"<{p}c r="{r}"{s}>{f}<{p}v>0</{p}v></{p}c>"#),
        (Some(Value::Num(n)), _) => format!(r#"<{p}c r="{r}"{s}>{f}<{p}v>{}</{p}v></{p}c>"#, number_text(*n)),
        (Some(Value::Bool(b)), _) => format!(r#"<{p}c r="{r}"{s} t="b">{f}<{p}v>{}</{p}v></{p}c>"#, if *b { 1 } else { 0 }),
        (Some(Value::Err(e)), _) => format!(r#"<{p}c r="{r}"{s} t="e">{f}<{p}v>{}</{p}v></{p}c>"#, xml::escape(e)),
        (Some(Value::Text(t)), true) => format!(r#"<{p}c r="{r}"{s} t="str">{f}<{p}v>{}</{p}v></{p}c>"#, xml::escape(t)),
        (Some(Value::Text(t)), false) => format!(r#"<{p}c r="{r}"{s} t="inlineStr"><{p}is><{p}t xml:space="preserve">{}</{p}t></{p}is></{p}c>"#, xml::escape(t)),
    }
}

/// 只更新公式单元格的缓存值：保留原 f 元素（共享公式的属性也不动）
fn recalc_cell(xml: &Xml, c: usize, p: &str, value: &Value) -> String {
    let mut open = xml.open_tag_expanded(c);
    let (t, v) = match value {
        Value::Num(n) => (None, number_text(*n)),
        Value::Empty => (None, "0".into()),
        Value::Bool(b) => (Some("b"), if *b { "1".into() } else { "0".into() }),
        Value::Err(e) => (Some("e"), xml::escape(e)),
        Value::Text(s) => (Some("str"), xml::escape(s)),
    };
    open = match t {
        Some(t) => xml::set_attr(&open, "t", t),
        None => xml::remove_attr(&open, "t"),
    };
    let f = child_local(xml, c, "f").map(|f| xml.outer(f)).unwrap_or("");
    let rest: String = xml.children(c).iter().filter(|&&ch| !matches!(local(xml.name(ch)), "f" | "v" | "is")).map(|&ch| xml.outer(ch)).collect();
    format!("{open}{f}<{p}v>{v}</{p}v>{rest}</{}>", xml.name(c))
}

/// 把写入合并进工作表 XML
fn rewrite_sheet(text: &str, writes: &BTreeMap<(u32, u32), Write>) -> Result<String> {
    if writes.is_empty() {
        return Ok(text.to_string());
    }
    let xml = Xml::parse(text)?;
    let root = xml.document_element().ok_or_else(|| EngineError::Format("工作表为空".into()))?;
    let p = prefix_of(xml.name(root));
    let data = child_local(&xml, root, "sheetData").ok_or_else(|| EngineError::Format("工作表缺少 sheetData".into()))?;
    let mut splice = Splice::new();

    let mut rows: Vec<(u32, usize)> = Vec::new();
    let mut row_no = 0;
    for row in children_local(&xml, data, "row") {
        row_no = xml.attr(row, "r").and_then(|v| v.parse().ok()).unwrap_or(row_no + 1);
        rows.push((row_no, row));
    }
    let mut by_row: BTreeMap<u32, Vec<(u32, &Write)>> = BTreeMap::new();
    for (&(r, c), w) in writes {
        by_row.entry(r).or_default().push((c, w));
    }

    let mut new_rows: Vec<(u32, String)> = Vec::new();
    for (r, items) in &by_row {
        let existing = rows.iter().find(|(n, _)| n == r).map(|&(_, node)| node);
        let mut cells: BTreeMap<u32, String> = BTreeMap::new();
        let mut trailing = String::new();
        let mut styles: HashMap<u32, String> = HashMap::new();
        let mut existing_nodes: HashMap<u32, usize> = HashMap::new();
        if let Some(row) = existing {
            let mut col = 0;
            for &child in xml.children(row) {
                if local(xml.name(child)) != "c" {
                    trailing.push_str(xml.outer(child));
                    continue;
                }
                col = xml.attr(child, "r").and_then(|v| parse_cell(&v)).map(|(_, c)| c).unwrap_or(col + 1);
                let outer = if xml.attr(child, "r").is_some() {
                    xml.outer(child).to_string()
                } else {
                    let open = xml::set_attr(xml.open_tag(child), "r", &cell_name(*r, col));
                    format!("{open}{}", &xml.outer(child)[xml.open_tag(child).len()..])
                };
                if let Some(s) = xml.attr(child, "s") {
                    styles.insert(col, s);
                }
                existing_nodes.insert(col, child);
                cells.insert(col, outer);
            }
        }
        for &(c, w) in items {
            let name = cell_name(*r, c);
            let style = styles.get(&c).map(String::as_str);
            match w {
                Write::Clear => match style {
                    Some(s) => {
                        cells.insert(c, format!(r#"<{p}c r="{name}" s="{s}"/>"#));
                    }
                    None => {
                        cells.remove(&c);
                    }
                },
                Write::New { value, formula } => {
                    cells.insert(c, cell_xml(&p, &name, style, value.as_ref(), formula.as_deref()));
                }
                Write::Recalc(v) => {
                    if let Some(&node) = existing_nodes.get(&c) {
                        cells.insert(c, recalc_cell(&xml, node, &p, v));
                    }
                }
            }
        }
        let body: String = cells.into_values().collect::<String>() + &trailing;
        match existing {
            Some(row) => {
                let open = xml::remove_attr(&xml::set_attr(&xml.open_tag_expanded(row), "r", &r.to_string()), "spans");
                let n = xml.node(row);
                splice.replace(n.start, n.end, format!("{open}{body}</{}>", xml.name(row)));
            }
            None => {
                if !body.is_empty() {
                    new_rows.push((*r, format!(r#"<{p}row r="{r}">{body}</{p}row>"#)));
                }
            }
        }
    }
    // 行号缺省的行补上 r，避免插入新行后顺序推断错位
    if !new_rows.is_empty() {
        for &(n, row) in &rows {
            if xml.attr(row, "r").is_none() && !by_row.contains_key(&n) {
                let node = xml.node(row);
                let open = xml::set_attr(xml.open_tag(row), "r", &n.to_string());
                splice.replace(node.start, node.open_end, open);
            }
        }
    }
    let dn = xml.node(data);
    if dn.self_closing {
        let body: String = new_rows.iter().map(|(_, x)| x.as_str()).collect();
        splice.replace(dn.start, dn.end, format!("{}{body}</{}>", xml.open_tag_expanded(data), xml.name(data)));
    } else {
        for (r, row_xml) in new_rows {
            let at = rows.iter().find(|(n, _)| *n > r).map(|&(_, node)| xml.node(node).start).unwrap_or(dn.close_start);
            splice.insert(at, row_xml);
        }
    }
    // 更新已用区域
    if let Some(dim) = child_local(&xml, root, "dimension") {
        let old = xml.attr(dim, "ref").and_then(|r| parse_range(&r));
        let mut bounds = old.unwrap_or(((u32::MAX, u32::MAX), (0, 0)));
        for (&(r, c), w) in writes {
            if matches!(w, Write::Clear) {
                continue;
            }
            bounds.0 = (bounds.0 .0.min(r), bounds.0 .1.min(c));
            bounds.1 = (bounds.1 .0.max(r), bounds.1 .1.max(c));
        }
        if bounds.1 .0 > 0 {
            let reference = if bounds.0 == bounds.1 { cell_name(bounds.0 .0, bounds.0 .1) } else { format!("{}:{}", cell_name(bounds.0 .0, bounds.0 .1), cell_name(bounds.1 .0, bounds.1 .1)) };
            let n = xml.node(dim);
            splice.replace(n.start, n.open_end, xml::set_attr(xml.open_tag(dim), "ref", &reference));
        }
    }
    splice.apply(text)
}

fn empty_sheet_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><dimension ref="A1"/><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetFormatPr defaultRowHeight="15"/><sheetData/><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/></worksheet>"#
        .to_string()
}

impl Workbook {
    pub fn apply(&mut self, ops: &[SheetOp]) -> Result<ApplyReport> {
        if ops.is_empty() {
            return Err(EngineError::Invalid("没有要执行的操作".into()));
        }
        let mut overlay: HashMap<usize, BTreeMap<(u32, u32), Option<Input>>> = HashMap::new();
        let mut changes: Vec<Change> = Vec::new();
        let mut charts: Vec<ChartJob> = Vec::new();
        let mut wb_text = self.wb_text.clone();
        let mut structure_changed = false;

        for (i, op) in ops.iter().enumerate() {
            let name = op.name();
            let wrap = |e: EngineError| match e {
                EngineError::Op { .. } => e,
                other => op_err(i, name, other.to_string()),
            };
            match op {
                SheetOp::Set { sheet, cell, value } => {
                    let idx = self.sheet_index(sheet.as_deref()).map_err(wrap)?;
                    let (r, c) = parse_cell(&split_sheet(cell).1).ok_or_else(|| op_err(i, name, format!("单元格“{cell}”格式不对，示例：B3")))?;
                    let before = self.cells(idx)?.get(&(r, c)).map(|x| display_cell(x));
                    overlay.entry(idx).or_default().insert((r, c), to_input(value));
                    changes.push(Change { op: i, kind: name.into(), target: format!("{}!{}", self.sheets[idx].name, cell_name(r, c)), before, after: Some(clip(&json_text(value), 200)) });
                }
                SheetOp::SetRange { sheet, start, values } => {
                    let idx = self.sheet_index(sheet.as_deref()).map_err(wrap)?;
                    let (r0, c0) = parse_cell(&split_sheet(start).1).ok_or_else(|| op_err(i, name, format!("起始单元格“{start}”格式不对，示例：A1")))?;
                    if values.is_empty() {
                        return Err(op_err(i, name, "values 为空"));
                    }
                    let width = values.iter().map(Vec::len).max().unwrap_or(0) as u32;
                    let entry = overlay.entry(idx).or_default();
                    for (dr, row) in values.iter().enumerate() {
                        for (dc, v) in row.iter().enumerate() {
                            entry.insert((r0 + dr as u32, c0 + dc as u32), to_input(v));
                        }
                    }
                    let end = cell_name(r0 + values.len() as u32 - 1, c0 + width.max(1) - 1);
                    let preview: Vec<String> = values.iter().take(3).map(|row| row.iter().map(json_text).collect::<Vec<_>>().join(" | ")).collect();
                    changes.push(Change { op: i, kind: name.into(), target: format!("{}!{}:{end}", self.sheets[idx].name, cell_name(r0, c0)), before: None, after: Some(clip(&preview.join("\n"), 300)) });
                }
                SheetOp::Clear { sheet, range } => {
                    let idx = self.sheet_index(sheet.as_deref()).map_err(wrap)?;
                    let ((r1, c1), (r2, c2)) = parse_range(&split_sheet(range).1).ok_or_else(|| op_err(i, name, format!("区域“{range}”格式不对，示例：A1:C10")))?;
                    let cells = self.cells(idx)?;
                    let entry = overlay.entry(idx).or_default();
                    let mut n = 0;
                    for (&(r, c), _) in cells.range((r1, 0)..=(r2, u32::MAX)).filter(|((_, c), _)| *c >= c1 && *c <= c2) {
                        entry.insert((r, c), None);
                        n += 1;
                    }
                    let pending: Vec<(u32, u32)> = entry.keys().copied().filter(|&(r, c)| r >= r1 && r <= r2 && c >= c1 && c <= c2).collect();
                    for k in pending {
                        entry.insert(k, None);
                    }
                    changes.push(Change { op: i, kind: name.into(), target: format!("{}!{range}", self.sheets[idx].name), before: Some(format!("{n} 个单元格")), after: None });
                }
                SheetOp::AddSheet { name: sheet_name, values } => {
                    valid_sheet_name(sheet_name).map_err(|e| op_err(i, name, e))?;
                    let sheet_name = sheet_name.trim().to_string();
                    if self.sheets.iter().any(|s| s.name.eq_ignore_ascii_case(&sheet_name)) {
                        return Err(op_err(i, name, format!("已有名为“{sheet_name}”的工作表")));
                    }
                    let part = self.pkg.unused_name("xl/worksheets/sheet", "xml");
                    self.pkg.write(&part, empty_sheet_xml());
                    self.pkg.content_type_override(&part, CT_WORKSHEET)?;
                    let rid = self.pkg.add_rel(&self.wb_part, REL_WORKSHEET, &Package::relative(&self.wb_part, &part), false)?;
                    let sheet_id = self.sheets.iter().map(|s| s.sheet_id).max().unwrap_or(0) + 1;
                    let entry = {
                        let xml = Xml::parse(&wb_text)?;
                        let root = xml.document_element().unwrap();
                        // 有的程序（openpyxl）把关系命名空间声明在每个 sheet 元素上而不是根元素上
                        let ns = if xml.open_tag(root).contains(&format!("xmlns:r=\"{NS_R}\"")) { String::new() } else { format!(r#" xmlns:r="{NS_R}""#) };
                        let p = prefix_of(xml.name(root));
                        let list = child_local(&xml, root, "sheets").unwrap();
                        let el = format!(r#"<{p}sheet name="{}" sheetId="{sheet_id}" r:id="{rid}"{ns}/>"#, xml::escape_attr(&sheet_name));
                        let n = xml.node(list);
                        let mut splice = Splice::new();
                        if n.self_closing {
                            splice.replace(n.start, n.end, format!("{}{el}</{}>", xml.open_tag_expanded(list), xml.name(list)));
                        } else {
                            splice.insert(n.close_start, el);
                        }
                        splice.apply(&wb_text)?
                    };
                    wb_text = entry;
                    structure_changed = true;
                    self.sheets.push(SheetMeta { name: sheet_name.clone(), part, rid, sheet_id, hidden: false });
                    let idx = self.sheets.len() - 1;
                    if let Some(values) = values {
                        let entry = overlay.entry(idx).or_default();
                        for (r, row) in values.iter().enumerate() {
                            for (c, v) in row.iter().enumerate() {
                                entry.insert((r as u32 + 1, c as u32 + 1), to_input(v));
                            }
                        }
                    }
                    changes.push(Change { op: i, kind: name.into(), target: sheet_name, before: None, after: values.as_ref().map(|v| format!("{} 行数据", v.len())) });
                }
                SheetOp::RenameSheet { sheet, name: new_name } => {
                    let idx = self.sheet_index(Some(sheet)).map_err(wrap)?;
                    valid_sheet_name(new_name).map_err(|e| op_err(i, name, e))?;
                    let new_name = new_name.trim().to_string();
                    if self.sheets.iter().enumerate().any(|(j, s)| j != idx && s.name.eq_ignore_ascii_case(&new_name)) {
                        return Err(op_err(i, name, format!("已有名为“{new_name}”的工作表")));
                    }
                    let updated = {
                        let xml = Xml::parse(&wb_text)?;
                        let root = xml.document_element().unwrap();
                        let list = child_local(&xml, root, "sheets").unwrap();
                        let rid = &self.sheets[idx].rid;
                        let node = children_local(&xml, list, "sheet").find(|&s| xml.open_tag(s).contains(&format!("\"{rid}\""))).ok_or_else(|| op_err(i, name, "workbook.xml 里找不到这个工作表"))?;
                        let mut splice = Splice::new();
                        splice.replace(xml.node(node).start, xml.node(node).open_end, xml::set_attr(xml.open_tag(node), "name", &new_name));
                        splice.apply(&wb_text)?
                    };
                    wb_text = updated;
                    structure_changed = true;
                    let old = std::mem::replace(&mut self.sheets[idx].name, new_name.clone());
                    changes.push(Change { op: i, kind: name.into(), target: old, before: None, after: Some(new_name) });
                }
                SheetOp::AddChart { sheet, chart_type, title, categories, series, anchor, width, height } => {
                    let idx = self.sheet_index(sheet.as_deref()).map_err(wrap)?;
                    let kind = match chart_type.to_lowercase().as_str() {
                        "column" | "col" | "柱形图" | "柱状图" => "column",
                        "bar" | "条形图" => "bar",
                        "line" | "折线图" => "line",
                        "pie" | "饼图" => "pie",
                        other => return Err(op_err(i, name, format!("不支持的图表类型“{other}”，可选 column / bar / line / pie"))),
                    };
                    if series.is_empty() {
                        return Err(op_err(i, name, "至少需要一个数据系列 series"));
                    }
                    let resolve = |text: &str| -> Result<(String, (u32, u32), (u32, u32))> {
                        let (s, r) = split_sheet(text);
                        let sheet_name = match s {
                            Some(s) => self.sheets[self.sheet_index(Some(&s))?].name.clone(),
                            None => self.sheets[idx].name.clone(),
                        };
                        let (a, b) = parse_range(&r).ok_or_else(|| EngineError::Invalid(format!("区域“{text}”格式不对，示例：Sheet1!B2:B10")))?;
                        Ok((sheet_name, a, b))
                    };
                    let cats = resolve(categories).map_err(wrap)?;
                    let mut ser = Vec::new();
                    for s in series {
                        ser.push((s.name.clone(), resolve(&s.values).map_err(wrap)?));
                    }
                    let anchor = match anchor {
                        Some(a) => Some(parse_cell(a).ok_or_else(|| op_err(i, name, format!("anchor“{a}”格式不对，示例：F2")))?),
                        None => None,
                    };
                    changes.push(Change { op: i, kind: name.into(), target: self.sheets[idx].name.clone(), before: None, after: Some(format!("{kind} 图表{}", title.as_ref().map(|t| format!("《{t}》")).unwrap_or_default())) });
                    charts.push(ChartJob { op: i, sheet: idx, kind: kind.into(), title: title.clone(), categories: cats, series: ser, anchor, width: width.unwrap_or(8).clamp(3, 30), height: height.unwrap_or(15).clamp(5, 60) });
                }
            }
        }

        // 计算：本批写入的公式，以及依赖了改动单元格的原有公式
        let names: Vec<String> = self.sheets.iter().map(|s| s.name.clone()).collect();
        let book = Book { wb: self, names, overlay: &overlay, base: RefCell::new(HashMap::new()), computed: RefCell::new(HashMap::new()), visiting: RefCell::new(HashSet::new()), recalc: RefCell::new(HashSet::new()) };
        let dependents = self.dependents(&overlay)?;
        book.recalc.borrow_mut().extend(dependents.iter().copied());
        let mut writes: HashMap<usize, BTreeMap<(u32, u32), Write>> = HashMap::new();
        let mut formulas_written = false;
        for (&idx, cells) in &overlay {
            for (&(r, c), input) in cells {
                let w = match input {
                    None => Write::Clear,
                    Some(Input::Value(v)) => Write::New { value: Some(v.clone()), formula: None },
                    Some(Input::Formula(f)) => {
                        formulas_written = true;
                        Write::New { value: book.formula_value(idx, r, c, f), formula: Some(f.clone()) }
                    }
                };
                writes.entry(idx).or_default().insert((r, c), w);
            }
        }
        for &(idx, r, c) in &dependents {
            if overlay.get(&idx).map(|m| m.contains_key(&(r, c))).unwrap_or(false) {
                continue;
            }
            let Some(cell) = book.base_cell(idx, r, c) else { continue };
            let Some(f) = &cell.formula else { continue };
            if let Some(v) = book.formula_value(idx, r, c, f) {
                if v != cell.value {
                    writes.entry(idx).or_default().insert((r, c), Write::Recalc(v));
                }
            }
        }
        // 写图表前先取出缓存数据
        let mut chart_xml = Vec::new();
        for job in &charts {
            let read = |range: &(String, (u32, u32), (u32, u32))| -> Vec<Value> {
                let (sheet, (r1, c1), (r2, c2)) = range;
                let mut out = Vec::new();
                for r in *r1..=*r2 {
                    for c in *c1..=*c2 {
                        out.push(book.cell(sheet, r, c));
                    }
                }
                out
            };
            let cats = read(&job.categories);
            let series: Vec<(Option<String>, Vec<Value>)> = job.series.iter().map(|(n, r)| (n.clone(), read(r))).collect();
            chart_xml.push(chart_space(job, &cats, &series));
        }
        drop(book);

        let touched_cells = !writes.is_empty();
        for (idx, w) in &writes {
            let part = self.sheets[*idx].part.clone();
            let text = self.pkg.read_str(&part)?;
            let updated = rewrite_sheet(&text, w)?;
            Xml::parse(&updated).map_err(|e| EngineError::Format(format!("内部错误：工作表改写后无法解析（{e}），文件未改动")))?;
            self.pkg.write(&part, updated);
        }
        for (job, xml_text) in charts.iter().zip(chart_xml) {
            self.insert_chart(job, xml_text).map_err(|e| op_err(job.op, "add_chart", e.to_string()))?;
        }
        if touched_cells {
            wb_text = force_full_calc(&wb_text)?;
            if formulas_written || !dependents.is_empty() || writes.values().any(|w| w.values().any(|x| matches!(x, Write::Clear))) {
                self.drop_calc_chain()?;
            }
        }
        if structure_changed || wb_text != self.wb_text {
            self.wb_text = wb_text;
            self.pkg.write(&self.wb_part.clone(), self.wb_text.clone());
        }
        self.cache.borrow_mut().clear();
        Ok(ApplyReport { changes, revision: self.revision()? })
    }

    /// 依赖本批改动的原有公式单元格（传递闭包）
    fn dependents(&self, overlay: &HashMap<usize, BTreeMap<(u32, u32), Option<Input>>>) -> Result<HashSet<(usize, u32, u32)>> {
        let mut formulas: Vec<(usize, u32, u32, Vec<(usize, (u32, u32), (u32, u32))>)> = Vec::new();
        let mut total = 0;
        for idx in 0..self.sheets.len() {
            let cells = match self.cells(idx) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (&(r, c), cell) in &cells {
                let Some(f) = &cell.formula else { continue };
                total += 1;
                if total > 50_000 {
                    // 公式太多时不做依赖重算，交给 Excel 打开时计算
                    return Ok(HashSet::new());
                }
                let refs = formula::references(f)
                    .into_iter()
                    .filter_map(|(sheet, a, b)| {
                        let target = match sheet {
                            Some(s) => self.sheets.iter().position(|m| m.name.eq_ignore_ascii_case(&s))?,
                            None => idx,
                        };
                        Some((target, a, b))
                    })
                    .collect();
                formulas.push((idx, r, c, refs));
            }
        }
        let mut changed: HashSet<(usize, u32, u32)> = overlay.iter().flat_map(|(&i, m)| m.keys().map(move |&(r, c)| (i, r, c))).collect();
        let mut out = HashSet::new();
        loop {
            let mut grew = false;
            for (idx, r, c, refs) in &formulas {
                let key = (*idx, *r, *c);
                if out.contains(&key) {
                    continue;
                }
                let hit = refs.iter().any(|(s, (r1, c1), (r2, c2))| changed.iter().any(|&(ci, cr, cc)| ci == *s && cr >= *r1 && cr <= *r2 && cc >= *c1 && cc <= *c2));
                if hit {
                    out.insert(key);
                    changed.insert(key);
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        Ok(out)
    }

    fn drop_calc_chain(&mut self) -> Result<()> {
        let rels = self.pkg.rels(&self.wb_part)?;
        if let Some(rel) = rels.iter().find(|r| r.kind.ends_with("/calcChain")) {
            let part = Package::resolve(&self.wb_part, &rel.target);
            self.pkg.remove(&part);
            self.pkg.remove_rel(&self.wb_part.clone(), &rel.id)?;
            self.pkg.remove_content_type_override(&part)?;
        }
        Ok(())
    }

    fn insert_chart(&mut self, job: &ChartJob, chart: String) -> Result<()> {
        let sheet_part = self.sheets[job.sheet].part.clone();
        let chart_part = self.pkg.unused_name("xl/charts/chart", "xml");
        self.pkg.write(&chart_part, chart);
        self.pkg.content_type_override(&chart_part, CT_CHART)?;

        let existing = self.pkg.target_of(&sheet_part, "/drawing")?;
        let (drawing_part, new_drawing) = match existing {
            Some(d) if self.pkg.has(&d) => (d, false),
            _ => (self.pkg.unused_name("xl/drawings/drawing", "xml"), true),
        };
        let chart_rid = if new_drawing {
            self.pkg.write(&drawing_part, r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"></xdr:wsDr>"#);
            self.pkg.content_type_override(&drawing_part, CT_DRAWING)?;
            self.pkg.add_rel(&drawing_part, REL_CHART, &Package::relative(&drawing_part, &chart_part), false)?
        } else {
            self.pkg.add_rel(&drawing_part, REL_CHART, &Package::relative(&drawing_part, &chart_part), false)?
        };
        // 位置：默认放在已用区域右侧
        let (row, col) = match job.anchor {
            Some((r, c)) => (r - 1, c - 1),
            None => {
                let cells = self.cells(job.sheet).unwrap_or_default();
                let max_col = cells.keys().map(|k| k.1).max().unwrap_or(0);
                (1, max_col + 1)
            }
        };
        let drawing_text = self.pkg.read_str(&drawing_part)?;
        let next_id = {
            let xml = Xml::parse(&drawing_text)?;
            let root = xml.document_element().unwrap();
            xml.descendants(root).into_iter().filter(|&d| local(xml.name(d)) == "cNvPr").filter_map(|d| xml.attr(d, "id")?.parse::<u32>().ok()).max().unwrap_or(1) + 1
        };
        let prefix = {
            let xml = Xml::parse(&drawing_text)?;
            prefix_of(xml.name(xml.document_element().unwrap()))
        };
        let x = &prefix;
        let anchor = format!(
            r#"<{x}twoCellAnchor><{x}from><{x}col>{col}</{x}col><{x}colOff>0</{x}colOff><{x}row>{row}</{x}row><{x}rowOff>0</{x}rowOff></{x}from><{x}to><{x}col>{}</{x}col><{x}colOff>0</{x}colOff><{x}row>{}</{x}row><{x}rowOff>0</{x}rowOff></{x}to><{x}graphicFrame macro=""><{x}nvGraphicFramePr><{x}cNvPr id="{next_id}" name="Chart {next_id}"/><{x}cNvGraphicFramePr/></{x}nvGraphicFramePr><{x}xfrm><a:off x="0" y="0" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"/><a:ext cx="0" cy="0" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"/></{x}xfrm><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:r="{NS_R}" r:id="{chart_rid}"/></a:graphicData></a:graphic></{x}graphicFrame><{x}clientData/></{x}twoCellAnchor>"#,
            col + job.width,
            row + job.height
        );
        let root_name = {
            let xml = Xml::parse(&drawing_text)?;
            xml.name(xml.document_element().unwrap()).to_string()
        };
        self.pkg.write(&drawing_part, insert_before_close(&drawing_text, &root_name, &anchor)?);

        if new_drawing {
            let rid = self.pkg.add_rel(&sheet_part, REL_DRAWING, &Package::relative(&sheet_part, &drawing_part), false)?;
            let text = self.pkg.read_str(&sheet_part)?;
            let updated = {
                let xml = Xml::parse(&text)?;
                let root = xml.document_element().unwrap();
                let p = prefix_of(xml.name(root));
                let ns = if xml.open_tag(root).contains(&format!("xmlns:r=\"{NS_R}\"")) { String::new() } else { format!(r#" xmlns:r="{NS_R}""#) };
                let el = format!(r#"<{p}drawing r:id="{rid}"{ns}/>"#);
                const AFTER: &[&str] = &["legacyDrawing", "legacyDrawingHF", "drawingHF", "picture", "oleObjects", "controls", "webPublishItems", "tableParts", "extLst"];
                let at = xml.children(root).iter().copied().find(|&c| AFTER.contains(&local(xml.name(c)))).map(|c| xml.node(c).start).unwrap_or(xml.node(root).close_start);
                let mut splice = Splice::new();
                splice.insert(at, el);
                splice.apply(&text)?
            };
            self.pkg.write(&sheet_part, updated);
        }
        Ok(())
    }

    pub fn save(&self) -> Result<Vec<u8>> {
        self.pkg.save()
    }
}

fn force_full_calc(wb: &str) -> Result<String> {
    let xml = Xml::parse(wb)?;
    let root = xml.document_element().unwrap();
    let p = prefix_of(xml.name(root));
    let mut splice = Splice::new();
    match child_local(&xml, root, "calcPr") {
        Some(calc) => {
            if xml.attr(calc, "fullCalcOnLoad").as_deref() == Some("1") {
                return Ok(wb.to_string());
            }
            let n = xml.node(calc);
            splice.replace(n.start, n.open_end, xml::set_attr(xml.open_tag(calc), "fullCalcOnLoad", "1"));
        }
        None => {
            const AFTER: &[&str] = &["oleSize", "customWorkbookViews", "pivotCaches", "smartTagPr", "smartTagTypes", "webPublishing", "fileRecoveryPr", "webPublishObjects", "extLst"];
            let at = xml.children(root).iter().copied().find(|&c| AFTER.contains(&local(xml.name(c)))).map(|c| xml.node(c).start).unwrap_or(xml.node(root).close_start);
            splice.insert(at, format!(r#"<{p}calcPr fullCalcOnLoad="1"/>"#));
        }
    }
    splice.apply(wb)
}

fn json_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "（清空）".into(),
        other => other.to_string(),
    }
}

fn display_cell(c: &Cell) -> String {
    let v = match &c.value {
        Value::Num(n) => formula::fmt_num(*n),
        Value::Text(t) => t.clone(),
        Value::Bool(b) => b.to_string().to_uppercase(),
        Value::Err(e) => e.clone(),
        Value::Empty => String::new(),
    };
    match &c.formula {
        Some(f) => format!("{v}〔={f}〕"),
        None => v,
    }
}

fn abs_ref(range: &(String, (u32, u32), (u32, u32))) -> String {
    let (sheet, (r1, c1), (r2, c2)) = range;
    let a = format!("${}${r1}", index_to_col(*c1));
    let b = format!("${}${r2}", index_to_col(*c2));
    let body = if a == b { a } else { format!("{a}:{b}") };
    format!("{}!{body}", quote_sheet(sheet))
}

fn chart_space(job: &ChartJob, cats: &[Value], series: &[(Option<String>, Vec<Value>)]) -> String {
    let str_cache = |vals: &[Value]| -> String {
        let pts: String = vals.iter().enumerate().map(|(i, v)| format!(r#"<c:pt idx="{i}"><c:v>{}</c:v></c:pt>"#, xml::escape(&value_text(v)))).collect();
        format!(r#"<c:strCache><c:ptCount val="{}"/>{pts}</c:strCache>"#, vals.len())
    };
    let num_cache = |vals: &[Value]| -> String {
        let pts: String = vals
            .iter()
            .enumerate()
            .filter_map(|(i, v)| match v {
                Value::Num(n) => Some(format!(r#"<c:pt idx="{i}"><c:v>{}</c:v></c:pt>"#, formula::fmt_num(*n))),
                _ => None,
            })
            .collect();
        format!(r#"<c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="{}"/>{pts}</c:numCache>"#, vals.len())
    };
    let cat_xml = format!("<c:cat><c:strRef><c:f>{}</c:f>{}</c:strRef></c:cat>", xml::escape(&abs_ref(&job.categories)), str_cache(cats));
    let mut sers = String::new();
    for (i, ((name, range), (_, vals))) in job.series.iter().zip(series.iter()).enumerate() {
        let tx = match name {
            Some(n) if !n.trim().is_empty() => match split_sheet(n) {
                // 系列名也可以是单元格引用
                (Some(s), r) if parse_cell(&r).is_some() => format!("<c:tx><c:strRef><c:f>{}!{}</c:f></c:strRef></c:tx>", xml::escape(&quote_sheet(&s)), r.replace('$', "")),
                _ => format!("<c:tx><c:v>{}</c:v></c:tx>", xml::escape(n)),
            },
            _ => String::new(),
        };
        let val = format!("<c:val><c:numRef><c:f>{}</c:f>{}</c:numRef></c:val>", xml::escape(&abs_ref(range)), num_cache(vals));
        let body = match job.kind.as_str() {
            "line" => format!(r#"{tx}<c:marker><c:symbol val="circle"/><c:size val="5"/></c:marker>{cat_xml}{val}<c:smooth val="0"/>"#),
            "pie" => format!("{tx}{cat_xml}{val}"),
            _ => format!(r#"{tx}<c:invertIfNegative val="0"/>{cat_xml}{val}"#),
        };
        sers.push_str(&format!(r#"<c:ser><c:idx val="{i}"/><c:order val="{i}"/>{body}</c:ser>"#));
    }
    let axes_ids = r#"<c:axId val="500000001"/><c:axId val="500000002"/>"#;
    let plot = match job.kind.as_str() {
        "line" => format!(r#"<c:lineChart><c:grouping val="standard"/><c:varyColors val="0"/>{sers}<c:marker val="1"/>{axes_ids}</c:lineChart>"#),
        "pie" => format!(r#"<c:pieChart><c:varyColors val="1"/>{sers}<c:firstSliceAng val="0"/></c:pieChart>"#),
        kind => format!(
            r#"<c:barChart><c:barDir val="{}"/><c:grouping val="clustered"/><c:varyColors val="0"/>{sers}<c:gapWidth val="150"/>{axes_ids}</c:barChart>"#,
            if kind == "bar" { "bar" } else { "col" }
        ),
    };
    let axes = if job.kind == "pie" {
        String::new()
    } else {
        let (cat_pos, val_pos) = if job.kind == "bar" { ("l", "b") } else { ("b", "l") };
        format!(
            r#"<c:catAx><c:axId val="500000001"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="{cat_pos}"/><c:numFmt formatCode="General" sourceLinked="1"/><c:tickLblPos val="nextTo"/><c:crossAx val="500000002"/><c:crosses val="autoZero"/><c:auto val="1"/><c:lblAlgn val="ctr"/><c:lblOffset val="100"/></c:catAx><c:valAx><c:axId val="500000002"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="{val_pos}"/><c:majorGridlines/><c:numFmt formatCode="General" sourceLinked="1"/><c:tickLblPos val="nextTo"/><c:crossAx val="500000001"/><c:crosses val="autoZero"/><c:crossBetween val="between"/></c:valAx>"#
        )
    };
    let title = match &job.title {
        Some(t) if !t.trim().is_empty() => format!(
            r#"<c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr b="1"/></a:pPr><a:r><a:rPr lang="zh-CN" b="1"/><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title><c:autoTitleDeleted val="0"/>"#,
            xml::escape(t)
        ),
        _ => r#"<c:autoTitleDeleted val="1"/>"#.into(),
    };
    let legend = if job.series.len() > 1 || job.kind == "pie" { r#"<c:legend><c:legendPos val="b"/><c:overlay val="0"/></c:legend>"# } else { "" };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="{NS_R}"><c:roundedCorners val="0"/><c:chart>{title}<c:plotArea><c:layout/>{plot}{axes}</c:plotArea>{legend}<c:plotVisOnly val="1"/><c:dispBlanksAs val="gap"/></c:chart></c:chartSpace>"#
    )
}

fn value_text(v: &Value) -> String {
    match v {
        Value::Num(n) => formula::fmt_num(*n),
        Value::Text(t) => t.clone(),
        Value::Bool(b) => b.to_string().to_uppercase(),
        Value::Err(e) => e.clone(),
        Value::Empty => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_xlsxwriter::{Format, Formula, Workbook as XWorkbook};

    fn sample() -> Vec<u8> {
        let mut wb = XWorkbook::new();
        let ws = wb.add_worksheet().set_name("Orders").unwrap();
        let date = Format::new().set_num_format("yyyy-mm-dd");
        ws.write_row(0, 0, ["地区", "金额", "类别", "日期"]).unwrap();
        let rows = [("华东", 100.0, "A"), ("华北", 50.0, "B"), ("华东", 30.0, "A"), ("西南", 20.0, "C")];
        for (i, (region, amount, cat)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            ws.write(r, 0, *region).unwrap();
            ws.write(r, 1, *amount).unwrap();
            ws.write(r, 2, *cat).unwrap();
            ws.write_number_with_format(r, 3, 46000.0 + i as f64, &date).unwrap();
        }
        ws.write_formula(5, 1, Formula::new("=SUM(B2:B5)").set_result("200")).unwrap();
        ws.write(5, 0, "合计").unwrap();
        wb.add_worksheet().set_name("备注").unwrap().write(0, 0, "不改动").unwrap();
        wb.save_to_buffer().unwrap()
    }

    #[test]
    fn reads_values_formulas_and_dates() {
        let wb = Workbook::open(sample()).unwrap();
        assert_eq!(wb.sheets.len(), 2);
        let view = wb.read(Some("orders"), None, 50).unwrap();
        assert_eq!(view.dimension, "A1:D6");
        let text = format_view(&view, 40);
        assert!(text.contains("200〔=SUM(B2:B5)〕"), "{text}");
        assert!(text.contains("2025-12-09"), "{text}");
        let info = wb.info().unwrap();
        assert_eq!(info.sheets[0].formulas, 1);
    }

    #[test]
    fn writes_formulas_recalcs_dependents_and_adds_chart() {
        let original = sample();
        let mut wb = Workbook::open(original.clone()).unwrap();
        let ops: Vec<SheetOp> = serde_json::from_str(
            r#"[
                {"op":"set","sheet":"Orders","cell":"B3","value":80},
                {"op":"add_sheet","name":"Summary","values":[["地区","合计"],["华东","=SUMIF(Orders!A2:A5,A2,Orders!B2:B5)"],["华北","=SUMIF(Orders!A2:A5,A3,Orders!B2:B5)"],["总计","=SUM(B2:B3)"]]},
                {"op":"add_chart","sheet":"Summary","type":"column","title":"各地区金额","categories":"Summary!A2:A3","series":[{"name":"合计","values":"Summary!B2:B3"}]},
                {"op":"set","sheet":"Orders","cell":"E1","value":"=XNPV(1,2)"}
            ]"#,
        )
        .unwrap();
        let report = wb.apply(&ops).unwrap();
        assert_eq!(report.changes.len(), 4);
        let saved = wb.save().unwrap();
        let wb2 = Workbook::open(saved.clone()).unwrap();
        let summary = wb2.read(Some("Summary"), None, 20).unwrap();
        let text = format_view(&summary, 40);
        assert!(text.contains("130〔=SUMIF(Orders!A2:A5,A2,Orders!B2:B5)〕"), "{text}");
        assert!(text.contains("80〔"), "{text}");
        assert!(text.contains("210〔=SUM(B2:B3)〕"), "{text}");
        // 原有的 SUM 公式依赖 B3，缓存值随之更新
        let orders = format_view(&wb2.read(Some("Orders"), None, 20).unwrap(), 40);
        assert!(orders.contains("230〔=SUM(B2:B5)〕"), "{orders}");
        // 不支持的函数只写公式
        assert!(orders.contains("〔=XNPV(1,2)〕"), "{orders}");
        assert_eq!(wb2.info().unwrap().sheets[2].charts, 1);
        // 没改的工作表逐字节保留
        let raw = |b: &[u8], n: &str| {
            let mut a = zip::ZipArchive::new(std::io::Cursor::new(b)).unwrap();
            let f = a.by_name(n).unwrap();
            (f.crc32(), f.compressed_size())
        };
        assert_eq!(raw(&original, "xl/worksheets/sheet2.xml"), raw(&saved, "xl/worksheets/sheet2.xml"));
        assert_eq!(raw(&original, "xl/styles.xml"), raw(&saved, "xl/styles.xml"));
    }

    #[test]
    fn clear_and_rename_and_errors() {
        let mut wb = Workbook::open(sample()).unwrap();
        let ops: Vec<SheetOp> = serde_json::from_str(r#"[{"op":"clear","sheet":"Orders","range":"A5:D5"},{"op":"rename_sheet","sheet":"备注","name":"Notes"}]"#).unwrap();
        wb.apply(&ops).unwrap();
        let wb2 = Workbook::open(wb.save().unwrap()).unwrap();
        assert_eq!(wb2.sheets[1].name, "Notes");
        let text = format_view(&wb2.read(Some("Orders"), None, 20).unwrap(), 40);
        assert!(!text.contains("西南"), "{text}");
        assert!(text.contains("180〔=SUM(B2:B5)〕"), "{text}");

        let mut wb = Workbook::open(sample()).unwrap();
        let bad: Vec<SheetOp> = serde_json::from_str(r#"[{"op":"set","sheet":"Nope","cell":"A1","value":1}]"#).unwrap();
        let err = wb.apply(&bad).unwrap_err().to_string();
        assert!(err.contains("找不到工作表“Nope”") && err.contains("Orders"), "{err}");
    }
}
