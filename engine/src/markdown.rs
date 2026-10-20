//! 插入内容用的简化 Markdown：标题、段落、无序 / 有序列表（缩进两格为一级）、表格、引用、代码块，
//! 行内支持 **加粗**、*斜体*、`代码`、[链接](url)。

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading { level: u8, spans: Vec<Span> },
    Para(Vec<Span>),
    Bullet { level: u8, spans: Vec<Span> },
    Numbered { level: u8, spans: Vec<Span> },
    Quote(Vec<Span>),
    Code(Vec<String>),
    /// 第一行是表头
    Table(Vec<Vec<Vec<Span>>>),
}

pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

fn list_item(line: &str) -> Option<(u8, bool, &str)> {
    let indent = line.len() - line.trim_start().len();
    let level = (indent / 2).min(8) as u8;
    let t = line.trim_start();
    for marker in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = t.strip_prefix(marker) {
            return Some((level, false, rest));
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let rest = &t[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")).or_else(|| rest.strip_prefix("、")) {
            return Some((level, true, rest));
        }
    }
    None
}

fn table_cells(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_separator(line: &str) -> bool {
    let t = line.trim();
    t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

pub fn parse(md: &str) -> Vec<Block> {
    let lines: Vec<&str> = md.lines().collect();
    let mut blocks = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let flush = |para: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Para(inline(&para.join(" "))));
            para.clear();
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            flush(&mut para, &mut blocks);
            i += 1;
            continue;
        }
        if t.starts_with("```") {
            flush(&mut para, &mut blocks);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i].to_string());
                i += 1;
            }
            blocks.push(Block::Code(code));
            i += 1;
            continue;
        }
        let hashes = t.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Heading { level: hashes as u8, spans: inline(t[hashes..].trim()) });
            i += 1;
            continue;
        }
        if t.starts_with('|') && i + 1 < lines.len() && is_separator(lines[i + 1]) {
            flush(&mut para, &mut blocks);
            let mut rows = vec![table_cells(t).iter().map(|c| inline(c)).collect::<Vec<_>>()];
            i += 2;
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(table_cells(lines[i]).iter().map(|c| inline(c)).collect());
                i += 1;
            }
            let width = rows.iter().map(Vec::len).max().unwrap_or(1);
            for row in &mut rows {
                row.resize(width, Vec::new());
            }
            blocks.push(Block::Table(rows));
            continue;
        }
        if let Some((level, numbered, rest)) = list_item(line) {
            flush(&mut para, &mut blocks);
            let spans = inline(rest.trim());
            blocks.push(if numbered { Block::Numbered { level, spans } } else { Block::Bullet { level, spans } });
            i += 1;
            continue;
        }
        if let Some(rest) = t.strip_prefix('>') {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Quote(inline(rest.trim())));
            i += 1;
            continue;
        }
        para.push(t.to_string());
        i += 1;
    }
    flush(&mut para, &mut blocks);
    blocks
}

/// 行内格式
pub fn inline(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut cur = String::new();
    let (mut bold, mut italic) = (false, false);
    let push = |spans: &mut Vec<Span>, cur: &mut String, bold: bool, italic: bool| {
        if !cur.is_empty() {
            spans.push(Span { text: std::mem::take(cur), bold, italic, ..Default::default() });
        }
    };
    let closes = |from: usize, pat: &[char]| -> bool {
        (from..chars.len().saturating_sub(pat.len() - 1)).any(|j| chars[j..j + pat.len()] == *pat)
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            cur.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') && (bold || closes(i + 2, &['*', '*'])) {
            push(&mut spans, &mut cur, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        }
        if (c == '*' || c == '_') && (italic || closes(i + 1, &[c])) && !(c == '_' && i > 0 && chars[i - 1].is_alphanumeric()) {
            push(&mut spans, &mut cur, bold, italic);
            italic = !italic;
            i += 1;
            continue;
        }
        if c == '`' {
            if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '`') {
                push(&mut spans, &mut cur, bold, italic);
                spans.push(Span { text: chars[i + 1..end].iter().collect(), code: true, bold, italic, ..Default::default() });
                i = end + 1;
                continue;
            }
        }
        if c == '[' {
            if let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == ']') {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(paren) = (close + 2..chars.len()).find(|&j| chars[j] == ')') {
                        push(&mut spans, &mut cur, bold, italic);
                        spans.push(Span {
                            text: chars[i + 1..close].iter().collect(),
                            link: Some(chars[close + 2..paren].iter().collect()),
                            bold,
                            italic,
                            ..Default::default()
                        });
                        i = paren + 1;
                        continue;
                    }
                }
            }
        }
        cur.push(c);
        i += 1;
    }
    push(&mut spans, &mut cur, bold, italic);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks() {
        let md = "## 标题\n\n第一段\n继续\n\n- a\n  - b\n1. one\n\n| x | y |\n|---|---|\n| 1 | 2 |\n";
        let blocks = parse(md);
        assert!(matches!(&blocks[0], Block::Heading { level: 2, .. }));
        assert_eq!(blocks[1], Block::Para(vec![Span { text: "第一段 继续".into(), ..Default::default() }]));
        assert!(matches!(&blocks[2], Block::Bullet { level: 0, .. }));
        assert!(matches!(&blocks[3], Block::Bullet { level: 1, .. }));
        assert!(matches!(&blocks[4], Block::Numbered { level: 0, .. }));
        match &blocks[5] {
            Block::Table(rows) => assert_eq!(rows.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_inline() {
        let spans = inline("a **b** *c* `d` [e](http://x) 2*3 snake_case");
        assert!(spans.iter().any(|s| s.text == "b" && s.bold));
        assert!(spans.iter().any(|s| s.text == "c" && s.italic));
        assert!(spans.iter().any(|s| s.text == "d" && s.code));
        assert!(spans.iter().any(|s| s.text == "e" && s.link.as_deref() == Some("http://x")));
        assert_eq!(plain(&spans), "a b c d e 2*3 snake_case");
    }
}
