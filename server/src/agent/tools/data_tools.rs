//! 数据分析工具：data_profile（看结构）、data_query（只读 SQL，结果可直接生成表格 / 图表产物）

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::tool::{OfficeTool, ToolArtifact, ToolContext, ToolResult};
use crate::data_engine::{chart_content, markdown_table, DataSession, QueryResult};

fn file_keys(input: &Value) -> Vec<String> {
    match &input["files"] {
        Value::Array(items) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Value::String(s) => vec![s.clone()],
        _ => vec![],
    }
}

fn progress(ctx: &ToolContext, step: &str, detail: String) {
    ctx.send("state_update", json!({ "phase": "running", "step": step, "detail": detail, "at": chrono::Utc::now().to_rfc3339() }));
}

async fn open(ctx: &ToolContext, input: &Value) -> Result<DataSession, String> {
    DataSession::open(&crate::state::db_pool(), &ctx.user_id, &file_keys(input)).await
}

/// 告诉模型本次注册了哪些表（表名需加双引号）
fn tables_note(session: &DataSession) -> String {
    session
        .tables
        .iter()
        .map(|t| {
            let sheet = t.sheet.as_deref().map(|s| format!(" · sheet {s}")).unwrap_or_default();
            format!("\"{}\"（{}{sheet}）", t.table, t.source)
        })
        .collect::<Vec<_>>()
        .join("、")
}

const FILES_PARAM: &str = "数据文件的路径（如 /销售/2026Q3.xlsx）或文件 id，可多个；支持 csv / tsv / xlsx / xls / ods / json。Excel 的每个 sheet 是一张表";

// ───────────────────────── data_profile ─────────────────────────

pub struct DataProfileTool;

#[async_trait]
impl OfficeTool for DataProfileTool {
    fn name(&self) -> &str {
        "data_profile"
    }
    fn description(&self) -> &str {
        "查看数据文件的结构和概况：每张表的行数、列名与类型、空值数、去重数、最小 / 最大值和样例行。用户提供了 Excel / CSV 等数据文件并要求分析、统计、做图表或写数据报告时，先调用它了解数据，再用 data_query 计算。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn produces_artifact(&self) -> bool {
        false
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": { "type": "array", "items": { "type": "string" }, "description": FILES_PARAM }
            },
            "required": ["files"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        progress(ctx, "读取数据", format!("正在加载 {} 个数据文件", file_keys(&input).len()));
        let session = match open(ctx, &input).await {
            Ok(s) => s,
            Err(e) => return ToolResult::err(e),
        };
        let profile = match session.profile().await {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        let mut text = String::new();
        for (table, info) in profile.iter().zip(&session.tables) {
            let sheet = info.sheet.as_deref().map(|s| format!(" · sheet {s}")).unwrap_or_default();
            text.push_str(&format!("表 \"{}\"（{}{sheet}），{} 行\n", info.table, info.source, table["rows"]));
            for c in table["columns"].as_array().into_iter().flatten() {
                let range = match (c["min"].as_str(), c["max"].as_str()) {
                    (Some(min), Some(max)) => format!("，范围 {} ~ {}", min.chars().take(24).collect::<String>(), max.chars().take(24).collect::<String>()),
                    _ => String::new(),
                };
                text.push_str(&format!(
                    "- \"{}\"：{}，空值 {}，去重 {}{range}\n",
                    c["name"].as_str().unwrap_or(""),
                    c["type"].as_str().unwrap_or(""),
                    c["nulls"],
                    c["distinct"]
                ));
            }
            let sample = QueryResult {
                columns: table["sample"]["columns"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                numeric: vec![],
                rows: serde_json::from_value(table["sample"]["rows"].clone()).unwrap_or_default(),
                truncated: false,
            };
            text.push_str(&format!("样例：\n{}\n", markdown_table(&sample, 5)));
        }
        text.push_str("用 data_query 查询时，表名和列名都要用双引号括起来。");
        ToolResult::ok(text, vec![]).with_data(json!({ "tables": profile }))
    }
}

// ───────────────────────── data_query ─────────────────────────

pub struct DataQueryTool;

fn sheet_artifact(title: &str, sql: &str, result: &QueryResult) -> ToolArtifact {
    let rows: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|r| {
            r.iter()
                .map(|v| match v {
                    Value::Null => String::new(),
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .collect();
    ToolArtifact {
        kind: "sheet".into(),
        title: title.to_string(),
        content: json!({
            "type": "sheet",
            "title": title,
            "tables": [{ "title": title, "headers": result.columns, "rows": rows, "summary": format!("查询：{sql}") }],
            "summary": format!("由 SQL 查询得到，共 {} 行{}", result.rows.len(), if result.truncated { "（已截断）" } else { "" }),
            "source": "data_query",
            "sql": sql,
        }),
    }
}

#[async_trait]
impl OfficeTool for DataQueryTool {
    fn name(&self) -> &str {
        "data_query"
    }
    fn description(&self) -> &str {
        "对数据文件执行只读 SQL（PostgreSQL 风格，单条 SELECT / WITH），返回真实计算结果；可同时用结果生成图表（chart）和表格产物。数据报告、PPT、图表里的数字都应来自这里的查询结果，不要估算或编造。表名和列名必须用双引号括起来（先用 data_profile 查看表名和列名）。互不依赖的多个查询可以在同一轮并行调用。"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": { "type": "array", "items": { "type": "string" }, "description": FILES_PARAM },
                "sql": { "type": "string", "description": "只读 SQL，例如 SELECT \"区域\", SUM(\"销售额\") AS \"总额\" FROM \"销售\" GROUP BY \"区域\" ORDER BY \"总额\" DESC" },
                "title": { "type": "string", "description": "这次查询的标题，用作表格 / 图表产物的名称" },
                "chart": {
                    "type": "object",
                    "description": "可选：用查询结果生成图表",
                    "properties": {
                        "type": { "type": "string", "enum": ["bar", "line", "pie", "scatter", "funnel"] },
                        "x": { "type": "string", "description": "作为横轴 / 分类的列名，默认第一列" },
                        "y": { "description": "数值列名或列名数组，默认所有数值列", "anyOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] },
                        "title": { "type": "string" },
                        "summary": { "type": "string", "description": "一句话结论，显示在图表上方" }
                    }
                },
                "save_table": { "type": "boolean", "description": "是否把结果保存为表格产物；默认结果超过 20 行时保存" }
            },
            "required": ["files", "sql"]
        })
    }
    async fn call(&self, input: Value, ctx: &ToolContext) -> ToolResult {
        let sql = input["sql"].as_str().unwrap_or("").trim().to_string();
        let title = input["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("查询结果").to_string();
        progress(ctx, "数据查询", format!("正在计算：{title}"));
        let session = match open(ctx, &input).await {
            Ok(s) => s,
            Err(e) => return ToolResult::err(e),
        };
        let result = match session.query(&sql).await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("{e}\n本次可用的表：{}", tables_note(&session))),
        };

        let mut artifacts = Vec::new();
        let mut notes = Vec::new();
        if let Some(spec) = input.get("chart").filter(|c| c.is_object()) {
            match chart_content(&result, spec, &title) {
                Ok((chart_title, content)) => {
                    artifacts.push(ToolArtifact { kind: "chart".into(), title: chart_title.clone(), content });
                    notes.push(format!("已生成图表《{chart_title}》"));
                }
                Err(e) => notes.push(format!("图表未生成：{e}")),
            }
        }
        let save_table = input["save_table"].as_bool().unwrap_or(result.rows.len() > 20);
        if save_table && !result.rows.is_empty() {
            artifacts.push(sheet_artifact(&title, &sql, &result));
            notes.push(format!("已保存表格《{title}》（{} 行，可导出 Excel）", result.rows.len()));
        }

        let observation = format!(
            "{title}：共 {} 行{}\n{}{}",
            result.rows.len(),
            if result.truncated { "（超过上限已截断）" } else { "" },
            markdown_table(&result, 30),
            if notes.is_empty() { String::new() } else { format!("\n{}", notes.join("；")) }
        );
        ToolResult::ok(observation, artifacts).with_data(json!({
            "sql": sql,
            "columns": result.columns,
            "row_count": result.rows.len(),
            "truncated": result.truncated,
            "summary": format!("{} 行结果", result.rows.len()),
        }))
    }
}
