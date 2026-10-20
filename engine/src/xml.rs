//! 只记录位置的 XML 扫描：把文档解析成元素树，每个元素记住它在原文里的字节范围。
//!
//! 引擎从不整体重写 XML：改动都是“用新片段替换某个元素的原文范围”，其余字节原样保留。
//! 因此这里不处理命名空间，也不做 DTD 校验，只要求标签成对。

use crate::{EngineError, Result};

#[derive(Debug, Clone)]
pub struct Node {
    /// 带前缀的标签名，如 `w:p`；根节点为空串
    pub name: String,
    /// `<` 的位置
    pub start: usize,
    /// 开始标签 `>` 之后的位置
    pub open_end: usize,
    /// 结束标签 `</` 的位置（自闭合元素等于 end）
    pub close_start: usize,
    /// 结束标签 `>` 之后的位置
    pub end: usize,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    pub self_closing: bool,
}

pub struct Xml<'a> {
    pub src: &'a str,
    pub nodes: Vec<Node>,
}

pub const ROOT: usize = 0;

fn find(src: &[u8], from: usize, pat: &[u8]) -> Option<usize> {
    if pat.is_empty() || from > src.len() {
        return None;
    }
    src[from..].windows(pat.len()).position(|w| w == pat).map(|p| p + from)
}

/// 开始标签的结束位置（跳过属性值里的 `>`）
fn tag_end(src: &[u8], from: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (i, &b) in src.iter().enumerate().skip(from) {
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'"' || b == b'\'' {
            quote = b;
        } else if b == b'>' {
            return Some(i);
        }
    }
    None
}

impl<'a> Xml<'a> {
    pub fn parse(src: &'a str) -> Result<Self> {
        let bytes = src.as_bytes();
        let mut nodes = vec![Node {
            name: String::new(),
            start: 0,
            open_end: 0,
            close_start: src.len(),
            end: src.len(),
            children: Vec::new(),
            parent: None,
            self_closing: false,
        }];
        let mut stack = vec![ROOT];
        let mut i = 0;
        let bad = |msg: String| EngineError::Xml(msg);
        while let Some(lt) = find(bytes, i, b"<") {
            let rest = &bytes[lt..];
            if rest.starts_with(b"<?") {
                i = find(bytes, lt, b"?>").ok_or_else(|| bad("处理指令没有结束".into()))? + 2;
            } else if rest.starts_with(b"<!--") {
                i = find(bytes, lt, b"-->").ok_or_else(|| bad("注释没有结束".into()))? + 3;
            } else if rest.starts_with(b"<![CDATA[") {
                i = find(bytes, lt, b"]]>").ok_or_else(|| bad("CDATA 没有结束".into()))? + 3;
            } else if rest.starts_with(b"<!") {
                i = tag_end(bytes, lt).ok_or_else(|| bad("声明没有结束".into()))? + 1;
            } else if rest.starts_with(b"</") {
                let gt = find(bytes, lt, b">").ok_or_else(|| bad("结束标签没有结束".into()))?;
                let name = src[lt + 2..gt].trim();
                let top = *stack.last().unwrap();
                if top == ROOT || nodes[top].name != name {
                    return Err(bad(format!("结束标签 </{name}> 与开始标签 <{}> 不匹配（位置 {lt}）", nodes[top].name)));
                }
                nodes[top].close_start = lt;
                nodes[top].end = gt + 1;
                stack.pop();
                i = gt + 1;
            } else {
                let gt = tag_end(bytes, lt).ok_or_else(|| bad("开始标签没有结束".into()))?;
                let self_closing = bytes[gt - 1] == b'/';
                let inner = &src[lt + 1..if self_closing { gt - 1 } else { gt }];
                let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
                let name = inner[..name_end].to_string();
                if name.is_empty() {
                    return Err(bad(format!("位置 {lt} 的标签没有名字")));
                }
                let parent = *stack.last().unwrap();
                let id = nodes.len();
                nodes.push(Node {
                    name,
                    start: lt,
                    open_end: gt + 1,
                    close_start: gt + 1,
                    end: gt + 1,
                    children: Vec::new(),
                    parent: Some(parent),
                    self_closing,
                });
                nodes[parent].children.push(id);
                if !self_closing {
                    stack.push(id);
                }
                i = gt + 1;
            }
        }
        if stack.len() != 1 {
            let open = &nodes[*stack.last().unwrap()].name;
            return Err(bad(format!("元素 <{open}> 没有结束")));
        }
        Ok(Self { src, nodes })
    }

    pub fn node(&self, id: usize) -> &Node {
        &self.nodes[id]
    }

    pub fn name(&self, id: usize) -> &str {
        &self.nodes[id].name
    }

    pub fn outer(&self, id: usize) -> &'a str {
        let n = &self.nodes[id];
        &self.src[n.start..n.end]
    }

    pub fn inner(&self, id: usize) -> &'a str {
        let n = &self.nodes[id];
        &self.src[n.open_end..n.close_start]
    }

    /// 开始标签原文（含 `<` 与 `>`）
    pub fn open_tag(&self, id: usize) -> &'a str {
        let n = &self.nodes[id];
        &self.src[n.start..n.open_end]
    }

    /// 开始标签，自闭合的改写成非自闭合（用于在空元素里放入内容）
    pub fn open_tag_expanded(&self, id: usize) -> String {
        let tag = self.open_tag(id);
        if self.nodes[id].self_closing {
            format!("{}>", tag[..tag.len() - 2].trim_end())
        } else {
            tag.to_string()
        }
    }

    pub fn attr(&self, id: usize, key: &str) -> Option<String> {
        let tag = self.open_tag(id);
        attr_in(tag, key)
    }

    pub fn children(&self, id: usize) -> &[usize] {
        &self.nodes[id].children
    }

    pub fn child(&self, id: usize, name: &str) -> Option<usize> {
        self.nodes[id].children.iter().copied().find(|&c| self.nodes[c].name == name)
    }

    pub fn children_named<'s>(&'s self, id: usize, name: &'s str) -> impl Iterator<Item = usize> + 's {
        self.nodes[id].children.iter().copied().filter(move |&c| self.nodes[c].name == name)
    }

    /// 深度优先、按文档顺序遍历后代（不含自身）
    pub fn descendants(&self, id: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.nodes[id].children.iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            out.push(n);
            stack.extend(self.nodes[n].children.iter().rev().copied());
        }
        out
    }

    pub fn find_desc(&self, id: usize, name: &str) -> Option<usize> {
        self.descendants(id).into_iter().find(|&n| self.nodes[n].name == name)
    }

    pub fn has_ancestor(&self, id: usize, name: &str, stop: usize) -> bool {
        let mut cur = self.nodes[id].parent;
        while let Some(p) = cur {
            if p == stop {
                return false;
            }
            if self.nodes[p].name == name {
                return true;
            }
            cur = self.nodes[p].parent;
        }
        false
    }

    /// 元素内的文字（解码实体）
    pub fn text(&self, id: usize) -> String {
        unescape(self.inner(id))
    }

    /// 第一个顶层元素（跳过声明）
    pub fn document_element(&self) -> Option<usize> {
        self.nodes[ROOT].children.first().copied()
    }
}

pub fn attr_in(tag: &str, key: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(pos) = tag[from..].find(key) {
        let at = from + pos;
        from = at + key.len();
        let before_ok = at > 0 && (bytes[at - 1] as char).is_whitespace();
        let rest = tag[at + key.len()..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let rest = rest[1..].trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let end = rest[1..].find(quote)?;
        return Some(unescape(&rest[1..1 + end]));
    }
    None
}

/// 在开始标签里设置属性（已有则替换值）
pub fn set_attr(tag: &str, key: &str, value: &str) -> String {
    let escaped = escape_attr(value);
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(pos) = tag[from..].find(key) {
        let at = from + pos;
        from = at + key.len();
        if at == 0 || !(bytes[at - 1] as char).is_whitespace() {
            continue;
        }
        let after = &tag[at + key.len()..];
        let trimmed = after.trim_start();
        if !trimmed.starts_with('=') {
            continue;
        }
        let eq = at + key.len() + (after.len() - trimmed.len());
        let rest = tag[eq + 1..].trim_start();
        let q_pos = tag.len() - rest.len();
        let quote = rest.chars().next().unwrap_or('"');
        if let Some(end) = rest[1..].find(quote) {
            return format!("{}{key}=\"{escaped}\"{}", &tag[..at], &tag[q_pos + 1 + end + 1..]);
        }
    }
    let close = if tag.ends_with("/>") { tag.len() - 2 } else { tag.len() - 1 };
    let head = tag[..close].trim_end();
    format!("{head} {key}=\"{escaped}\"{}", &tag[close..])
}

pub fn remove_attr(tag: &str, key: &str) -> String {
    let Some(_) = attr_in(tag, key) else { return tag.to_string() };
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(pos) = tag[from..].find(key) {
        let at = from + pos;
        from = at + key.len();
        if at == 0 || !(bytes[at - 1] as char).is_whitespace() {
            continue;
        }
        let rest = tag[at + key.len()..].trim_start();
        if !rest.starts_with('=') {
            continue;
        }
        let rest = rest[1..].trim_start();
        let quote = rest.chars().next().unwrap_or('"');
        let Some(end) = rest[1..].find(quote) else { break };
        let stop = tag.len() - rest.len() + 1 + end + 1;
        let mut begin = at;
        while begin > 0 && (bytes[begin - 1] as char).is_whitespace() {
            begin -= 1;
        }
        return format!("{}{}", &tag[..begin], &tag[stop..]);
    }
    tag.to_string()
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // XML 1.0 不允许的控制字符直接丢弃
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

pub fn escape_attr(text: &str) -> String {
    escape(text).replace('"', "&quot;")
}

pub fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after.find(';').filter(|&s| s <= 12) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 一组不重叠的替换，按位置应用到原文
#[derive(Default)]
pub struct Splice {
    edits: Vec<(usize, usize, String)>,
}

impl Splice {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn replace(&mut self, start: usize, end: usize, text: impl Into<String>) {
        self.edits.push((start, end, text.into()));
    }

    pub fn insert(&mut self, at: usize, text: impl Into<String>) {
        self.edits.push((at, at, text.into()));
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn apply(mut self, src: &str) -> Result<String> {
        // 同一位置的多个插入保持提交顺序
        self.edits.sort_by_key(|e| (e.0, e.1));
        let mut out = String::with_capacity(src.len() + 256);
        let mut pos = 0;
        for (start, end, text) in self.edits {
            if start < pos {
                return Err(EngineError::Xml(format!("内部错误：修改范围重叠（{start} < {pos}）")));
            }
            out.push_str(&src[pos..start]);
            out.push_str(&text);
            pos = end;
        }
        out.push_str(&src[pos..]);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_positions_and_attrs() {
        let src = r#"<?xml version="1.0"?><a x="1>2"><b/><c y='q'>t&amp;u</c></a>"#;
        let xml = Xml::parse(src).unwrap();
        let a = xml.document_element().unwrap();
        assert_eq!(xml.name(a), "a");
        assert_eq!(xml.attr(a, "x").as_deref(), Some("1>2"));
        let c = xml.child(a, "c").unwrap();
        assert_eq!(xml.text(c), "t&u");
        assert_eq!(xml.outer(c), "<c y='q'>t&amp;u</c>");
        assert!(xml.nodes[xml.child(a, "b").unwrap()].self_closing);
    }

    #[test]
    fn rejects_mismatched() {
        assert!(Xml::parse("<a><b></a></b>").is_err());
        assert!(Xml::parse("<a>").is_err());
    }

    #[test]
    fn attr_editing() {
        assert_eq!(set_attr(r#"<c r="A1" s="2">"#, "s", "5"), r#"<c r="A1" s="5">"#);
        assert_eq!(set_attr(r#"<c r="A1"/>"#, "t", "b"), r#"<c r="A1" t="b"/>"#);
        assert_eq!(remove_attr(r#"<row r="1" spans="1:3" x="y">"#, "spans"), r#"<row r="1" x="y">"#);
        assert_eq!(attr_in(r#"<w:p w:rsidR="00" r="1">"#, "r").as_deref(), Some("1"));
    }
}
