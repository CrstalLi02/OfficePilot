//! 本地格式转换（不联网）

use std::path::Path;

use officepilot_engine::markdown::{self, Block, Span};
use officepilot_engine::{docx, pptx, xlsx, Kind};
use serde_json::{json, Value};

use crate::io::{read_file, text_input, write_atomic};
use crate::office::{build_xlsx, csv_rows, markdown_to_docx};
use crate::{Done, Fail, Out};

fn ext(path: &str) -> String {
    Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase()
}

fn default_out(file: &str, to: &str) -> String {
    Path::new(file).with_extension(to).to_string_lossy().into_owned()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', "<br>")
}

fn md_table(rows: &[Vec<String>]) -> String {
    let Some(first) = rows.first() else { return String::new() };
    let width = rows.iter().map(Vec::len).max().unwrap_or(first.len()).max(1);
    let line = |r: &Vec<String>| format!("| {} |", (0..width).map(|i| md_cell(r.get(i).map(String::as_str).unwrap_or(""))).collect::<Vec<_>>().join(" | "));
    let mut out = vec![line(first), format!("|{}|", vec!["---"; width].join("|"))];
    out.extend(rows[1..].iter().map(line));
    out.join("\n")
}

fn html_table(rows: &[Vec<String>]) -> String {
    let mut out = String::from("<table>\n");
    for (i, r) in rows.iter().enumerate() {
        let tag = if i == 0 { "th" } else { "td" };
        out.push_str(&format!("<tr>{}</tr>\n", r.iter().map(|c| format!("<{tag}>{}</{tag}>", esc(c).replace('\n', "<br>"))).collect::<String>()));
    }
    out.push_str("</table>\n");
    out
}

fn html_page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title><style>body{{font-family:-apple-system,\"PingFang SC\",\"Microsoft YaHei\",sans-serif;max-width:860px;margin:40px auto;padding:0 16px;line-height:1.7;color:#1f2937}}table{{border-collapse:collapse;margin:12px 0}}td,th{{border:1px solid #d1d5db;padding:6px 10px}}th{{background:#f3f4f6}}blockquote{{border-left:4px solid #cbd5e1;margin:0;padding-left:12px;color:#475569}}code{{background:#f3f4f6;padding:1px 4px;border-radius:4px}}</style></head><body>\n{body}</body></html>\n",
        esc(title)
    )
}

// ───────── Word ─────────

fn docx_to_md(bytes: Vec<u8>) -> Result<String, Fail> {
    let blocks = docx::Document::open(bytes)?.blocks()?;
    let mut out = Vec::new();
    for b in blocks {
        out.push(match b.kind.as_str() {
            "heading" => format!("{} {}", "#".repeat(b.level.unwrap_or(1).clamp(1, 6) as usize), b.text),
            "list" => format!("{}- {}", "  ".repeat(b.level.unwrap_or(1).saturating_sub(1) as usize), b.text),
            "table" => md_table(b.rows.as_deref().unwrap_or(&[])),
            _ if b.text.trim().is_empty() => continue,
            _ => b.text.replace('\n', "  \n"),
        });
    }
    Ok(join_md(out))
}

/// 列表项之间不空行，其余块之间空一行
fn join_md(lines: Vec<String>) -> String {
    let mut out = String::new();
    let mut prev_list = false;
    for l in lines {
        let is_list = l.trim_start().starts_with("- ");
        if !out.is_empty() {
            out.push_str(if is_list && prev_list { "\n" } else { "\n\n" });
        }
        out.push_str(&l);
        prev_list = is_list;
    }
    out.push('\n');
    out
}

fn docx_to_txt(bytes: Vec<u8>) -> Result<String, Fail> {
    let blocks = docx::Document::open(bytes)?.blocks()?;
    Ok(blocks.into_iter().map(|b| match b.rows {
        Some(rows) => rows.iter().map(|r| r.join("\t")).collect::<Vec<_>>().join("\n"),
        None => b.text,
    }).collect::<Vec<_>>().join("\n") + "\n")
}

// ───────── Markdown → HTML ─────────

fn spans_html(spans: &[Span]) -> String {
    spans
        .iter()
        .map(|s| {
            let mut t = esc(&s.text);
            if s.code {
                t = format!("<code>{t}</code>");
            }
            if s.bold {
                t = format!("<strong>{t}</strong>");
            }
            if s.italic {
                t = format!("<em>{t}</em>");
            }
            if let Some(url) = &s.link {
                t = format!("<a href=\"{}\">{t}</a>", esc(url).replace('"', "&quot;"));
            }
            t
        })
        .collect()
}

pub fn markdown_to_html(md: &str) -> String {
    let mut out = String::new();
    let mut open_list: Option<&str> = None;
    for block in markdown::parse(md) {
        let list_tag = match &block {
            Block::Bullet { .. } => Some("ul"),
            Block::Numbered { .. } => Some("ol"),
            _ => None,
        };
        if open_list != list_tag {
            if let Some(t) = open_list {
                out.push_str(&format!("</{t}>\n"));
            }
            if let Some(t) = list_tag {
                out.push_str(&format!("<{t}>\n"));
            }
            open_list = list_tag;
        }
        match block {
            Block::Heading { level, spans } => out.push_str(&format!("<h{level}>{}</h{level}>\n", spans_html(&spans))),
            Block::Para(spans) => out.push_str(&format!("<p>{}</p>\n", spans_html(&spans))),
            Block::Bullet { level, spans } | Block::Numbered { level, spans } => out.push_str(&format!("<li style=\"margin-left:{}em\">{}</li>\n", level as u32 * 1, spans_html(&spans))),
            Block::Quote(spans) => out.push_str(&format!("<blockquote>{}</blockquote>\n", spans_html(&spans))),
            Block::Code(lines) => out.push_str(&format!("<pre><code>{}</code></pre>\n", esc(&lines.join("\n")))),
            Block::Table(rows) => out.push_str(&html_table(&rows.iter().map(|r| r.iter().map(|c| markdown::plain(c)).collect()).collect::<Vec<_>>())),
        }
    }
    if let Some(t) = open_list {
        out.push_str(&format!("</{t}>\n"));
    }
    out
}

// ───────── Excel ─────────

/// 工作表的完整矩形数据（显示值）
fn sheet_grid(wb: &xlsx::Workbook, name: &str) -> Result<Vec<Vec<String>>, Fail> {
    let view = wb.read(Some(name), None, 1_048_576)?;
    let Some((first, last)) = officepilot_engine::formula::parse_range(&view.dimension) else { return Ok(Vec::new()) };
    let mut grid = Vec::new();
    for (r, cells) in &view.rows {
        let mut row = vec![String::new(); (last.1 - first.1 + 1) as usize];
        for c in cells {
            if let Some((_, col)) = officepilot_engine::formula::parse_cell(&c.cell) {
                if col >= first.1 && col <= last.1 {
                    row[(col - first.1) as usize] = match &c.value {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    };
                }
            }
        }
        let _ = r;
        grid.push(row);
    }
    while grid.last().map(|r| r.iter().all(String::is_empty)).unwrap_or(false) {
        grid.pop();
    }
    Ok(grid)
}

fn csv_line(row: &[String]) -> String {
    row.iter()
        .map(|c| if c.contains([',', '"', '\n', '\r']) { format!("\"{}\"", c.replace('"', "\"\"")) } else { c.clone() })
        .collect::<Vec<_>>()
        .join(",")
}

// ───────── PPT ─────────

fn pptx_to_md(bytes: Vec<u8>) -> Result<String, Fail> {
    let slides = pptx::Deck::open(bytes)?.slides()?;
    let mut out = Vec::new();
    for s in slides {
        let title = s.shapes.iter().find(|x| x.kind == "title").map(|x| x.text.replace('\n', " ")).unwrap_or_else(|| format!("第 {} 页", s.index));
        out.push(format!("## {title}"));
        for sh in s.shapes.iter().filter(|x| x.kind != "title" && x.kind != "footer") {
            match (&sh.rows, sh.kind.as_str()) {
                (Some(rows), _) => out.push(md_table(rows)),
                (None, "picture") => out.push(format!("![{}]()", sh.text)),
                (None, "subtitle") if !sh.text.is_empty() => out.push(format!("*{}*", sh.text.replace('\n', " "))),
                (None, _) if !sh.text.trim().is_empty() => {
                    for line in sh.text.lines().filter(|l| !l.trim().is_empty()) {
                        out.push(format!("- {}", line.trim()));
                    }
                }
                _ => {}
            }
        }
        if let Some(n) = &s.notes {
            out.push(format!("> 备注：{}", n.replace('\n', " ")));
        }
    }
    Ok(join_md(out))
}

pub fn convert(file: &str, to: &str, out: Option<&str>) -> Out {
    let to = to.trim_start_matches('.').to_lowercase();
    let from = ext(file);
    let target = out.map(str::to_string).unwrap_or_else(|| default_out(file, &to));
    if Path::new(&target) == Path::new(file) {
        return Err(Fail::new("输出路径和输入相同").hint("用 --out 指定另一个文件"));
    }
    let title = Path::new(file).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let write_text = |text: String| -> Out {
        write_atomic(&target, text.as_bytes())?;
        Ok(Done::ok(format!("已转换 {file} → {target}（{} 字节）", text.len()), json!({ "path": target, "bytes": text.len() })))
    };
    let kind = Kind::from_name(file);
    match (from.as_str(), to.as_str()) {
        (_, "md" | "markdown") if kind == Some(Kind::Docx) => write_text(docx_to_md(read_file(file)?)?),
        (_, "txt") if kind == Some(Kind::Docx) => write_text(docx_to_txt(read_file(file)?)?),
        (_, "html") if kind == Some(Kind::Docx) => write_text(html_page(&title, &markdown_to_html(&docx_to_md(read_file(file)?)?))),
        (_, "md" | "markdown") if kind == Some(Kind::Pptx) => write_text(pptx_to_md(read_file(file)?)?),
        (_, "txt") if kind == Some(Kind::Pptx) => write_text(pptx_to_md(read_file(file)?)?.replace("## ", "").replace("- ", "")),
        (_, "html") if kind == Some(Kind::Pptx) => write_text(html_page(&title, &markdown_to_html(&pptx_to_md(read_file(file)?)?))),
        (_, "csv" | "md" | "markdown" | "html") if kind == Some(Kind::Xlsx) => {
            let wb = xlsx::Workbook::open(read_file(file)?)?;
            let names: Vec<String> = wb.sheets.iter().filter(|s| !s.hidden).map(|s| s.name.clone()).collect();
            if to == "csv" {
                // 每个工作表一个文件；只有一个时就用目标文件名
                let mut written = Vec::new();
                for name in &names {
                    let grid = sheet_grid(&wb, name)?;
                    let text = format!("\u{feff}{}\n", grid.iter().map(|r| csv_line(r)).collect::<Vec<_>>().join("\n"));
                    let path = if names.len() == 1 { target.clone() } else { default_out(&target, "").trim_end_matches('.').to_string() + &format!("-{name}.csv") };
                    write_atomic(&path, text.as_bytes())?;
                    written.push(path);
                }
                return Ok(Done::ok(format!("已转换 {file} → {}", written.join("、")), json!({ "paths": written })));
            }
            let mut md = Vec::new();
            for name in &names {
                md.push(format!("## {name}"));
                md.push(md_table(&sheet_grid(&wb, name)?));
            }
            let md = join_md(md);
            write_text(if to == "html" { html_page(&title, &markdown_to_html(&md)) } else { md })
        }
        ("md" | "markdown" | "txt", "docx") => {
            let text = text_input(file)?;
            let md = if from == "txt" { text.lines().map(|l| l.replace('#', "\\#")).collect::<Vec<_>>().join("\n\n") } else { text };
            let bytes = markdown_to_docx(&md, None)?;
            write_atomic(&target, &bytes)?;
            Ok(Done::ok(format!("已转换 {file} → {target}"), json!({ "path": target })))
        }
        ("md" | "markdown", "html") => write_text(html_page(&title, &markdown_to_html(&text_input(file)?))),
        ("csv" | "tsv", "xlsx") => {
            let rows = csv_rows(&text_input(file)?);
            let (bytes, _) = build_xlsx(json!({ "sheets": [{ "name": title.chars().take(31).collect::<String>(), "rows": rows }] }))?;
            write_atomic(&target, &bytes)?;
            Ok(Done::ok(format!("已转换 {file} → {target}"), json!({ "path": target })))
        }
        ("pdf", "txt" | "md") => {
            let output = std::process::Command::new("pdftotext").args(["-layout", "-enc", "UTF-8", file, "-"]).output();
            match output {
                Ok(o) if o.status.success() => write_text(String::from_utf8_lossy(&o.stdout).into_owned()),
                _ => Err(Fail::new("PDF 转文字需要本机安装 pdftotext（poppler）").hint("macOS：brew install poppler；PDF 转 Word / Excel / PPT 会在后续版本提供")),
            }
        }
        (_, "pdf" | "png") => Err(Fail::new("转 PDF / 渲染图片需要排版引擎，暂不支持").hint("后续版本提供；现在可以用 officepilot open 在 Office 软件里导出")),
        _ => Err(Fail::new(format!("不支持 {from} → {to}")).hint("支持：docx/pptx → md/txt/html，xlsx → csv/md/html，md/txt → docx，md → html，csv → xlsx，pdf → txt/md（需 pdftotext）")),
    }
}
