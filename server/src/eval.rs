//! 离线路由评测：`walioffice eval [options]`
//!
//! 对每条用例，用线上同一套逻辑（`resolve_routing` + `build_initial_request`）组装第一轮请求，
//! 调用一次 LLM，只检查模型选择了哪些工具（不实际执行工具），统计准确率并输出报告。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::agent::agent_loop::{build_initial_request, InitialRequest};
use crate::agent::plan::AgentMode;
use crate::agent::AgentConfig;
use crate::llm::LlmClient;
use crate::models::{ChatAttachment, ChatMessage, ChatRequest};
use crate::routes::chat::{build_user_message, resolve_routing, Routing};

/// 1x1 PNG，用作图片附件占位
const PLACEHOLDER_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

#[derive(Debug, Clone, Deserialize)]
struct EvalCase {
    id: String,
    category: String,
    message: String,
    #[serde(default)]
    tool_kind: Option<String>,
    #[serde(default)]
    attachments: Vec<CaseAttachment>,
    #[serde(default)]
    history: Vec<CaseMessage>,
    /// 必须全部调用
    #[serde(default)]
    expect: Vec<String>,
    /// 至少调用其中一个
    #[serde(default)]
    expect_any: Vec<String>,
    /// 不允许调用
    #[serde(default)]
    forbid: Vec<String>,
    /// 必须直接回复文本、不调用任何工具
    #[serde(default)]
    expect_no_tool: bool,
    /// 需要 --mcp 指定的插件（工具名前缀 mcp__demo__）；未指定时跳过
    #[serde(default)]
    requires_mcp: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct CaseAttachment {
    kind: String,
    name: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CaseMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct Attempt {
    passed: bool,
    /// 严格判定未通过，但第一轮只做了合理的准备步骤（加载技能、找 / 读文件、搜索、提交计划），
    /// 且没有调用任何禁止的工具。报告里单独统计，不计入严格通过率
    #[serde(default)]
    prep: bool,
    tools: Vec<String>,
    tool_call_count: usize,
    latency_ms: u128,
    reason: String,
}

#[derive(Debug, Clone, Serialize)]
struct CaseReport {
    id: String,
    category: String,
    message: String,
    exposed_tools: Vec<String>,
    attempts: Vec<Attempt>,
}

impl CaseReport {
    fn pass_rate(&self) -> f64 {
        if self.attempts.is_empty() {
            return 0.0;
        }
        self.attempts.iter().filter(|a| a.passed).count() as f64 / self.attempts.len() as f64
    }
}

struct Options {
    cases: PathBuf,
    model: Option<String>,
    repeat: usize,
    concurrency: usize,
    filter: Option<String>,
    out: PathBuf,
    label: Option<String>,
    mode: AgentMode,
    /// 评测时接入的 MCP 服务地址（Streamable HTTP，路径以 /sse 结尾时按旧版 SSE）
    mcp: Option<String>,
    /// 访问 MCP 服务的请求头，格式 "Name: value"
    mcp_header: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut opts = Options {
        cases: PathBuf::from("evals/routing.jsonl"),
        model: None,
        repeat: 1,
        concurrency: 4,
        filter: None,
        out: PathBuf::from("evals/reports"),
        label: None,
        mode: AgentMode::Craft,
        mcp: None,
        mcp_header: None,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .ok_or_else(|| anyhow!("参数 {arg} 缺少取值"))
        };
        match arg.as_str() {
            "--cases" => opts.cases = PathBuf::from(value()?),
            "--model" => opts.model = Some(value()?),
            "--repeat" => opts.repeat = value()?.parse().context("--repeat")?,
            "--concurrency" => opts.concurrency = value()?.parse().context("--concurrency")?,
            "--filter" => opts.filter = Some(value()?),
            "--out" => opts.out = PathBuf::from(value()?),
            "--label" => opts.label = Some(value()?),
            "--mcp" => opts.mcp = Some(value()?),
            "--mcp-header" => opts.mcp_header = Some(value()?),
            "--mode" => {
                opts.mode = serde_json::from_value(serde_json::Value::String(value()?))
                    .context("--mode 只能是 craft / plan / ask")?
            }
            "-h" | "--help" => {
                println!(
                    "用法: walioffice eval [--cases evals/routing.jsonl] [--model NAME] [--repeat N]\n\
                     \x20                     [--concurrency N] [--filter TEXT] [--out evals/reports] [--label TEXT]\n\
                     \x20                     [--mode craft|plan|ask]\n\
                     模型地址与 key 读取 .env 中的 LLM 配置；--model 覆盖模型名。"
                );
                std::process::exit(0);
            }
            other => return Err(anyhow!("未知参数: {other}")),
        }
    }
    opts.repeat = opts.repeat.max(1);
    opts.concurrency = opts.concurrency.max(1);
    Ok(opts)
}

fn load_cases(path: &Path, filter: Option<&str>) -> Result<Vec<EvalCase>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取用例失败: {}", path.display()))?;
    let mut cases = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let case: EvalCase = serde_json::from_str(line)
            .with_context(|| format!("{}:{} 解析失败", path.display(), line_no + 1))?;
        if filter.map_or(true, |f| case.id.contains(f) || case.category.contains(f)) {
            cases.push(case);
        }
    }
    Ok(cases)
}

fn to_attachments(case: &EvalCase) -> Vec<ChatAttachment> {
    case.attachments
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let is_image = a.kind == "image";
            let text = a.text.clone().unwrap_or_default();
            ChatAttachment {
                id: format!("eval-att-{i}"),
                name: a.name.clone(),
                kind: a.kind.clone(),
                mime_type: if is_image { "image/png" } else { "text/markdown" }.to_string(),
                size: if is_image { 70 } else { text.len() },
                text_content: (!is_image).then_some(text),
                data_url: is_image.then(|| PLACEHOLDER_PNG.to_string()),
            }
        })
        .collect()
}

/// 模型实际选择的工具。若调用了 propose_plan，计划里列出的工具也算作选择（兼容 M2 的计划确认）
fn effective_tools(tool_calls: &[crate::llm::types::ToolCall]) -> BTreeSet<String> {
    let mut tools = BTreeSet::new();
    for tc in tool_calls {
        tools.insert(tc.function.name.clone());
        if tc.function.name == "propose_plan" {
            let args: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or_default();
            for step in args["steps"].as_array().into_iter().flatten() {
                if let Some(tool) = step["tool"].as_str() {
                    tools.insert(tool.to_string());
                }
            }
        }
    }
    tools
}

/// 多步任务里合理的“第一步”：不产出最终结果，只是为后续步骤做准备
const PREP_TOOLS: &[&str] = &[
    "use_skill",
    "read_skill_file",
    "workspace_list",
    "workspace_search",
    "workspace_read",
    "docs_read",
    "sheet_read",
    "slides_read",
    "data_profile",
    "web_search",
    "propose_plan",
];

/// 严格未通过时，是否属于“只做了准备步骤”
fn is_prep_step(case: &EvalCase, tools: &BTreeSet<String>) -> bool {
    !case.expect_no_tool
        && !tools.is_empty()
        && tools.iter().all(|t| PREP_TOOLS.contains(&t.as_str()))
        && !case.forbid.iter().any(|t| tools.contains(t))
}

fn judge(case: &EvalCase, tools: &BTreeSet<String>) -> (bool, String) {
    let mut problems = Vec::new();
    if case.expect_no_tool && !tools.is_empty() {
        problems.push("应直接回复，不应调用工具".to_string());
    }
    let missing: Vec<_> = case.expect.iter().filter(|t| !tools.contains(*t)).collect();
    if !missing.is_empty() {
        problems.push(format!("缺少 {missing:?}"));
    }
    if !case.expect_any.is_empty() && !case.expect_any.iter().any(|t| tools.contains(t)) {
        problems.push(format!("未调用 {:?} 中任何一个", case.expect_any));
    }
    let forbidden: Vec<_> = case.forbid.iter().filter(|t| tools.contains(*t)).collect();
    if !forbidden.is_empty() {
        problems.push(format!("误调用 {forbidden:?}"));
    }
    (problems.is_empty(), problems.join("；"))
}

async fn run_attempt(
    case: &EvalCase,
    rep: usize,
    mode: AgentMode,
    client: &LlmClient,
    extra_tools: &[crate::agent::tool::DynTool],
) -> (Vec<String>, Attempt) {
    let session_id = format!("eval-{}-{rep}", case.id);
    let history: Vec<ChatMessage> = case
        .history
        .iter()
        .map(|m| ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_calls: None,
            tool_call_id: None,
        })
        .collect();

    let attachments = to_attachments(case);
    let req = ChatRequest {
        message: case.message.clone(),
        session_id: Some(session_id),
        tool_kind: case.tool_kind.clone(),
        attachments: (!attachments.is_empty()).then(|| attachments.clone()),
        mode: Some(mode),
        ..Default::default()
    };
    let Routing {
        system_prompt,
        allowed_tools,
        mode,
        ..
    } = resolve_routing(&req);
    let config = AgentConfig {
        max_turns: 1,
        // 与线上一致：附上技能清单（评测只用内置技能）
        system_prompt: format!("{system_prompt}\n\n{}", crate::skills::builtin_listing()),
        allowed_tools,
        mode,
        extra_tools: extra_tools.to_vec(),
    };
    let InitialRequest {
        conversation,
        function_defs,
    } = build_initial_request(history, &build_user_message(&req), &config, client)
        .await;
    let exposed: Vec<String> = function_defs.iter().map(|d| d.function.name.clone()).collect();

    let started = Instant::now();
    // 网络错误不是路由结果：重试一次，避免把它记成失败
    let call = || {
        client.chat_with_attachments(
            &conversation,
            (!function_defs.is_empty()).then_some(function_defs.as_slice()),
            (!attachments.is_empty()).then_some(attachments.as_slice()),
        )
    };
    let result = match call().await {
        Err(e) if e.to_string().contains("error sending request") => {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            call().await
        }
        other => other,
    };
    let latency_ms = started.elapsed().as_millis();

    let attempt = match result {
        Ok(resp) => {
            let calls = resp
                .choices
                .first()
                .and_then(|c| c.message.tool_calls.clone())
                .unwrap_or_default();
            let tools = effective_tools(&calls);
            let (passed, reason) = judge(case, &tools);
            let prep = !passed && is_prep_step(case, &tools);
            Attempt {
                passed,
                prep,
                tools: tools.into_iter().collect(),
                tool_call_count: calls.len(),
                latency_ms,
                reason,
            }
        }
        Err(e) => Attempt {
            passed: false,
            prep: false,
            tools: vec![],
            tool_call_count: 0,
            latency_ms,
            reason: format!("LLM 调用失败: {e}"),
        },
    };
    (exposed, attempt)
}

fn percentile(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn render_markdown(
    reports: &[CaseReport],
    model: &str,
    mode: AgentMode,
    label: &str,
    repeat: usize,
    started_at: &str,
) -> String {
    let attempts: Vec<&Attempt> = reports.iter().flat_map(|r| &r.attempts).collect();
    let total = attempts.len().max(1) as f64;
    let passed = attempts.iter().filter(|a| a.passed).count() as f64;
    let errors = attempts.iter().filter(|a| a.reason.starts_with("LLM 调用失败")).count();
    let with_tools: Vec<_> = attempts.iter().filter(|a| a.tool_call_count > 0).collect();
    let avg_calls = if with_tools.is_empty() {
        0.0
    } else {
        with_tools.iter().map(|a| a.tool_call_count).sum::<usize>() as f64 / with_tools.len() as f64
    };
    let multi_rate = with_tools.iter().filter(|a| a.tool_call_count > 1).count() as f64
        / with_tools.len().max(1) as f64;
    let mut latencies: Vec<u128> = attempts.iter().map(|a| a.latency_ms).collect();
    latencies.sort_unstable();

    let mut out = String::new();
    out.push_str(&format!("# 路由评测报告 · {label}\n\n"));
    out.push_str(&format!(
        "- 时间：{started_at}\n- 模型：`{model}`\n- 模式：{}\n- 用例：{} 条 × {repeat} 次\n\n",
        mode.as_str(),
        reports.len()
    ));
    out.push_str("| 指标 | 值 |\n|---|---|\n");
    let prep = attempts.iter().filter(|a| a.prep).count() as f64;
    out.push_str(&format!("| 通过率 | **{:.1}%** ({}/{}) |\n", passed / total * 100.0, passed, attempts.len()));
    out.push_str(&format!(
        "| 含准备步骤的通过率 | {:.1}% ({}/{})：第一轮只做了加载技能、找 / 读文件、搜索、提交计划，且未调用禁止的工具 |\n",
        (passed + prep) / total * 100.0,
        passed + prep,
        attempts.len()
    ));
    out.push_str(&format!("| 调用失败 | {errors} |\n"));
    out.push_str(&format!("| 调用工具时平均 tool_call 数 | {avg_calls:.2} |\n"));
    out.push_str(&format!("| 单轮多工具比例 | {:.1}% |\n", multi_rate * 100.0));
    out.push_str(&format!(
        "| 延迟 p50 / p95 | {} ms / {} ms |\n\n",
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.95)
    ));

    let mut by_cat: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for r in reports {
        let entry = by_cat.entry(&r.category).or_default();
        entry.0 += r.attempts.iter().filter(|a| a.passed).count();
        entry.1 += r.attempts.len();
    }
    out.push_str("## 分类\n\n| 分类 | 通过 |\n|---|---|\n");
    for (cat, (ok, n)) in &by_cat {
        out.push_str(&format!("| {cat} | {ok}/{n} |\n"));
    }

    out.push_str("\n## 用例明细\n\n| 用例 | 通过率 | 选择的工具 | 问题 |\n|---|---|---|---|\n");
    for r in reports {
        let tools = r
            .attempts
            .iter()
            .map(|a| if a.tools.is_empty() { "（文本）".to_string() } else { a.tools.join("+") })
            .collect::<Vec<_>>()
            .join(" / ");
        let reasons = r
            .attempts
            .iter()
            .filter(|a| !a.passed)
            .map(|a| format!("{}{}", if a.prep { "（准备步骤）" } else { "" }, a.reason.replace('|', "\\|")))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join("；");
        out.push_str(&format!(
            "| `{}` | {:.0}% | {} | {} |\n",
            r.id,
            r.pass_rate() * 100.0,
            tools,
            reasons
        ));
    }
    out
}

/// 评测用的插件：标识固定为 demo，工具名为 mcp__demo__*
async fn load_eval_mcp(url: &str, header: Option<&str>) -> Result<Vec<crate::agent::tool::DynTool>> {
    let mut headers = std::collections::BTreeMap::new();
    if let Some((name, value)) = header.and_then(|h| h.split_once(':')) {
        headers.insert(name.trim().to_string(), value.trim().to_string());
    }
    let sse = reqwest::Url::parse(url).is_ok_and(|u| u.path().trim_end_matches('/').ends_with("/sse"));
    let now = chrono::Utc::now().to_rfc3339();
    let server = crate::mcp::McpServer {
        id: "eval-mcp".into(),
        owner_id: "eval".into(),
        slug: "demo".into(),
        name: "演示插件".into(),
        transport: if sse { "sse".into() } else { "http".into() },
        url: Some(url.to_string()),
        command: None,
        args: vec![],
        config: crate::mcp::ServerConfig::default(),
        secrets: crate::mcp::ServerSecrets { headers, ..Default::default() },
        secrets_broken: false,
        enabled: true,
        created_at: now.clone(),
        updated_at: now,
    };
    let tools = crate::mcp::pool::tools(&server, true).await.map_err(|e| anyhow!("连接评测插件失败：{e}"))?;
    println!("▶ 接入插件 {url}：{} 个工具", tools.len());
    Ok(tools
        .into_iter()
        .map(|info| std::sync::Arc::new(crate::mcp::pool::McpTool::new(&server, info)) as crate::agent::tool::DynTool)
        .collect())
}

pub async fn run(args: &[String]) -> Result<()> {
    let opts = parse_args(args)?;
    let mut cases = load_cases(&opts.cases, opts.filter.as_deref())?;

    crate::agent::tools::register_all_tools().await;
    let extra_tools = match opts.mcp.as_deref() {
        Some(url) => load_eval_mcp(url, opts.mcp_header.as_deref()).await?,
        None => {
            let before = cases.len();
            cases.retain(|c| !c.requires_mcp);
            if cases.len() < before {
                println!("（跳过 {} 条需要插件的用例；用 --mcp <地址> 接入 dev/mock_mcp.py 后可评测）", before - cases.len());
            }
            vec![]
        }
    };
    if cases.is_empty() {
        return Err(anyhow!("没有匹配的用例"));
    }

    let mut client = LlmClient::new();
    if let Some(model) = opts.model.as_deref() {
        client = client.with_model(model);
    }
    let client = Arc::new(client);
    let model = client.model().to_string();
    println!(
        "▶ 评测 {} 条用例 × {} 次，{} 模式，模型 {} @ {}",
        cases.len(),
        opts.repeat,
        opts.mode.as_str(),
        model,
        client.base_url()
    );

    let started_at = chrono::Local::now();
    let mode = opts.mode;
    let jobs = cases
        .iter()
        .enumerate()
        .flat_map(|(ci, _)| (0..opts.repeat).map(move |rep| (ci, rep)));
    let results: Vec<(usize, Vec<String>, Attempt)> = futures::stream::iter(jobs)
        .map(|(ci, rep)| {
            let client = client.clone();
            let case = &cases[ci];
            let extra_tools = &extra_tools;
            async move {
                let (exposed, attempt) = run_attempt(case, rep, mode, &client, extra_tools).await;
                println!(
                    "  {} {:<28} {}",
                    if attempt.passed { "✓" } else { "✗" },
                    case.id,
                    if attempt.passed { attempt.tools.join("+") } else { attempt.reason.clone() }
                );
                (ci, exposed, attempt)
            }
        })
        .buffer_unordered(opts.concurrency)
        .collect()
        .await;

    let mut reports: Vec<CaseReport> = cases
        .iter()
        .map(|c| CaseReport {
            id: c.id.clone(),
            category: c.category.clone(),
            message: c.message.clone(),
            exposed_tools: vec![],
            attempts: vec![],
        })
        .collect();
    for (ci, exposed, attempt) in results {
        reports[ci].exposed_tools = exposed;
        reports[ci].attempts.push(attempt);
    }

    let label = opts.label.clone().unwrap_or_else(|| "未命名".to_string());
    let markdown = render_markdown(
        &reports,
        &model,
        mode,
        &label,
        opts.repeat,
        &started_at.format("%Y-%m-%d %H:%M:%S").to_string(),
    );

    std::fs::create_dir_all(&opts.out)?;
    let slug = |s: &str| {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
            .collect::<String>()
    };
    let stem = format!(
        "{}-{}-{}{}",
        started_at.format("%Y%m%d-%H%M%S"),
        slug(&model),
        mode.as_str(),
        opts.label.as_deref().map(|l| format!("-{}", slug(l))).unwrap_or_default()
    );
    let md_path = opts.out.join(format!("{stem}.md"));
    let json_path = opts.out.join(format!("{stem}.json"));
    std::fs::write(&md_path, &markdown)?;
    std::fs::write(&json_path, serde_json::to_string_pretty(&reports)?)?;

    let summary_line = markdown
        .lines()
        .find(|l| l.starts_with("| 通过率"))
        .unwrap_or_default();
    println!("\n{summary_line}\n报告：{}\n明细：{}", md_path.display(), json_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{ToolCall, ToolFunction};

    fn case(json: &str) -> EvalCase {
        serde_json::from_str(json).unwrap()
    }

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolFunction {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn judge_expect_forbid_and_no_tool() {
        let c = case(r#"{"id":"a","category":"x","message":"m","expect":["doc_generate"],"forbid":["ppt_plan"]}"#);
        assert!(judge(&c, &set(&["doc_generate"])).0);
        assert!(!judge(&c, &set(&["md_generate"])).0);
        assert!(!judge(&c, &set(&["doc_generate", "ppt_plan"])).0);

        let c = case(r#"{"id":"b","category":"x","message":"m","expect_any":["ppt_plan","ppt_generate"]}"#);
        assert!(judge(&c, &set(&["ppt_generate"])).0);
        assert!(!judge(&c, &set(&[])).0);

        let c = case(r#"{"id":"c","category":"x","message":"m","expect_no_tool":true}"#);
        assert!(judge(&c, &set(&[])).0);
        assert!(!judge(&c, &set(&["web_search"])).0);
    }

    #[test]
    fn prep_steps_are_counted_separately() {
        let c = case(r#"{"id":"a","category":"x","message":"m","expect_any":["data_query"],"forbid":["chart_generate"]}"#);
        assert!(is_prep_step(&c, &set(&["workspace_search"])));
        assert!(is_prep_step(&c, &set(&["use_skill", "data_profile"])));
        assert!(!is_prep_step(&c, &set(&["workspace_search", "chart_generate"])), "调用了禁止的工具");
        assert!(!is_prep_step(&c, &set(&["md_generate"])), "不是准备工具");
        assert!(!is_prep_step(&c, &set(&[])));
        let chat = case(r#"{"id":"b","category":"x","message":"m","expect_no_tool":true}"#);
        assert!(!is_prep_step(&chat, &set(&["web_search"])), "应直接回复的用例不算");
    }

    #[test]
    fn propose_plan_steps_count_as_selected_tools() {
        let calls = vec![call(
            "propose_plan",
            r#"{"goal":"g","steps":[{"tool":"ppt_plan"},{"tool":"drawio_generate"}]}"#,
        )];
        assert_eq!(
            effective_tools(&calls),
            set(&["propose_plan", "ppt_plan", "drawio_generate"])
        );
    }

    #[test]
    fn bundled_cases_parse() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/routing.jsonl");
        let cases = load_cases(&path, None).unwrap();
        assert!(cases.len() >= 40);
        let mut ids = BTreeSet::new();
        for c in &cases {
            assert!(ids.insert(c.id.clone()), "重复用例 id: {}", c.id);
        }
    }
}
