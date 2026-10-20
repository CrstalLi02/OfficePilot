//! OOXML 包（zip）：读取部件、记录改动；保存时没改过的条目按原始压缩数据逐字节复制。

use std::collections::{BTreeMap, HashSet};
use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::xml::{self, Xml};
use crate::{EngineError, Result};

pub const REL_OFFICE_DOCUMENT: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";

pub struct Package {
    bytes: Vec<u8>,
    names: Vec<String>,
    changed: BTreeMap<String, Vec<u8>>,
    added: Vec<String>,
    removed: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct Rel {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub external: bool,
}

impl Package {
    pub fn open(bytes: Vec<u8>) -> Result<Self> {
        let mut archive = ZipArchive::new(Cursor::new(&bytes)).map_err(|e| EngineError::Format(format!("不是有效的 Office 文件（zip 无法打开：{e}）")))?;
        let mut names = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let file = archive.by_index_raw(i).map_err(|e| EngineError::Format(e.to_string()))?;
            names.push(file.name().to_string());
        }
        drop(archive);
        if !names.iter().any(|n| n == "[Content_Types].xml") {
            return Err(EngineError::Format("缺少 [Content_Types].xml，不是 Office Open XML 文件".into()));
        }
        Ok(Self { bytes, names, changed: BTreeMap::new(), added: Vec::new(), removed: HashSet::new() })
    }

    pub fn has(&self, name: &str) -> bool {
        !self.removed.contains(name) && (self.changed.contains_key(name) || self.names.iter().any(|n| n == name))
    }

    pub fn names(&self) -> Vec<String> {
        self.names.iter().chain(self.added.iter()).filter(|n| !self.removed.contains(*n)).cloned().collect()
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        if self.removed.contains(name) {
            return Err(EngineError::Format(format!("部件 {name} 已删除")));
        }
        if let Some(data) = self.changed.get(name) {
            return Ok(data.clone());
        }
        let mut archive = ZipArchive::new(Cursor::new(&self.bytes)).map_err(|e| EngineError::Format(e.to_string()))?;
        let mut file = archive.by_name(name).map_err(|_| EngineError::Format(format!("缺少部件 {name}")))?;
        let mut out = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut out).map_err(|e| EngineError::Format(e.to_string()))?;
        Ok(out)
    }

    pub fn read_str(&self, name: &str) -> Result<String> {
        let bytes = self.read(name)?;
        // 去掉 UTF-8 BOM
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).map(<[u8]>::to_vec).unwrap_or(bytes);
        String::from_utf8(bytes).map_err(|_| EngineError::Format(format!("{name} 不是 UTF-8 编码")))
    }

    pub fn write(&mut self, name: &str, data: impl Into<Vec<u8>>) {
        self.removed.remove(name);
        if !self.names.iter().any(|n| n == name) && !self.added.iter().any(|n| n == name) {
            self.added.push(name.to_string());
        }
        self.changed.insert(name.to_string(), data.into());
    }

    pub fn remove(&mut self, name: &str) {
        self.changed.remove(name);
        self.removed.insert(name.to_string());
    }

    pub fn is_modified(&self) -> bool {
        !self.changed.is_empty() || !self.removed.is_empty()
    }

    pub fn save(&self) -> Result<Vec<u8>> {
        let mut archive = ZipArchive::new(Cursor::new(&self.bytes)).map_err(|e| EngineError::Format(e.to_string()))?;
        let mut writer = ZipWriter::new(Cursor::new(Vec::with_capacity(self.bytes.len() + 4096)));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let io = |e: zip::result::ZipError| EngineError::Format(format!("写入 zip 失败：{e}"));
        for i in 0..archive.len() {
            let file = archive.by_index_raw(i).map_err(io)?;
            let name = file.name().to_string();
            if self.removed.contains(&name) {
                continue;
            }
            if let Some(data) = self.changed.get(&name) {
                drop(file);
                writer.start_file(name, options).map_err(io)?;
                writer.write_all(data).map_err(|e| EngineError::Format(e.to_string()))?;
            } else {
                writer.raw_copy_file(file).map_err(io)?;
            }
        }
        for name in &self.added {
            if self.removed.contains(name) {
                continue;
            }
            writer.start_file(name.as_str(), options).map_err(io)?;
            writer.write_all(&self.changed[name]).map_err(|e| EngineError::Format(e.to_string()))?;
        }
        Ok(writer.finish().map_err(io)?.into_inner())
    }

    // ───────────── 关系（.rels）与内容类型 ─────────────

    pub fn rels_path(part: &str) -> String {
        match part.rfind('/') {
            Some(slash) => format!("{}/_rels/{}.rels", &part[..slash], &part[slash + 1..]),
            None => format!("_rels/{part}.rels"),
        }
    }

    pub fn rels(&self, part: &str) -> Result<Vec<Rel>> {
        let path = Self::rels_path(part);
        if !self.has(&path) {
            return Ok(Vec::new());
        }
        let text = self.read_str(&path)?;
        let xml = Xml::parse(&text)?;
        let Some(root) = xml.document_element() else { return Ok(Vec::new()) };
        Ok(xml
            .children_named(root, "Relationship")
            .map(|r| Rel {
                id: xml.attr(r, "Id").unwrap_or_default(),
                kind: xml.attr(r, "Type").unwrap_or_default(),
                target: xml.attr(r, "Target").unwrap_or_default(),
                external: xml.attr(r, "TargetMode").as_deref() == Some("External"),
            })
            .collect())
    }

    /// 把关系目标解析成包内的部件名
    pub fn resolve(part: &str, target: &str) -> String {
        if let Some(abs) = target.strip_prefix('/') {
            return abs.to_string();
        }
        let base = part.rfind('/').map(|s| &part[..s]).unwrap_or("");
        let mut segs: Vec<&str> = if base.is_empty() { Vec::new() } else { base.split('/').collect() };
        for seg in target.split('/') {
            match seg {
                "." | "" => {}
                ".." => {
                    segs.pop();
                }
                s => segs.push(s),
            }
        }
        segs.join("/")
    }

    /// 从 from 部件指向 to 部件的相对路径
    pub fn relative(from_part: &str, to_part: &str) -> String {
        let from: Vec<&str> = from_part.split('/').collect();
        let to: Vec<&str> = to_part.split('/').collect();
        let from_dir = &from[..from.len() - 1];
        let common = from_dir.iter().zip(to.iter()).take_while(|(a, b)| a == b).count();
        let mut out: Vec<String> = std::iter::repeat("..".to_string()).take(from_dir.len() - common).collect();
        out.extend(to[common..].iter().map(|s| s.to_string()));
        out.join("/")
    }

    pub fn target_of(&self, part: &str, kind_suffix: &str) -> Result<Option<String>> {
        Ok(self.rels(part)?.into_iter().find(|r| r.kind.ends_with(kind_suffix) && !r.external).map(|r| Self::resolve(part, &r.target)))
    }

    /// 包的主文档（_rels/.rels 里的 officeDocument）
    pub fn main_part(&self) -> Result<String> {
        let rels = self.rels("")?;
        rels.iter()
            .find(|r| r.kind == REL_OFFICE_DOCUMENT || r.kind.ends_with("/officeDocument"))
            .map(|r| r.target.trim_start_matches('/').to_string())
            .ok_or_else(|| EngineError::Format("找不到主文档".into()))
    }

    /// 新增一条关系，返回 rId
    pub fn add_rel(&mut self, part: &str, kind: &str, target: &str, external: bool) -> Result<String> {
        let path = Self::rels_path(part);
        let text = if self.has(&path) {
            self.read_str(&path)?
        } else {
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#
                .to_string()
        };
        let existing: HashSet<String> = self.rels(part).unwrap_or_default().into_iter().map(|r| r.id).collect();
        let mut n = existing.len() + 1;
        let id = loop {
            let id = format!("rId{n}");
            if !existing.contains(&id) {
                break id;
            }
            n += 1;
        };
        let mode = if external { r#" TargetMode="External""# } else { "" };
        let rel = format!(r#"<Relationship Id="{id}" Type="{kind}" Target="{}"{mode}/>"#, xml::escape_attr(target));
        let updated = insert_before_close(&text, "Relationships", &rel)?;
        self.write(&path, updated);
        Ok(id)
    }

    pub fn remove_rel(&mut self, part: &str, id: &str) -> Result<()> {
        let path = Self::rels_path(part);
        if !self.has(&path) {
            return Ok(());
        }
        let text = self.read_str(&path)?;
        let updated = {
            let doc = Xml::parse(&text)?;
            let Some(root) = doc.document_element() else { return Ok(()) };
            let Some(rel) = doc.children_named(root, "Relationship").find(|&r| doc.attr(r, "Id").as_deref() == Some(id)) else { return Ok(()) };
            let mut splice = xml::Splice::new();
            splice.replace(doc.node(rel).start, doc.node(rel).end, "");
            splice.apply(&text)?
        };
        self.write(&path, updated);
        Ok(())
    }

    pub fn content_type_override(&mut self, part: &str, content_type: &str) -> Result<()> {
        let text = self.read_str("[Content_Types].xml")?;
        let part_name = format!("/{part}");
        let updated = {
            let doc = Xml::parse(&text)?;
            let root = doc.document_element().ok_or_else(|| EngineError::Format("内容类型表为空".into()))?;
            if doc.children_named(root, "Override").any(|o| doc.attr(o, "PartName").as_deref() == Some(part_name.as_str())) {
                return Ok(());
            }
            let entry = format!(r#"<Override PartName="{}" ContentType="{content_type}"/>"#, xml::escape_attr(&part_name));
            insert_before_close(&text, "Types", &entry)?
        };
        self.write("[Content_Types].xml", updated);
        Ok(())
    }

    pub fn content_type_default(&mut self, ext: &str, content_type: &str) -> Result<()> {
        let text = self.read_str("[Content_Types].xml")?;
        let updated = {
            let doc = Xml::parse(&text)?;
            let root = doc.document_element().ok_or_else(|| EngineError::Format("内容类型表为空".into()))?;
            if doc.children_named(root, "Default").any(|o| doc.attr(o, "Extension").map(|e| e.eq_ignore_ascii_case(ext)).unwrap_or(false)) {
                return Ok(());
            }
            insert_before_close(&text, "Types", &format!(r#"<Default Extension="{ext}" ContentType="{content_type}"/>"#))?
        };
        self.write("[Content_Types].xml", updated);
        Ok(())
    }

    pub fn remove_content_type_override(&mut self, part: &str) -> Result<()> {
        let text = self.read_str("[Content_Types].xml")?;
        let part_name = format!("/{part}");
        let updated = {
            let doc = Xml::parse(&text)?;
            let Some(root) = doc.document_element() else { return Ok(()) };
            let Some(o) = doc.children_named(root, "Override").find(|&o| doc.attr(o, "PartName").as_deref() == Some(part_name.as_str())) else { return Ok(()) };
            let mut splice = xml::Splice::new();
            splice.replace(doc.node(o).start, doc.node(o).end, "");
            splice.apply(&text)?
        };
        self.write("[Content_Types].xml", updated);
        Ok(())
    }

    /// 在 prefix{n}.ext 里找一个没被占用的部件名
    pub fn unused_name(&self, prefix: &str, ext: &str) -> String {
        let names: HashSet<String> = self.names().into_iter().collect();
        (1..).map(|n| format!("{prefix}{n}.{ext}")).find(|n| !names.contains(n)).unwrap()
    }
}

/// 在根元素的结束标签前插入内容（根元素自闭合时展开）
pub fn insert_before_close(text: &str, root_name: &str, content: &str) -> Result<String> {
    let doc = Xml::parse(text)?;
    let root = doc.document_element().filter(|&r| doc.name(r) == root_name).ok_or_else(|| EngineError::Format(format!("缺少根元素 {root_name}")))?;
    let n = doc.node(root);
    let mut splice = xml::Splice::new();
    if n.self_closing {
        splice.replace(n.start, n.end, format!("{}{content}</{root_name}>", doc.open_tag_expanded(root)));
    } else {
        splice.insert(n.close_start, content);
    }
    splice.apply(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_targets() {
        assert_eq!(Package::resolve("word/document.xml", "media/a.png"), "word/media/a.png");
        assert_eq!(Package::resolve("xl/worksheets/sheet1.xml", "../drawings/drawing1.xml"), "xl/drawings/drawing1.xml");
        assert_eq!(Package::resolve("", "word/document.xml"), "word/document.xml");
        assert_eq!(Package::resolve("ppt/slides/slide1.xml", "/ppt/media/x.png"), "ppt/media/x.png");
        assert_eq!(Package::relative("xl/worksheets/sheet1.xml", "xl/drawings/drawing1.xml"), "../drawings/drawing1.xml");
        assert_eq!(Package::relative("xl/workbook.xml", "xl/worksheets/sheet2.xml"), "worksheets/sheet2.xml");
        assert_eq!(Package::rels_path("word/document.xml"), "word/_rels/document.xml.rels");
        assert_eq!(Package::rels_path(""), "_rels/.rels");
    }
}
