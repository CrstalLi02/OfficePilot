//! 内置说明与能力清单

use serde_json::json;

use crate::{Done, Fail, Out};

pub const TOPICS: &[(&str, &str)] = &[
    ("overview", include_str!("../guides/overview.md")),
    ("docs", include_str!("../guides/docs.md")),
    ("sheet", include_str!("../guides/sheet.md")),
    ("slides", include_str!("../guides/slides.md")),
    ("deck", include_str!("../guides/deck.md")),
    ("formulas", include_str!("../guides/formulas.md")),
];

pub fn guide(topic: Option<&str>) -> Out {
    let wanted = topic.unwrap_or("overview").trim().to_lowercase();
    let alias = match wanted.as_str() {
        "word" | "docx" | "doc" => "docs",
        "excel" | "xlsx" | "sheets" => "sheet",
        "ppt" | "pptx" | "slide" => "slides",
        "design" | "decks" => "deck",
        "formula" => "formulas",
        other => other,
    };
    match TOPICS.iter().find(|(name, _)| *name == alias) {
        Some((name, text)) => Ok(Done::ok(text.trim_end().to_string(), json!({ "topic": name, "text": text }))),
        None => Err(Fail::new(format!("没有“{wanted}”这个主题")).hint(format!("可选：{}", TOPICS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" / ")))),
    }
}

fn has_command(cmd: &str) -> bool {
    std::process::Command::new(cmd).arg("-v").output().is_ok()
}

pub fn capabilities() -> Out {
    let pdftotext = has_command("pdftotext");
    let data = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "read": ["docx", "xlsx", "pptx"],
        "edit": ["docx", "xlsx", "pptx"],
        "create": ["docx（来自 Markdown）", "xlsx（来自 JSON / CSV）", "pptx（来自页面规格）"],
        "convert": {
            "docx": ["md", "txt", "html"],
            "pptx": ["md", "txt", "html"],
            "xlsx": ["csv", "md", "html"],
            "md": ["docx", "html"],
            "txt": ["docx"],
            "csv": ["xlsx"],
            "pdf": if pdftotext { json!(["txt", "md"]) } else { json!([]) },
        },
        "checks": ["slides check（页面规格）", "slides audit（已有 .pptx）"],
        "render": false,
        "cloud": [],
        "guides": TOPICS.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
    });
    let summary = format!(
        "officepilot {}\n读取 / 编辑：docx xlsx pptx（原格式，可写 Word 修订）\n新建：docx ← Markdown，xlsx ← JSON/CSV，pptx ← 页面规格\n转换：docx/pptx → md/txt/html，xlsx → csv/md/html，md → docx/html，csv → xlsx{}\n检查：slides check / slides audit\n渲染图片 / 转 PDF：暂不支持\n联网能力：无（命令行不调用大模型或网络）",
        env!("CARGO_PKG_VERSION"),
        if pdftotext { "，pdf → txt/md" } else { "" }
    );
    Ok(Done::ok(summary, data))
}
