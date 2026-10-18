//! 插件目录：内置的常用 MCP 服务模板（地址与用法已按官方文档核实），用户只需填写所需字段。

use std::collections::{BTreeMap, HashMap};

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use super::{NamedValue, ServerInput};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub category: String,
    pub description: String,
    pub docs: String,
    pub transport: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// 值里的 {字段} 会被替换；引用的可选字段没填时整项省略
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub approval: Option<String>,
    /// 这个服务的工具都只读取数据（服务端没有标注时由目录声明）
    #[serde(default)]
    pub read_only: bool,
}

static ENTRIES: Lazy<Vec<Entry>> =
    Lazy::new(|| serde_json::from_str(include_str!("../../catalog/mcp.json")).expect("catalog/mcp.json 格式错误"));

pub fn entries() -> &'static [Entry] {
    &ENTRIES
}

pub fn find(id: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|e| e.id == id)
}

/// 替换模板里的 {字段}；引用的字段没填时返回 None
fn fill(template: &str, values: &HashMap<String, String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else { break };
        let key = &rest[start + 1..start + len];
        let value = values.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())?;
        out.push_str(&rest[..start]);
        out.push_str(value);
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn fill_all(map: &BTreeMap<String, String>, values: &HashMap<String, String>) -> Vec<NamedValue> {
    map.iter()
        .filter_map(|(name, template)| fill(template, values).map(|value| NamedValue { name: name.clone(), value }))
        .collect()
}

/// 用用户填写的字段生成插件输入
pub fn to_input(entry: &Entry, values: &HashMap<String, String>) -> Result<ServerInput, String> {
    for field in &entry.fields {
        if field.required && values.get(&field.key).map_or(true, |v| v.trim().is_empty()) {
            return Err(format!("请填写「{}」", field.label));
        }
    }
    Ok(ServerInput {
        name: entry.name.clone(),
        slug: Some(entry.slug.clone()),
        transport: entry.transport.clone(),
        url: entry.url.as_deref().and_then(|u| fill(u, values)),
        command: entry.command.clone(),
        args: entry.args.iter().filter_map(|a| fill(a, values)).collect(),
        description: Some(entry.description.clone()),
        headers: fill_all(&entry.headers, values),
        env: fill_all(&entry.env, values),
        enabled: Some(true),
        approval: entry.approval.clone(),
        assume_read_only: Some(entry.read_only),
        query: fill_all(&entry.query, values),
        catalog_id: Some(entry.id.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn catalog_parses_and_is_consistent() {
        assert!(entries().len() >= 6);
        for e in entries() {
            assert!(matches!(e.transport.as_str(), "http" | "sse" | "stdio"), "{}", e.id);
            assert!(super::super::slugify(&e.slug) == e.slug, "{} 的标识不合法", e.id);
            if e.transport == "stdio" {
                assert!(e.command.is_some(), "{}", e.id);
            } else {
                assert!(e.url.as_deref().is_some_and(|u| u.starts_with("https://")), "{}", e.id);
            }
            // 模板里引用的字段都要声明
            let keys: Vec<&str> = e.fields.iter().map(|f| f.key.as_str()).collect();
            for t in e.headers.values().chain(e.query.values()).chain(e.env.values()) {
                let key = t.split('{').nth(1).and_then(|r| r.split('}').next()).unwrap_or_default();
                assert!(key.is_empty() || keys.contains(&key), "{} 引用了未声明的字段 {key}", e.id);
            }
        }
    }

    #[test]
    fn builds_inputs_from_fields() {
        let amap = find("amap").unwrap();
        assert!(to_input(amap, &values(&[])).unwrap_err().contains("高德 Key"));
        let input = to_input(amap, &values(&[("AMAP_KEY", " k123 ")])).unwrap();
        assert_eq!(input.url.as_deref(), Some("https://mcp.amap.com/mcp"), "密钥不进地址");
        assert_eq!((input.query[0].name.as_str(), input.query[0].value.as_str()), ("key", "k123"));
        assert_eq!(input.catalog_id.as_deref(), Some("amap"));

        let c7 = find("context7").unwrap();
        assert!(to_input(c7, &values(&[])).unwrap().headers.is_empty(), "可选字段没填时省略请求头");
        assert_eq!(to_input(c7, &values(&[("CONTEXT7_API_KEY", "ck")])).unwrap().headers[0].value, "Bearer ck");

        let gh = to_input(find("github").unwrap(), &values(&[("GITHUB_PAT", "ghp_x")])).unwrap();
        assert_eq!(gh.headers[0].value, "Bearer ghp_x");
        assert_eq!(fill("a{X}b{Y}", &values(&[("X", "1"), ("Y", "2")])).as_deref(), Some("a1b2"));
    }
}
