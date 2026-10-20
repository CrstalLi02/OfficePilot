//! 段落内的文字查找与替换（Word 的 w:p / w:r / w:t 与 PPT 的 a:p / a:r / a:t 结构相同）。
//!
//! 段落文字分散在多个文本节点里（同一句话可能跨好几个格式片段）。这里把文本节点和
//! 制表符、换行等“原子”串起来查找，命中后只改写涉及的文本节点：替换内容放进第一个
//! 命中的节点，沿用它的格式，其余节点里被替换掉的部分删掉，没涉及的节点原样保留。

use crate::xml::{self, Splice, Xml};
use crate::Result;

pub struct Names {
    pub para: &'static str,
    pub run: &'static str,
    pub text: &'static str,
    /// 表示一个字符的空元素，如 w:tab → "\t"
    pub chars: &'static [(&'static str, &'static str)],
    /// 这些元素里的内容不算段落正文（文本框、兼容回退内容、已删除的修订等）
    pub skip: &'static [&'static str],
    /// 文本节点是否需要 xml:space="preserve"
    pub preserve_space: bool,
}

pub const WORD: Names = Names {
    para: "w:p",
    run: "w:r",
    text: "w:t",
    chars: &[("w:tab", "\t"), ("w:br", "\n"), ("w:cr", "\n"), ("w:noBreakHyphen", "-"), ("w:sym", "")],
    skip: &["w:del", "w:moveFrom", "mc:Fallback", "w:txbxContent", "w:drawing", "w:pict", "w:object", "w:footnoteReference", "w:endnoteReference"],
    preserve_space: true,
};

pub const DRAWING: Names = Names {
    para: "a:p",
    run: "a:r",
    text: "a:t",
    chars: &[("a:br", "\n")],
    skip: &["mc:Fallback"],
    preserve_space: false,
};

#[derive(Debug, Clone)]
pub struct Atom {
    pub node: usize,
    /// 文本节点为 true，制表符 / 换行等为 false
    pub is_text: bool,
    pub text: String,
    /// 在段落文字里的字节偏移
    pub offset: usize,
}

fn skipped(xml: &Xml, node: usize, para: usize, names: &Names) -> bool {
    names.skip.iter().any(|s| xml.name(node) == *s || xml.has_ancestor(node, s, para))
}

/// 段落里所有文字原子（按文档顺序）
pub fn atoms(xml: &Xml, para: usize, names: &Names) -> Vec<Atom> {
    let mut out = Vec::new();
    let mut offset = 0;
    for node in xml.descendants(para) {
        let name = xml.name(node);
        // 嵌套段落（文本框）不属于这个段落
        if name == names.para || xml.has_ancestor(node, names.para, para) {
            continue;
        }
        if name == names.text {
            if skipped(xml, node, para, names) {
                continue;
            }
            let text = xml.text(node);
            let len = text.len();
            out.push(Atom { node, is_text: true, text, offset });
            offset += len;
        } else if let Some((_, ch)) = names.chars.iter().find(|(n, _)| *n == name) {
            if skipped(xml, node, para, names) || ch.is_empty() {
                continue;
            }
            out.push(Atom { node, is_text: false, text: ch.to_string(), offset });
            offset += ch.len();
        }
    }
    out
}

pub fn paragraph_text(xml: &Xml, para: usize, names: &Names) -> String {
    atoms(xml, para, names).iter().map(|a| a.text.as_str()).collect()
}

/// 片段里的正文段落（不含文本框等跳过区域里的段落）
pub fn paragraphs(xml: &Xml, root: usize, names: &Names) -> Vec<usize> {
    let mut out = Vec::new();
    if xml.name(root) == names.para {
        out.push(root);
    }
    for node in xml.descendants(root) {
        if xml.name(node) == names.para && !names.skip.iter().any(|s| xml.has_ancestor(node, s, root)) && !xml.has_ancestor(node, names.para, root) {
            out.push(node);
        }
    }
    out
}

/// 命中位置：[start, end) 与替换内容锚定的位置
#[derive(Debug, Clone)]
pub struct Hit {
    pub start: usize,
    pub end: usize,
    /// 替换文字放在哪个位置（第一个与命中重叠的文本节点内）
    pub anchor: usize,
}

pub fn find_hits(atoms: &[Atom], text: &str, find: &str, all: bool) -> Vec<Hit> {
    if find.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (start, m) in text.match_indices(find) {
        let end = start + m.len();
        let anchor = atoms
            .iter()
            .filter(|a| a.is_text)
            .find(|a| a.offset + a.text.len() > start && a.offset < end)
            .map(|a| start.max(a.offset))
            .or_else(|| atoms.iter().filter(|a| a.is_text).find(|a| a.offset + a.text.len() >= start).map(|a| start.max(a.offset)))
            .unwrap_or(start);
        hits.push(Hit { start, end, anchor });
        if !all {
            break;
        }
    }
    hits
}

pub fn text_node(names: &Names, open_tag: &str, text: &str) -> String {
    let mut tag = open_tag.to_string();
    if tag.ends_with("/>") {
        tag = format!("{}>", tag[..tag.len() - 2].trim_end());
    }
    if names.preserve_space {
        tag = xml::set_attr(&tag, "xml:space", "preserve");
    }
    format!("{tag}{}</{}>", xml::escape(text), names.text)
}

/// 在片段的所有段落里查找替换（不留修订痕迹）。返回新片段与替换次数。
pub fn replace_plain(src: &str, names: &Names, find: &str, replace: &str, all: bool) -> Result<(String, usize)> {
    let xml = Xml::parse(src)?;
    let mut splice = Splice::new();
    let mut count = 0;
    let root = xml.document_element().unwrap_or(xml::ROOT);
    for para in paragraphs(&xml, root, names) {
        if count > 0 && !all {
            break;
        }
        let atoms = atoms(&xml, para, names);
        let text: String = atoms.iter().map(|a| a.text.as_str()).collect();
        let hits = find_hits(&atoms, &text, find, all);
        if hits.is_empty() {
            continue;
        }
        count += hits.len();
        for atom in &atoms {
            let a_start = atom.offset;
            let a_end = atom.offset + atom.text.len();
            let node = xml.node(atom.node);
            if !atom.is_text {
                // 制表符 / 换行整体落在命中范围内就删掉
                if hits.iter().any(|h| h.start <= a_start && h.end >= a_end) {
                    splice.replace(node.start, node.end, "");
                }
                continue;
            }
            let mut out = String::new();
            for (i, ch) in atom.text.char_indices() {
                let pos = a_start + i;
                for _ in hits.iter().filter(|h| h.anchor == pos) {
                    out.push_str(replace);
                }
                if !hits.iter().any(|h| pos >= h.start && pos < h.end) {
                    out.push(ch);
                }
            }
            if out != atom.text {
                splice.replace(node.start, node.end, text_node(names, xml.open_tag(atom.node), &out));
            }
        }
    }
    Ok((splice.apply(src)?, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello </w:t></w:r><w:r><w:t>wor</w:t></w:r><w:r><w:t xml:space="preserve">ld again</w:t></w:r></w:p>"#;

    #[test]
    fn replaces_across_runs_keeping_first_format() {
        let (out, n) = replace_plain(P, &WORD, "world", "there", true).unwrap();
        assert_eq!(n, 1);
        let xml = Xml::parse(&out).unwrap();
        let p = xml.document_element().unwrap();
        assert_eq!(paragraph_text(&xml, p, &WORD), "Hello there again");
        // 第一个片段（加粗）原样保留
        assert!(out.starts_with(r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello </w:t></w:r>"#));
    }

    #[test]
    fn replaces_inside_single_node_and_all_occurrences() {
        let src = r#"<a:p><a:r><a:t>a-b-a</a:t></a:r></a:p>"#;
        let (out, n) = replace_plain(src, &DRAWING, "a", "X", true).unwrap();
        assert_eq!(n, 2);
        assert_eq!(out, r#"<a:p><a:r><a:t>X-b-X</a:t></a:r></a:p>"#);
        let (out, n) = replace_plain(src, &DRAWING, "a", "Y", false).unwrap();
        assert_eq!(n, 1);
        assert_eq!(out, r#"<a:p><a:r><a:t>Y-b-a</a:t></a:r></a:p>"#);
    }

    #[test]
    fn skips_deleted_text_and_escapes() {
        let src = r#"<w:p><w:del><w:r><w:delText>old</w:delText></w:r></w:del><w:r><w:t>A&amp;B</w:t></w:r></w:p>"#;
        let xml = Xml::parse(src).unwrap();
        assert_eq!(paragraph_text(&xml, xml.document_element().unwrap(), &WORD), "A&B");
        let (out, _) = replace_plain(src, &WORD, "&", "<和>", true).unwrap();
        assert!(out.contains(r#"<w:t xml:space="preserve">A&lt;和&gt;B</w:t>"#));
    }
}
