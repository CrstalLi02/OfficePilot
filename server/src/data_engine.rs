//! 数据分析引擎：把工作区里的 csv / tsv / xlsx / xls / ods / json 文件注册成表，用进程内的
//! DataFusion（Arrow）执行只读 SQL。
//!
//! 安全边界：
//! - 每次分析新建独立的会话，只注册本次指定的文件；
//! - SQL 先做文本校验（单条 SELECT / WITH），再由 DataFusion 的 `SQLOptions` 禁止 DDL / DML / SET
//!   （例如 `CREATE EXTERNAL TABLE`、`COPY`），查询无法读取未注册的文件；
//! - 内存上限、超时、结果行数上限。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use datafusion::arrow::array::{Array, Float64Array};
use datafusion::arrow::compute::cast;
use datafusion::arrow::datatypes::DataType;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::arrow::util::display::array_value_to_string;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::prelude::{CsvReadOptions, SessionConfig, SessionContext, SQLOptions};
use serde::Serialize;
use serde_json::{json, Value};

use crate::db::file_repo::FileRow;
use crate::db::DbPool;
use crate::workspace::Tree;

/// 返回给调用方（产物）的最大行数
pub const MAX_RESULT_ROWS: usize = 10_000;
/// 查询超时
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// 查询可用内存
const MEMORY_LIMIT: usize = 512 * 1024 * 1024;
/// 单次分析最多加载的文件数
const MAX_FILES: usize = 8;
/// 单个文件大小上限
const MAX_FILE_BYTES: i64 = 100 * 1024 * 1024;

// ───────────────────────── SQL 校验 ─────────────────────────

/// 去掉注释，只允许单条 SELECT / WITH 查询；返回去掉结尾分号的 SQL
pub fn check_sql(sql: &str) -> Result<String, String> {
    let mut cleaned = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cleaned.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cleaned.push(c);
                }
                '-' if chars.peek() == Some(&'-') => {
                    for n in chars.by_ref() {
                        if n == '\n' {
                            cleaned.push('\n');
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    let mut prev = ' ';
                    for n in chars.by_ref() {
                        if prev == '*' && n == '/' {
                            break;
                        }
                        prev = n;
                    }
                    cleaned.push(' ');
                }
                ';' => {
                    if chars.clone().any(|n| !n.is_whitespace()) {
                        return Err("只能执行一条查询语句".to_string());
                    }
                }
                _ => cleaned.push(c),
            },
        }
    }
    if quote.is_some() {
        return Err("SQL 中的引号没有闭合".to_string());
    }
    let cleaned = cleaned.trim().to_string();
    let first = cleaned
        .split(|c: char| c.is_whitespace() || c == '(')
        .find(|w| !w.is_empty())
        .unwrap_or("")
        .to_ascii_uppercase();
    if first != "SELECT" && first != "WITH" {
        return Err("只允许 SELECT / WITH 查询".to_string());
    }
    Ok(cleaned)
}

// ───────────────────────── 文件转换成 CSV ─────────────────────────

/// 表名：保留字母（含中文）、数字和下划线；数字开头加前缀；与已有表名冲突时加序号
pub fn table_name(raw: &str, taken: &HashSet<String>) -> String {
    let mut name: String = raw
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if name.is_empty() {
        name = "data".to_string();
    }
    if name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        name = format!("t_{name}");
    }
    let mut candidate = name.clone();
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{name}_{n}");
        n += 1;
    }
    candidate
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// 表头：空的补成"列N"，重名的加序号
fn header_names(raw: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.into_iter()
        .enumerate()
        .map(|(i, h)| {
            let base = if h.trim().is_empty() { format!("列{}", i + 1) } else { h.trim().to_string() };
            let mut name = base.clone();
            let mut n = 2;
            while !seen.insert(name.clone()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect()
}

fn write_rows(header: Vec<String>, rows: impl Iterator<Item = Vec<String>>) -> String {
    let header = header_names(header);
    let width = header.len();
    let mut out = header.iter().map(|h| csv_escape(h)).collect::<Vec<_>>().join(",");
    out.push('\n');
    for mut row in rows {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        row.resize(width, String::new());
        out.push_str(&row.iter().take(width).map(|c| csv_escape(c)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

/// 文本解码：UTF-8（去 BOM），不是合法 UTF-8 时按 GBK 解码（国内 Excel 导出的 CSV 常见）
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::GBK.decode(bytes).0.into_owned(),
    }
}

fn excel_cell(cell: &calamine::Data) -> String {
    use calamine::Data;
    match cell {
        Data::Int(i) => i.to_string(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                f.to_string()
            }
        }
        Data::String(s) => s.clone(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(t) if t.time() == chrono::NaiveTime::MIN => t.format("%Y-%m-%d").to_string(),
            Some(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => dt.as_f64().to_string(),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(_) | Data::Empty => String::new(),
    }
}

/// 每个 sheet 转成一份 CSV：(sheet 名, csv 文本)
pub fn excel_to_csv(path: &Path) -> Result<Vec<(String, String)>, String> {
    use calamine::Reader;
    let mut workbook = calamine::open_workbook_auto(path).map_err(|e| format!("无法打开表格：{e}"))?;
    let mut out = Vec::new();
    for sheet in workbook.sheet_names() {
        let range = workbook.worksheet_range(&sheet).map_err(|e| format!("读取 sheet {sheet} 失败：{e}"))?;
        let mut rows = range.rows().map(|r| r.iter().map(excel_cell).collect::<Vec<_>>());
        // 跳过开头的空行，第一行非空行作为表头
        let header = loop {
            match rows.next() {
                Some(row) if row.iter().all(|c| c.trim().is_empty()) => continue,
                Some(row) => break Some(row),
                None => break None,
            }
        };
        if let Some(header) = header {
            out.push((sheet, write_rows(header, rows)));
        }
    }
    Ok(out)
}

/// JSON 数组（对象数组，或 {"data": [...]} 这类包装）转成 CSV
pub fn json_to_csv(text: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("JSON 解析失败：{e}"))?;
    let items = match &value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => map
            .values()
            .find_map(|v| v.as_array().cloned())
            .ok_or("JSON 里没有数组数据")?,
        _ => return Err("JSON 需要是对象数组".to_string()),
    };
    let mut columns: Vec<String> = Vec::new();
    for item in &items {
        if let Value::Object(map) = item {
            for key in map.keys() {
                if !columns.contains(key) {
                    columns.push(key.clone());
                }
            }
        }
    }
    if columns.is_empty() {
        return Err("JSON 数组里没有对象".to_string());
    }
    let rows = items.iter().map(|item| {
        columns
            .iter()
            .map(|c| match item.get(c) {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
            })
            .collect()
    });
    Ok(write_rows(columns.clone(), rows))
}

// ───────────────────────── 会话 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TableInfo {
    pub table: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    pub columns: Vec<(String, String)>,
}

/// 临时目录，用完删除
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct DataSession {
    ctx: SessionContext,
    pub tables: Vec<TableInfo>,
    _temp: TempDir,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    /// 每列是否为数值
    pub numeric: Vec<bool>,
    pub rows: Vec<Vec<Value>>,
    /// 结果是否超过 MAX_RESULT_ROWS 被截断
    pub truncated: bool,
}

fn new_context() -> Result<SessionContext, String> {
    let runtime = RuntimeEnvBuilder::new()
        .with_memory_limit(MEMORY_LIMIT, 1.0)
        .build_arc()
        .map_err(|e| e.to_string())?;
    let config = SessionConfig::new().with_target_partitions(2).with_information_schema(false);
    Ok(SessionContext::new_with_config_rt(config, runtime))
}

impl DataSession {
    /// 用一组已转换好的 CSV 建立会话（测试和工具共用）
    pub async fn from_csv(sources: Vec<(String, Option<String>, String)>) -> Result<Self, String> {
        let temp = TempDir(std::env::temp_dir().join("walioffice-data").join(uuid::Uuid::new_v4().to_string()));
        std::fs::create_dir_all(&temp.0).map_err(|e| e.to_string())?;
        let ctx = new_context()?;
        let mut taken = HashSet::new();
        let mut tables = Vec::new();
        for (source, sheet, csv) in sources {
            let stem = Path::new(&source).file_stem().and_then(|s| s.to_str()).unwrap_or("data").to_string();
            let raw = match &sheet {
                Some(sheet) => format!("{stem}_{sheet}"),
                None => stem,
            };
            let table = table_name(&raw, &taken);
            taken.insert(table.clone());
            let path = temp.0.join(format!("{}.csv", tables.len()));
            std::fs::write(&path, csv).map_err(|e| e.to_string())?;
            ctx.register_csv(
                format!("\"{table}\""),
                path.to_string_lossy(),
                CsvReadOptions::new().has_header(true).schema_infer_max_records(2000),
            )
            .await
            .map_err(|e| format!("加载 {source} 失败：{e}"))?;
            let schema = ctx
                .table(format!("\"{table}\""))
                .await
                .map_err(|e| e.to_string())?
                .schema()
                .as_arrow()
                .clone();
            let columns = schema.fields().iter().map(|f| (f.name().clone(), type_label(f.data_type()))).collect();
            tables.push(TableInfo { table, source, sheet, columns });
        }
        Ok(Self { ctx, tables, _temp: temp })
    }

    /// 从工作区文件建立会话
    pub async fn open(pool: &DbPool, owner_id: &str, keys: &[String]) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("请至少指定一个数据文件（csv / xlsx / json）".to_string());
        }
        if keys.len() > MAX_FILES {
            return Err(format!("一次最多分析 {MAX_FILES} 个文件"));
        }
        let tree = Tree::load(pool, owner_id).await.map_err(|e| e.to_string())?;
        let mut sources = Vec::new();
        for key in keys {
            let file: FileRow = tree
                .find_file(key)?
                .cloned()
                .ok_or_else(|| format!("找不到文件：{key}"))?;
            if file.file_size > MAX_FILE_BYTES {
                return Err(format!("{} 超过 100MB，暂不支持", file.name));
            }
            let path = tree.file_path(&file);
            let ext = file.name.rsplit('.').next().unwrap_or("").to_lowercase();
            let disk = PathBuf::from(&file.file_path);
            match ext.as_str() {
                "xlsx" | "xlsm" | "xls" | "xlsb" | "ods" => {
                    let sheets = tokio::task::spawn_blocking(move || excel_to_csv(&disk)).await.map_err(|e| e.to_string())??;
                    if sheets.is_empty() {
                        return Err(format!("{path} 没有数据"));
                    }
                    let single = sheets.len() == 1;
                    for (sheet, csv) in sheets {
                        sources.push((path.clone(), (!single).then_some(sheet), csv));
                    }
                }
                "csv" | "tsv" | "txt" => {
                    let bytes = tokio::fs::read(&disk).await.map_err(|e| format!("读取 {path} 失败：{e}"))?;
                    let text = decode_text(&bytes);
                    let csv = if ext == "tsv" || (ext == "txt" && text.lines().next().is_some_and(|l| l.contains('\t'))) {
                        // TSV 统一转成 CSV
                        let mut lines = text.lines().map(|l| l.split('\t').map(str::to_string).collect::<Vec<_>>());
                        let header = lines.next().unwrap_or_default();
                        write_rows(header, lines)
                    } else {
                        text
                    };
                    sources.push((path, None, csv));
                }
                "json" => {
                    let bytes = tokio::fs::read(&disk).await.map_err(|e| format!("读取 {path} 失败：{e}"))?;
                    sources.push((path, None, json_to_csv(&decode_text(&bytes))?));
                }
                _ => return Err(format!("{path} 不是数据文件；支持 csv / tsv / xlsx / xls / ods / json")),
            }
        }
        Self::from_csv(sources).await
    }

    /// 执行只读查询
    pub async fn query(&self, sql: &str) -> Result<QueryResult, String> {
        let sql = check_sql(sql)?;
        let options = SQLOptions::new().with_allow_ddl(false).with_allow_dml(false).with_allow_statements(false);
        let run = async {
            let df = self.ctx.sql_with_options(&sql, options).await?;
            df.limit(0, Some(MAX_RESULT_ROWS + 1))?.collect().await
        };
        let batches = tokio::time::timeout(QUERY_TIMEOUT, run)
            .await
            .map_err(|_| format!("查询超过 {} 秒，已中断；请缩小范围或先聚合", QUERY_TIMEOUT.as_secs()))?
            .map_err(|e| friendly_error(&e.to_string()))?;
        Ok(batches_to_result(&batches))
    }

    /// 表的基本画像：行数，每列的类型、空值数、去重数、最小值、最大值，以及样例行
    pub async fn profile(&self) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for table in &self.tables {
            let quoted = format!("\"{}\"", table.table);
            let mut aggregates = vec!["COUNT(*) AS \"__rows\"".to_string()];
            for (i, (name, _)) in table.columns.iter().take(40).enumerate() {
                let col = format!("\"{}\"", name.replace('"', "\"\""));
                aggregates.push(format!("COUNT({col}) AS \"n{i}\""));
                aggregates.push(format!("COUNT(DISTINCT {col}) AS \"d{i}\""));
                aggregates.push(format!("CAST(MIN({col}) AS VARCHAR) AS \"min{i}\""));
                aggregates.push(format!("CAST(MAX({col}) AS VARCHAR) AS \"max{i}\""));
            }
            let stats = self.query(&format!("SELECT {} FROM {quoted}", aggregates.join(", "))).await?;
            let row = stats.rows.first().cloned().unwrap_or_default();
            let get = |name: &str| stats.columns.iter().position(|c| c == name).and_then(|i| row.get(i)).cloned().unwrap_or(Value::Null);
            let total = get("__rows").as_f64().unwrap_or(0.0) as i64;
            let columns: Vec<Value> = table
                .columns
                .iter()
                .take(40)
                .enumerate()
                .map(|(i, (name, ty))| {
                    let non_null = get(&format!("n{i}")).as_f64().unwrap_or(0.0) as i64;
                    json!({
                        "name": name,
                        "type": ty,
                        "nulls": total - non_null,
                        "distinct": get(&format!("d{i}")),
                        "min": get(&format!("min{i}")),
                        "max": get(&format!("max{i}")),
                    })
                })
                .collect();
            let sample = self.query(&format!("SELECT * FROM {quoted} LIMIT 5")).await?;
            out.push(json!({
                "table": table.table,
                "source": table.source,
                "sheet": table.sheet,
                "rows": total,
                "columns": columns,
                "sample": { "columns": sample.columns, "rows": sample.rows },
            }));
        }
        Ok(out)
    }
}

fn type_label(t: &DataType) -> String {
    match t {
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => "整数".into(),
        DataType::Float16 | DataType::Float32 | DataType::Float64 | DataType::Decimal128(..) | DataType::Decimal256(..) => "小数".into(),
        DataType::Boolean => "布尔".into(),
        DataType::Date32 | DataType::Date64 => "日期".into(),
        DataType::Timestamp(..) => "时间".into(),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "文本".into(),
        other => other.to_string(),
    }
}

/// 把 DataFusion 的报错改写成模型容易修正的提示
fn friendly_error(e: &str) -> String {
    if e.contains("not found") && e.contains("table") {
        format!("{e}。表名请用双引号括起来，并使用 data_profile 返回的表名")
    } else if e.contains("No field named") || e.contains("Schema error") {
        format!("{e}。列名区分大小写，请用双引号括起来，例如 \"销售额\"")
    } else if e.contains("not supported") || e.contains("Unsupported") {
        format!("不允许的操作：{e}")
    } else {
        e.to_string()
    }
}

fn json_number(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9e15 {
        json!(v as i64)
    } else {
        json!((v * 1e6).round() / 1e6)
    }
}

pub fn batches_to_result(batches: &[RecordBatch]) -> QueryResult {
    let Some(first) = batches.first() else {
        return QueryResult { columns: vec![], numeric: vec![], rows: vec![], truncated: false };
    };
    let schema = first.schema();
    let columns: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
    let numeric: Vec<bool> = schema.fields().iter().map(|f| f.data_type().is_numeric()).collect();
    let mut rows = Vec::new();
    'outer: for batch in batches {
        let casted: Vec<Option<Arc<dyn Array>>> = batch
            .columns()
            .iter()
            .zip(&numeric)
            .map(|(col, is_num)| if *is_num { cast(col, &DataType::Float64).ok() } else { None })
            .collect();
        for r in 0..batch.num_rows() {
            if rows.len() >= MAX_RESULT_ROWS {
                break 'outer;
            }
            let row = batch
                .columns()
                .iter()
                .enumerate()
                .map(|(c, col)| {
                    if col.is_null(r) {
                        return Value::Null;
                    }
                    if let Some(Some(f)) = casted.get(c) {
                        if let Some(arr) = f.as_any().downcast_ref::<Float64Array>() {
                            return json_number(arr.value(r));
                        }
                    }
                    Value::String(array_value_to_string(col.as_ref(), r).unwrap_or_default())
                })
                .collect();
            rows.push(row);
        }
    }
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    QueryResult { columns, numeric, rows, truncated: total > MAX_RESULT_ROWS }
}

/// 结果的 Markdown 表格（给模型看，最多 max_rows 行）
pub fn markdown_table(result: &QueryResult, max_rows: usize) -> String {
    if result.columns.is_empty() {
        return "（无结果）".to_string();
    }
    let cell = |v: &Value| match v {
        Value::Null => String::new(),
        Value::String(s) => s.replace('|', "\\|").replace('\n', " "),
        other => other.to_string(),
    };
    let mut out = format!("| {} |\n|{}|\n", result.columns.join(" | "), vec!["---"; result.columns.len()].join("|"));
    for row in result.rows.iter().take(max_rows) {
        out.push_str(&format!("| {} |\n", row.iter().map(cell).collect::<Vec<_>>().join(" | ")));
    }
    if result.rows.len() > max_rows {
        out.push_str(&format!("…共 {} 行{}，只显示前 {max_rows} 行\n", result.rows.len(), if result.truncated { "（已截断）" } else { "" }));
    }
    out
}

// ───────────────────────── 图表 ─────────────────────────

/// 用查询结果生成 ECharts 图表产物内容（数据完全来自查询，不经过模型）
pub fn chart_content(result: &QueryResult, spec: &Value, fallback_title: &str) -> Result<(String, Value), String> {
    let chart_type = spec["type"].as_str().unwrap_or("bar").to_lowercase();
    if !matches!(chart_type.as_str(), "bar" | "line" | "pie" | "scatter" | "funnel") {
        return Err(format!("不支持的图表类型 {chart_type}，可选 bar / line / pie / scatter / funnel"));
    }
    let col_index = |name: &str| {
        result
            .columns
            .iter()
            .position(|c| c == name)
            .ok_or_else(|| format!("结果中没有列「{name}」，可用列：{}", result.columns.join("、")))
    };
    let x_name = spec["x"].as_str().map(str::to_string).unwrap_or_else(|| result.columns.first().cloned().unwrap_or_default());
    let x = col_index(&x_name)?;
    let y_names: Vec<String> = match &spec["y"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => result
            .columns
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != x && result.numeric[*i])
            .map(|(_, c)| c.clone())
            .collect(),
    };
    if y_names.is_empty() {
        return Err("没有可作为数值的列，请在 chart.y 指定数值列".to_string());
    }
    let ys = y_names.iter().map(|n| col_index(n)).collect::<Result<Vec<_>, _>>()?;
    let rows: Vec<&Vec<Value>> = result.rows.iter().take(500).collect();
    let labels: Vec<String> = rows
        .iter()
        .map(|r| match &r[x] {
            Value::String(s) => s.clone(),
            Value::Null => "（空）".to_string(),
            other => other.to_string(),
        })
        .collect();
    let series_values: Vec<Vec<f64>> = ys
        .iter()
        .map(|&y| rows.iter().map(|r| r[y].as_f64().or_else(|| r[y].as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)).collect())
        .collect();
    let title = spec["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or(fallback_title).to_string();

    let option = match chart_type.as_str() {
        "pie" | "funnel" => json!({
            "title": { "text": title, "left": "center" },
            "tooltip": { "trigger": "item" },
            "legend": { "bottom": 0, "type": "scroll" },
            "series": [{
                "name": y_names[0],
                "type": chart_type,
                "radius": if chart_type == "pie" { json!(["40%", "68%"]) } else { Value::Null },
                "data": labels.iter().zip(&series_values[0]).map(|(name, v)| json!({ "name": name, "value": v })).collect::<Vec<_>>(),
            }],
        }),
        _ => json!({
            "title": { "text": title, "left": "center" },
            "tooltip": { "trigger": "axis" },
            "legend": { "top": 36, "type": "scroll" },
            "grid": { "left": 48, "right": 24, "top": 76, "bottom": 48, "containLabel": true },
            "xAxis": { "type": "category", "data": labels, "axisLabel": { "rotate": if labels.len() > 8 { 30 } else { 0 } } },
            "yAxis": { "type": "value" },
            "series": y_names.iter().zip(&series_values).map(|(name, values)| json!({
                "name": name,
                "type": chart_type,
                "smooth": chart_type == "line",
                "data": values,
            })).collect::<Vec<_>>(),
        }),
    };
    let content = json!({
        "type": chart_type,
        "chart_type": chart_type,
        "title": title,
        "summary": spec["summary"].as_str(),
        "source": "data_query",
        "chart_data": { "labels": labels, "values": series_values[0], "seriesName": y_names[0] },
        "option": option,
    });
    Ok((title, content))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALES: &str = "区域,月份,销售额,订单数\n华东,2026-01,1200.5,30\n华东,2026-02,1500,35\n华北,2026-01,800,20\n华北,2026-02,950.25,22\n华南,2026-01,,15\n";

    async fn session() -> DataSession {
        DataSession::from_csv(vec![("/数据/2026 销售.csv".into(), None, SALES.into())]).await.unwrap()
    }

    #[test]
    fn sql_guard_allows_only_single_select() {
        assert_eq!(check_sql("  select 1; ").unwrap(), "select 1");
        assert!(check_sql("WITH t AS (SELECT 1) SELECT * FROM t").is_ok());
        assert!(check_sql("(SELECT 1)").is_ok());
        assert!(check_sql("SELECT 1; DROP TABLE x").is_err());
        assert!(check_sql("DELETE FROM x").is_err());
        assert!(check_sql("COPY (SELECT 1) TO '/tmp/x.csv'").is_err());
        assert!(check_sql("-- 注释\nCREATE EXTERNAL TABLE x STORED AS CSV LOCATION '/etc/passwd'").is_err());
        assert!(check_sql("/* SELECT */ SET x = 1").is_err());
        // 字符串里的分号和注释符号不受影响
        assert_eq!(check_sql("SELECT 'a;b -- c' AS s").unwrap(), "SELECT 'a;b -- c' AS s");
        assert!(check_sql("SELECT 'unclosed").is_err());
    }

    #[test]
    fn table_names_and_headers_are_normalized() {
        let mut taken = HashSet::new();
        assert_eq!(table_name("2026 销售-明细", &taken), "t_2026_销售_明细");
        taken.insert("订单".to_string());
        assert_eq!(table_name("订单", &taken), "订单_2");
        assert_eq!(table_name("***", &taken), "data");
        assert_eq!(header_names(vec!["a".into(), "".into(), "a".into()]), vec!["a", "列2", "a_2"]);
        assert_eq!(csv_escape("x,\"y\""), "\"x,\"\"y\"\"\"");
    }

    #[test]
    fn decodes_gbk_and_bom() {
        let (gbk, _, _) = encoding_rs::GBK.encode("区域,金额\n华东,1\n");
        assert_eq!(decode_text(&gbk), "区域,金额\n华东,1\n");
        assert_eq!(decode_text("\u{feff}a,b".as_bytes()), "a,b");
    }

    #[test]
    fn json_arrays_become_csv() {
        let csv = json_to_csv(r#"{"data":[{"name":"a","v":1},{"name":"b,c","extra":true}]}"#).unwrap();
        assert_eq!(csv, "name,v,extra\na,1,\n\"b,c\",,true\n");
        assert!(json_to_csv("[1,2]").is_err());
    }

    #[tokio::test]
    async fn query_aggregates_with_types() {
        let s = session().await;
        assert_eq!(s.tables[0].table, "t_2026_销售");
        let result = s
            .query(r#"SELECT "区域", SUM("销售额") AS "总额", SUM("订单数") AS "订单" FROM "t_2026_销售" GROUP BY "区域" ORDER BY "总额" DESC NULLS LAST"#)
            .await
            .unwrap();
        assert_eq!(result.columns, ["区域", "总额", "订单"]);
        assert_eq!(result.numeric, [false, true, true]);
        assert_eq!(result.rows[0], vec![json!("华东"), json!(2700.5), json!(65)]);
        assert_eq!(result.rows[2], vec![json!("华南"), Value::Null, json!(15)]);
        assert!(markdown_table(&result, 2).contains("…共 3 行"));
    }

    #[tokio::test]
    async fn writes_and_file_access_are_rejected() {
        let s = session().await;
        for sql in [
            "CREATE EXTERNAL TABLE x STORED AS CSV LOCATION '/etc/hosts'",
            "COPY (SELECT 1) TO '/tmp/walioffice-x.csv'",
            "INSERT INTO \"t_2026_销售\" VALUES ('a','b',1,1)",
            "SET datafusion.execution.batch_size = 1",
        ] {
            assert!(s.query(sql).await.is_err(), "{sql}");
        }
        // 只能看到本次注册的表
        assert!(s.query("SELECT * FROM information_schema.tables").await.is_err());
        let err = s.query("SELECT * FROM 销售").await.unwrap_err();
        assert!(err.contains("双引号"), "{err}");
    }

    #[tokio::test]
    async fn profile_reports_nulls_distinct_and_range() {
        let s = session().await;
        let profile = s.profile().await.unwrap();
        let table = &profile[0];
        assert_eq!(table["rows"], 5);
        let sales = table["columns"].as_array().unwrap().iter().find(|c| c["name"] == "销售额").unwrap();
        assert_eq!(sales["nulls"], 1);
        assert_eq!(sales["type"], "小数");
        assert_eq!(sales["max"], "1500.0");
        assert_eq!(table["sample"]["rows"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn chart_uses_query_values() {
        let s = session().await;
        let result = s
            .query(r#"SELECT "月份", SUM("销售额") AS "销售额", SUM("订单数") AS "订单数" FROM "t_2026_销售" GROUP BY "月份" ORDER BY "月份""#)
            .await
            .unwrap();
        let (title, content) = chart_content(&result, &json!({"type": "line", "x": "月份", "title": "月度趋势"}), "查询结果").unwrap();
        assert_eq!(title, "月度趋势");
        assert_eq!(content["chart_data"]["labels"], json!(["2026-01", "2026-02"]));
        assert_eq!(content["chart_data"]["values"], json!([2000.5, 2450.25]));
        assert_eq!(content["option"]["series"].as_array().unwrap().len(), 2, "未指定 y 时所有数值列都作为系列");
        let (_, pie) = chart_content(&result, &json!({"type": "pie", "x": "月份", "y": "订单数"}), "t").unwrap();
        assert_eq!(pie["option"]["series"][0]["data"][0], json!({"name": "2026-01", "value": 65.0}));
        assert!(chart_content(&result, &json!({"x": "不存在"}), "t").is_err());
        assert!(chart_content(&result, &json!({"type": "radar"}), "t").is_err());
    }

    #[test]
    fn excel_sheets_become_csv() {
        // 用 rust_xlsxwriter 生成一个两 sheet 的表格
        let path = std::env::temp_dir().join(format!("walioffice-test-{}.xlsx", uuid::Uuid::new_v4()));
        let mut wb = rust_xlsxwriter::Workbook::new();
        let s1 = wb.add_worksheet().set_name("一月").unwrap();
        s1.write(1, 0, "产品").unwrap();
        s1.write(1, 1, "数量").unwrap();
        s1.write(2, 0, "A,B").unwrap();
        s1.write(2, 1, 3).unwrap();
        let s2 = wb.add_worksheet().set_name("二月").unwrap();
        s2.write(0, 0, "产品").unwrap();
        s2.write(1, 0, "C").unwrap();
        wb.save(&path).unwrap();
        let sheets = excel_to_csv(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0], ("一月".to_string(), "产品,数量\n\"A,B\",3\n".to_string()));
        assert_eq!(sheets[1].1, "产品\nC\n");
    }
}
