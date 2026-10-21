//! PPT：页面检查、版面审查、分阶段做 PPT（deck start → page → build → replace）
//!
//! 工作目录结构：style.json、outline.json、pages/01.json…、build.json（上次组装的输出路径）

use std::path::{Path, PathBuf};

use officepilot_engine::build::deck::{self, DeckSpec, Finding, Outline, Page, Style};
use officepilot_engine::pptx;
use serde_json::{json, Value};

use crate::io::{json_input, read_file, write_atomic};
use crate::{Done, Fail, Out};

fn parse<T: serde::de::DeserializeOwned>(value: Value, what: &str) -> Result<T, Fail> {
    serde_json::from_value(value).map_err(|e| Fail::new(format!("{what} 格式不对：{e}")).hint("格式见 officepilot guide deck"))
}

fn findings_text(findings: &[Finding]) -> String {
    findings.iter().map(|f| format!("- [{}] {}", if f.level == "error" { "错误" } else { "提示" }, f.message)).collect::<Vec<_>>().join("\n")
}

fn errors(findings: &[Finding]) -> usize {
    findings.iter().filter(|f| f.level == "error").count()
}

fn load_style(path: Option<&str>) -> Result<Style, Fail> {
    match path {
        Some(p) => {
            let style: Style = parse(json_input(p)?, "风格表")?;
            style.validate().map_err(Fail::new)?;
            Ok(style)
        }
        None => Ok(Style::default()),
    }
}

fn base_of(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    if p.exists() { p.parent().map(Path::to_path_buf) } else { std::env::current_dir().ok() }
}

pub fn check(spec: &str, style: Option<&str>, outline: Option<&str>, page_no: Option<usize>) -> Out {
    let value = json_input(spec)?;
    // 大纲：只有 pages 里放标题
    let is_outline = value.get("pages").and_then(Value::as_array).map(|a| a.iter().all(|p| p.get("title").is_some() && p.get("layout").is_none() && p.get("bullets").is_none())).unwrap_or(false) && value.get("style").is_none();
    if is_outline {
        let o: Outline = parse(value, "大纲")?;
        let f = deck::check_outline(&o);
        let n = errors(&f);
        let summary = if f.is_empty() { format!("大纲检查通过：{} 页", o.pages.len()) } else { format!("大纲 {} 页，{} 个问题：\n{}", o.pages.len(), f.len(), findings_text(&f)) };
        return Ok(Done { summary, data: json!({ "pages": o.pages.len(), "findings": f }), failed: n > 0 });
    }
    let style = load_style(style)?;
    let page: Page = parse(value, "页面规格")?;
    let outline_v: Option<Outline> = match outline {
        Some(o) => Some(parse(json_input(o)?, "大纲")?),
        None => None,
    };
    let f = deck::check_page(&page, &style, outline_v.as_ref().zip(page_no).map(|(o, n)| (o, n)), base_of(spec).as_deref());
    let n = errors(&f);
    let summary = if f.is_empty() { "检查通过：没有溢出、重叠或超出页面".to_string() } else { format!("{} 个问题（{} 个必须修改）：\n{}", f.len(), n, findings_text(&f)) };
    Ok(Done { summary, data: json!({ "findings": f, "errors": n }), failed: n > 0 })
}

pub fn audit(file: &str) -> Out {
    let deck_file = pptx::Deck::open(read_file(file)?)?;
    let slides = deck_file.slides()?;
    let found = deck::audit(&slides, deck_file.size);
    let n = found.iter().filter(|(_, f)| f.level == "error").count();
    let list: Vec<Value> = found.iter().map(|(s, f)| json!({ "slide": s, "level": f.level, "code": f.code, "message": f.message })).collect();
    let summary = if found.is_empty() {
        format!("{} 页，没有发现版面问题", slides.len())
    } else {
        format!("{} 页，{} 个问题：\n{}", slides.len(), found.len(), found.iter().map(|(s, f)| format!("- 第 {s} 页 [{}] {}", if f.level == "error" { "错误" } else { "提示" }, f.message)).collect::<Vec<_>>().join("\n"))
    };
    Ok(Done { summary, data: json!({ "slides": slides.len(), "findings": list }), failed: n > 0 })
}

// ───────── 工作目录 ─────────

struct DeckDir {
    root: PathBuf,
}

impl DeckDir {
    fn new(dir: &str) -> Self {
        Self { root: PathBuf::from(dir) }
    }
    fn style(&self) -> Result<Style, Fail> {
        let p = self.root.join("style.json");
        if !p.exists() {
            return Err(Fail::new(format!("{} 不是 deck 工作目录", self.root.display())).hint("先运行 officepilot deck start <目录> --style … --outline …"));
        }
        load_style(Some(&p.to_string_lossy()))
    }
    fn outline(&self) -> Result<Outline, Fail> {
        parse(json_input(&self.root.join("outline.json").to_string_lossy())?, "大纲")
    }
    fn page_path(&self, n: usize) -> PathBuf {
        self.root.join("pages").join(format!("{n:02}.json"))
    }
}

fn pretty(v: &impl serde::Serialize) -> Vec<u8> {
    serde_json::to_vec_pretty(v).unwrap_or_default()
}

pub fn start(dir: &str, style: &str, outline: &str) -> Out {
    let style = load_style(Some(style))?;
    let outline_v: Outline = parse(json_input(outline)?, "大纲")?;
    let f = deck::check_outline(&outline_v);
    if errors(&f) > 0 {
        return Ok(Done { summary: format!("大纲没有通过检查，未写入：\n{}", findings_text(&f)), data: json!({ "findings": f }), failed: true });
    }
    let d = DeckDir::new(dir);
    std::fs::create_dir_all(d.root.join("pages"))?;
    write_atomic(&d.root.join("style.json").to_string_lossy(), &pretty(&style))?;
    write_atomic(&d.root.join("outline.json").to_string_lossy(), &pretty(&outline_v))?;
    let list: Vec<String> = outline_v.pages.iter().enumerate().map(|(i, p)| format!("{}. {}", i + 1, p.title)).collect();
    Ok(Done::ok(
        format!("已建立 {dir}：{} 页待写。接下来逐页运行 officepilot deck page {dir} <页码> <页面.json>\n{}{}", outline_v.pages.len(), list.join("\n"), if f.is_empty() { String::new() } else { format!("\n{}", findings_text(&f)) }),
        json!({ "dir": dir, "pages": outline_v.pages.len(), "findings": f }),
    ))
}

pub fn page(dir: &str, n: usize, spec: &str, replace: bool) -> Out {
    let d = DeckDir::new(dir);
    let style = d.style()?;
    let outline = d.outline()?;
    if n == 0 || n > outline.pages.len() {
        return Err(Fail::new(format!("页码 {n} 超出大纲范围（1–{}）", outline.pages.len())));
    }
    if replace && !d.page_path(n).exists() {
        return Err(Fail::new(format!("第 {n} 页还没有提交过")).hint("先用 deck page 提交"));
    }
    let value = json_input(spec)?;
    let page: Page = parse(value, "页面规格")?;
    let f = deck::check_page(&page, &style, Some((&outline, n)), Some(&d.root));
    let errs = errors(&f);
    if errs > 0 {
        return Ok(Done { summary: format!("第 {n} 页没有通过检查（未保存），{errs} 个必须修改：\n{}", findings_text(&f)), data: json!({ "page": n, "saved": false, "findings": f }), failed: true });
    }
    write_atomic(&d.page_path(n).to_string_lossy(), &pretty(&page))?;
    let mut summary = format!("第 {n} 页已保存{}", if f.is_empty() { "，检查通过".to_string() } else { format!("，{} 条提示：\n{}", f.len(), findings_text(&f)) });
    let mut data = json!({ "page": n, "saved": true, "findings": f });
    if replace {
        if let Ok(text) = std::fs::read_to_string(d.root.join("build.json")) {
            if let Some(out) = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["out"].as_str().map(str::to_string)) {
                let rebuilt = build(dir, &out, true)?;
                summary.push_str(&format!("\n{}", rebuilt.summary));
                data["rebuilt"] = rebuilt.data;
            }
        }
    }
    Ok(Done { summary, data, failed: false })
}

pub fn status(dir: &str) -> Out {
    let d = DeckDir::new(dir);
    d.style()?;
    let outline = d.outline()?;
    let done: Vec<usize> = (1..=outline.pages.len()).filter(|&n| d.page_path(n).exists()).collect();
    let missing: Vec<usize> = (1..=outline.pages.len()).filter(|n| !done.contains(n)).collect();
    Ok(Done::ok(
        format!("{} 页，已提交 {} 页{}", outline.pages.len(), done.len(), if missing.is_empty() { String::new() } else { format!("，还差：{}", missing.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("、")) }),
        json!({ "pages": outline.pages.len(), "done": done, "missing": missing }),
    ))
}

fn write_deck(spec: &DeckSpec, base: Option<&Path>, out: &str) -> Out {
    let result = deck::build(spec, base)?;
    write_atomic(out, &result.bytes)?;
    // 组装后再按成品审查一遍
    let deck_file = pptx::Deck::open(result.bytes)?;
    let audit = deck::audit(&deck_file.slides()?, deck_file.size);
    let mut all: Vec<Value> = result.findings.iter().map(|(p, f)| json!({ "slide": p, "level": f.level, "code": f.code, "message": f.message })).collect();
    all.extend(audit.iter().filter(|(_, f)| f.code != "overlap").map(|(p, f)| json!({ "slide": p, "level": f.level, "code": f.code, "message": f.message })));
    let lines: Vec<String> = all.iter().map(|v| format!("- 第 {} 页 [{}] {}", v["slide"], if v["level"] == "error" { "错误" } else { "提示" }, v["message"].as_str().unwrap_or(""))).collect();
    Ok(Done::ok(
        format!("已生成 {out}（{} 页）{}", spec.pages.len(), if lines.is_empty() { "，版面检查通过".to_string() } else { format!("，{} 条版面提示：\n{}", lines.len(), lines.join("\n")) }),
        json!({ "path": out, "slides": spec.pages.len(), "findings": all }),
    ))
}

pub fn build(dir: &str, out: &str, allow_missing: bool) -> Out {
    let d = DeckDir::new(dir);
    let style = d.style()?;
    let outline = d.outline()?;
    let mut pages = Vec::new();
    let mut missing = Vec::new();
    for n in 1..=outline.pages.len() {
        match std::fs::read_to_string(d.page_path(n)) {
            Ok(t) => pages.push(parse::<Page>(serde_json::from_str(&t).map_err(|e| Fail::new(e.to_string()))?, "页面规格")?),
            Err(_) => missing.push(n),
        }
    }
    if !missing.is_empty() && !allow_missing {
        return Err(Fail::new(format!("还有 {} 页没提交：{}", missing.len(), missing.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("、"))).hint("先用 deck page 补齐，或加 --allow-missing"));
    }
    let spec = DeckSpec { title: Some(outline.title.clone()), style, pages };
    let done = write_deck(&spec, Some(&d.root), out)?;
    write_atomic(&d.root.join("build.json").to_string_lossy(), &pretty(&json!({ "out": out })))?;
    Ok(done)
}

/// 一次性生成：spec 是整套规格文件，或页面目录（配合 outline / style）
pub fn create_pptx(spec: &str, outline: Option<&str>, style: Option<&str>, out: &str) -> Out {
    let path = Path::new(spec);
    if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|e| e == "json").unwrap_or(false)).collect();
        files.sort();
        let mut pages = Vec::new();
        for f in files {
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(name, "style.json" | "outline.json" | "build.json") {
                continue;
            }
            pages.push(parse::<Page>(json_input(&f.to_string_lossy())?, &format!("页面 {name}"))?);
        }
        let style = load_style(style)?;
        let title = match outline {
            Some(o) => parse::<Outline>(json_input(o)?, "大纲")?.title,
            None => String::new(),
        };
        return write_deck(&DeckSpec { title: Some(title), style, pages }, Some(path), out);
    }
    let mut spec_v: DeckSpec = parse(json_input(spec)?, "PPT 规格")?;
    if let Some(s) = style {
        spec_v.style = load_style(Some(s))?;
    }
    write_deck(&spec_v, base_of(spec).as_deref(), out)
}
