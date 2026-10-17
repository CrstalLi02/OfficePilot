//! 处理 IM 消息：去重 → 绑定校验 → 命令 / 计划确认 / 发起运行，并把运行进度与结果推回 IM。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde_json::{json, Value};

use super::feishu::{self, Tone};
use super::{manager, Channel, Conversation, Incoming};
use crate::agent::plan::{AgentMode, Decision, Plan, PlanDecision};
use crate::db::{session_repo, DbPool};
use crate::models::{Artifact, ChatRequest};
use crate::tasks::{Origin, TaskEvent};

/// 每个 IM 账号每分钟最多触发的消息数
const RATE_LIMIT_PER_MINUTE: usize = 10;
/// 单条消息最长字数
const MAX_TEXT_CHARS: usize = 4000;
/// 进度卡片最短更新间隔（飞书单条消息更新有频率限制）
const CARD_UPDATE_EVERY: Duration = Duration::from_millis(1500);

const HELP: &str = "直接发送需求就会开始干活，例如「把本周会议纪要整理成周报」。\n\n\
**命令**\n\
- `/new` 开启新对话\n\
- `/mode 执行|规划|问答` 切换模式（规划模式会先给出计划，回复「确认」再执行）\n\
- `/stop` 停止正在运行的任务\n\
- `/status` 查看当前对话与模式\n\
- `/help` 查看帮助\n\n\
群聊里需要 @ 机器人。";

// ───────────────────────── 命令 ─────────────────────────

#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    New,
    Mode(Option<AgentMode>),
    Stop,
    Status,
    Bind(String),
    Text(String),
}

fn parse_mode(text: &str) -> Option<AgentMode> {
    match text.trim().to_lowercase().as_str() {
        "craft" | "执行" | "执行模式" => Some(AgentMode::Craft),
        "plan" | "规划" | "计划" | "规划模式" => Some(AgentMode::Plan),
        "ask" | "问答" | "问答模式" => Some(AgentMode::Ask),
        _ => None,
    }
}

pub fn mode_label(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Craft => "执行",
        AgentMode::Plan => "规划",
        AgentMode::Ask => "问答",
    }
}

/// 只有以 / 开头或整条消息恰好是命令词时才当作命令，避免「帮助我写周报」被误判
pub fn parse_command(text: &str) -> Command {
    let text = text.trim();
    let (head, rest) = match text.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (text, ""),
    };
    let slash = head.strip_prefix('/').or_else(|| head.strip_prefix('／'));
    let word = slash.unwrap_or(head).to_lowercase();
    let exact = rest.is_empty();
    match word.as_str() {
        "help" | "帮助" if slash.is_some() || exact => Command::Help,
        "new" | "新对话" if slash.is_some() || exact => Command::New,
        "stop" | "停止" if slash.is_some() || exact => Command::Stop,
        "status" | "状态" if slash.is_some() || exact => Command::Status,
        "mode" | "模式" if slash.is_some() || !exact => Command::Mode(parse_mode(rest)),
        "bind" | "绑定" if (slash.is_some() || !exact) && rest.len() == 6 && rest.chars().all(|c| c.is_ascii_digit()) => {
            Command::Bind(rest.to_string())
        }
        _ => Command::Text(text.to_string()),
    }
}

/// 待确认计划时用户的回复
pub fn plan_reply(text: &str) -> (Decision, Option<String>) {
    let t = text.trim().trim_end_matches(['。', '!', '！', '.']).to_lowercase();
    match t.as_str() {
        "确认" | "确定" | "执行" | "好" | "好的" | "可以" | "同意" | "ok" | "yes" | "y" | "approve" | "开始" | "开始执行" => (Decision::Approve, None),
        "取消" | "放弃" | "不用了" | "算了" | "no" | "n" | "reject" | "cancel" => (Decision::Reject, None),
        _ => (Decision::Revise, Some(text.trim().to_string())),
    }
}

// ───────────────────────── 频率限制 ─────────────────────────

static RECENT: Lazy<Mutex<HashMap<String, VecDeque<Instant>>>> = Lazy::new(Default::default);

fn allow(key: &str) -> bool {
    let mut all = RECENT.lock().unwrap();
    let now = Instant::now();
    let window = all.entry(key.to_string()).or_default();
    while window.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60)) {
        window.pop_front();
    }
    if window.len() >= RATE_LIMIT_PER_MINUTE {
        return false;
    }
    window.push_back(now);
    true
}

// ───────────────────────── 入口 ─────────────────────────

/// 长连接与事件回调收到的事件都从这里进来
pub async fn handle_event(channel_id: String, event: Value) {
    let pool = crate::state::db_pool();
    let channel = match super::find_channel(&pool, &channel_id).await {
        Ok(Some(c)) if c.enabled => c,
        _ => return,
    };
    manager::touch(&channel.id);
    let bot = manager::bot_open_id(&channel.id);
    let Some(msg) = feishu::parse_message_event(&event, bot.as_deref()) else { return };
    if !super::first_seen(&pool, &channel.id, &msg.event_key).await {
        return;
    }
    if msg.chat_type != "p2p" && !msg.mentioned_bot {
        return;
    }
    let client = feishu::Client::new(&channel);
    if let Err(e) = process(&pool, &channel, &client, &msg).await {
        tracing::warn!("[IM] 处理消息失败：{e:#}");
        let _ = client.reply(&msg.message_id, "text", &json!({ "text": format!("处理失败：{e}") })).await;
    }
}

async fn reply_text(client: &feishu::Client, msg: &Incoming, text: &str) -> anyhow::Result<()> {
    client.reply(&msg.message_id, "text", &json!({ "text": text })).await?;
    Ok(())
}

async fn reply_card(client: &feishu::Client, msg: &Incoming, title: &str, tone: Tone, body: &str) -> anyhow::Result<String> {
    client.reply(&msg.message_id, "interactive", &feishu::card(title, tone, body, None, None)).await
}

fn web_link(session_id: &str) -> Option<String> {
    let base = std::env::var("PUBLIC_BASE_URL").ok()?;
    let base = base.trim().trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}/?s={session_id}"))
}

async fn process(pool: &DbPool, channel: &Channel, client: &feishu::Client, msg: &Incoming) -> anyhow::Result<()> {
    if !allow(&format!("{}|{}", channel.id, msg.sender_id)) {
        return reply_text(client, msg, "消息太频繁了，请稍后再试").await;
    }
    let command = parse_command(&msg.text);

    // 绑定不要求已绑定
    if let Command::Bind(code) = &command {
        if !super::consume_bind_code(&channel.id, code) {
            return reply_text(client, msg, "绑定码无效或已过期，请在网页「设置 → IM 接入」重新生成").await;
        }
        super::add_binding(pool, &channel.id, &msg.sender_id, msg.sender_name.as_deref()).await?;
        reply_card(client, msg, "绑定成功", Tone::Success, &format!("现在可以直接在这里让我干活了。\n\n{HELP}")).await?;
        return Ok(());
    }
    if !super::is_bound(pool, &channel.id, &msg.sender_id).await? {
        return reply_text(
            client,
            msg,
            "你还没有绑定账号：请在网页「设置 → IM 接入」里为这个机器人生成绑定码，然后在这里发送「绑定 123456」。",
        )
        .await;
    }
    if let Some(kind) = &msg.unsupported {
        if msg.text.is_empty() {
            return reply_text(client, msg, &format!("暂时只支持文字消息（收到的是 {kind}），图片和文件即将支持")).await;
        }
    }

    let mut conv = super::conversation(pool, channel, &msg.chat_id).await?;
    let owner = channel.owner_id.as_str();
    let running = conv.session_id.as_deref().and_then(|sid| crate::tasks::running_for_session(sid, owner));

    match command {
        Command::Help => return reply_card(client, msg, "使用说明", Tone::Neutral, HELP).await.map(|_| ()),
        Command::New => {
            conv.session_id = None;
            conv.pending_run_id = None;
            super::save_conversation(pool, &conv).await?;
            return reply_text(client, msg, &format!("已开启新对话（{}模式）", mode_label(conv.mode))).await;
        }
        Command::Mode(None) => return reply_text(client, msg, "可选模式：执行 / 规划 / 问答，例如 /mode 规划").await,
        Command::Mode(Some(mode)) => {
            conv.mode = mode;
            conv.pending_run_id = None;
            super::save_conversation(pool, &conv).await?;
            return reply_text(client, msg, &format!("已切换到{}模式", mode_label(mode))).await;
        }
        Command::Stop => {
            let text = match running {
                Some(task_id) if crate::tasks::cancel(&task_id, owner) => "已停止当前任务",
                _ => "当前没有运行中的任务",
            };
            return reply_text(client, msg, text).await;
        }
        Command::Status => {
            let session = match &conv.session_id {
                Some(sid) => session_repo::find_by_id(pool, sid).await?.map(|s| s.title),
                None => None,
            };
            let mut body = format!(
                "**模式**：{}\n**对话**：{}\n**状态**：{}",
                mode_label(conv.mode),
                session.unwrap_or_else(|| "（新对话）".into()),
                if running.is_some() { "运行中" } else if conv.pending_run_id.is_some() { "计划待确认" } else { "空闲" },
            );
            if let Some(link) = conv.session_id.as_deref().and_then(web_link) {
                body.push_str(&format!("\n[在网页中打开]({link})"));
            }
            return reply_card(client, msg, "当前状态", Tone::Neutral, &body).await.map(|_| ());
        }
        Command::Bind(_) => unreachable!(),
        Command::Text(text) => {
            if text.is_empty() {
                return reply_card(client, msg, "使用说明", Tone::Neutral, HELP).await.map(|_| ());
            }
            if text.chars().count() > MAX_TEXT_CHARS {
                return reply_text(client, msg, &format!("消息太长了（最多 {MAX_TEXT_CHARS} 字），请精简后再发")).await;
            }
            if running.is_some() {
                return reply_text(client, msg, "上一个任务还在运行，发送 /stop 可以停止，或者等它完成后再发").await;
            }
            start_run(pool, channel, client, msg, &mut conv, text).await
        }
    }
}

async fn start_run(
    pool: &DbPool,
    channel: &Channel,
    client: &feishu::Client,
    msg: &Incoming,
    conv: &mut Conversation,
    text: String,
) -> anyhow::Result<()> {
    let owner = channel.owner_id.as_str();
    // 当前对话不存在（例如在网页上删掉了）时换新会话
    let session_id = match &conv.session_id {
        Some(sid) if session_repo::find_by_id(pool, sid).await?.is_some_and(|s| s.owner_id == owner) => sid.clone(),
        _ => {
            conv.pending_run_id = None;
            let title = format!("[飞书] {}", feishu::truncate(&text, 24));
            session_repo::create(pool, owner, None, None, &title).await?.id
        }
    };

    // 有待确认的计划：这条消息就是对计划的回复
    let resume = conv.pending_run_id.take().map(|run_id| {
        let (decision, feedback) = plan_reply(&text);
        PlanDecision { run_id, decision, steps: None, feedback }
    });
    let req = ChatRequest {
        message: text.clone(),
        session_id: Some(session_id.clone()),
        mode: Some(conv.mode),
        timezone: Some(channel.config.timezone.clone()),
        resume,
        ..Default::default()
    };
    conv.session_id = Some(session_id.clone());
    super::save_conversation(pool, conv).await?;

    let started = match crate::routes::chat::start_chat_task(owner, req, Origin::Im).await {
        Ok(started) => started,
        Err(e) => return reply_text(client, msg, &format!("没能开始运行：{e}")).await,
    };
    let card_id = reply_card(client, msg, &format!("处理中 · {}模式", mode_label(conv.mode)), Tone::Running, "收到，正在处理…").await?;
    tokio::spawn(pump(channel.clone(), client.clone(), msg.clone(), conv.id.clone(), started.task_id, started.session_id, card_id));
    Ok(())
}

// ───────────────────────── 进度回推 ─────────────────────────

/// 工具名称在 IM 里的显示（与网页时间线一致）
fn tool_label(tool: &str) -> &str {
    match tool {
        "ppt_plan" => "规划 PPT 大纲",
        "ppt_generate" => "生成 PPT",
        "doc_generate" => "生成 Word 文档",
        "md_generate" => "生成 Markdown",
        "sheet_generate" => "生成表格",
        "chart_generate" => "生成图表",
        "drawio_generate" => "绘制流程图",
        "image_prompt" => "生成图片",
        "video_generate" => "生成视频",
        "video_batch_generate" => "批量生成视频",
        "video_storyboard" => "规划视频分镜",
        "web_search" => "联网搜索",
        "artifact_edit" => "局部修改产物",
        "use_skill" => "使用技能",
        "read_skill_file" => "读取技能资料",
        "data_profile" => "查看数据结构",
        "data_query" => "数据查询",
        "workspace_list" => "查看文件",
        "workspace_search" => "搜索文件",
        "workspace_read" => "读取文件",
        "workspace_write" => "保存到文件",
        "workspace_organize" => "整理文件",
        "schedule_create" => "创建定时任务",
        "schedule_list" => "查看定时任务",
        "propose_plan" => "制定计划",
        other => other,
    }
}

/// 运行过程中累积的状态，用来渲染进度卡片
#[derive(Default)]
pub struct Progress {
    steps: Vec<(String, String, Option<bool>)>,
    text: String,
    plan: Option<(String, String)>,
    finished: Option<Value>,
    error: Option<String>,
}

impl Progress {
    /// 吃进一个事件，返回卡片是否需要更新
    pub fn apply(&mut self, ev: &TaskEvent) -> bool {
        let d = &ev.data;
        let call_id = || d.get("call_id").and_then(Value::as_str).unwrap_or_default().to_string();
        match ev.event.as_str() {
            "tool_start" | "tool_call" => {
                let id = call_id();
                if !self.steps.iter().any(|(c, _, _)| *c == id) {
                    let tool = d.get("tool").and_then(Value::as_str).unwrap_or_default();
                    self.steps.push((id, tool_label(tool).to_string(), None));
                    return true;
                }
                false
            }
            "tool_result" => {
                let id = call_id();
                let ok = d.get("success").and_then(Value::as_bool).unwrap_or(false);
                if let Some(step) = self.steps.iter_mut().find(|(c, _, _)| *c == id) {
                    step.2 = Some(ok);
                }
                true
            }
            "message" => {
                self.text.push_str(d.get("text").and_then(Value::as_str).unwrap_or_default());
                true
            }
            "plan_proposed" => {
                let run_id = d.get("run_id").and_then(Value::as_str).unwrap_or_default().to_string();
                let plan = serde_json::from_value::<Plan>(d.get("plan").cloned().unwrap_or_default())
                    .map(|p| p.to_markdown())
                    .unwrap_or_default();
                self.plan = Some((run_id, plan));
                true
            }
            "done" => {
                self.finished = Some(d.clone());
                true
            }
            "error" => {
                self.error = Some(d.get("message").and_then(Value::as_str).unwrap_or("运行失败").to_string());
                true
            }
            _ => false,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished.is_some() || self.error.is_some()
    }

    fn steps_markdown(&self) -> String {
        self.steps
            .iter()
            .map(|(_, label, ok)| {
                let mark = match ok {
                    None => "⏳",
                    Some(true) => "✅",
                    Some(false) => "⚠️",
                };
                format!("{mark} {label}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 运行中的卡片正文：已执行的步骤 + 最新输出的几行
    pub fn running_body(&self) -> String {
        let mut body = self.steps_markdown();
        let tail: String = {
            let chars: Vec<char> = self.text.trim().chars().collect();
            let start = chars.len().saturating_sub(300);
            chars[start..].iter().collect()
        };
        if !tail.is_empty() {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(&tail);
        }
        if body.is_empty() {
            "收到，正在处理…".into()
        } else {
            body
        }
    }
}

async fn pump(
    channel: Channel,
    client: feishu::Client,
    msg: Incoming,
    conversation_id: String,
    task_id: String,
    session_id: String,
    card_id: String,
) {
    let owner = channel.owner_id.clone();
    let Some((history, mut rx, _)) = crate::tasks::subscribe(&task_id, &owner, 0) else { return };
    let started = Instant::now();
    let mut progress = Progress::default();
    for ev in &history {
        progress.apply(ev);
    }
    let mut dirty = true;
    let mut ticker = tokio::time::interval(CARD_UPDATE_EVERY);
    while !progress.is_finished() {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => dirty |= progress.apply(&ev),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            _ = ticker.tick() => {
                if dirty && !card_id.is_empty() {
                    let card = feishu::card("处理中", Tone::Running, &progress.running_body(), Some("发送 /stop 可以停止"), None);
                    if let Err(e) = client.update_card(&card_id, &card).await {
                        tracing::debug!("[IM] 更新进度卡片失败：{e:#}");
                    }
                    dirty = false;
                }
            }
        }
    }

    let pool = crate::state::db_pool();
    let elapsed = started.elapsed().as_secs();
    let link = web_link(&session_id);
    let link_button = link.as_deref().map(|l| ("在网页中打开", l));
    let note = format!("用时 {}", if elapsed < 60 { format!("{elapsed} 秒") } else { format!("{} 分 {} 秒", elapsed / 60, elapsed % 60) });
    let steps = progress.steps_markdown();
    let with_steps = |body: &str| if steps.is_empty() { body.to_string() } else { format!("{steps}\n\n{body}") };

    let mut files: Vec<Artifact> = Vec::new();
    let card = match (&progress.finished, &progress.error) {
        (Some(done), _) if done.get("awaiting_approval").and_then(Value::as_bool) == Some(true) => {
            let (run_id, plan) = progress.plan.clone().unwrap_or_default();
            // 记下待确认的计划：下一条消息就是对它的回复
            if let Ok(mut conv) = find_conversation(&pool, &conversation_id).await {
                conv.pending_run_id = (!run_id.is_empty()).then_some(run_id);
                let _ = super::save_conversation(&pool, &conv).await;
            }
            let body = format!("{plan}\n\n回复 **确认** 开始执行，回复 **取消** 放弃，或者直接回复修改意见。");
            feishu::card("计划待确认", Tone::Warning, &body, Some(&note), link_button)
        }
        (Some(done), _) if done.get("cancelled").and_then(Value::as_bool) == Some(true) => {
            feishu::card("已停止", Tone::Neutral, &with_steps("任务已停止"), Some(&note), link_button)
        }
        (Some(done), _) => {
            files = done
                .get("new_artifacts")
                .and_then(|a| serde_json::from_value::<Vec<Artifact>>(a.clone()).ok())
                .unwrap_or_default();
            let summary = done.get("summary").and_then(Value::as_str).unwrap_or_default();
            let mut body = with_steps(&feishu::truncate(summary.trim(), 3500));
            if !files.is_empty() {
                body.push_str(&format!("\n\n**生成了 {} 个文件**，马上发给你", files.len()));
            }
            feishu::card("已完成", Tone::Success, &body, Some(&note), link_button)
        }
        (None, Some(error)) => feishu::card("运行失败", Tone::Failed, &with_steps(error), Some(&note), link_button),
        (None, None) => feishu::card("连接中断", Tone::Neutral, "没能拿到运行结果，请在网页上查看", Some(&note), link_button),
    };
    let updated = if card_id.is_empty() { Err(anyhow::anyhow!("没有进度卡片")) } else { client.update_card(&card_id, &card).await };
    if let Err(e) = updated {
        tracing::warn!("[IM] 更新结果卡片失败，改为发新消息：{e:#}");
        let _ = client.reply(&msg.message_id, "interactive", &card).await;
    }

    for artifact in files {
        let Some(file) = crate::file_store::render_artifact_file(&owner, &artifact).await else { continue };
        // 没有专门格式的产物（搜索结果等）渲染成 JSON，发到 IM 没有意义
        if file.mime == "application/json" {
            continue;
        }
        let name = file.name.clone();
        let sent = match client.upload(&file.name, &file.mime, file.bytes).await {
            Ok((msg_type, content)) => client.reply(&msg.message_id, &msg_type, &content).await.map(|_| ()),
            Err(e) => Err(e),
        };
        if let Err(e) = sent {
            tracing::warn!("[IM] 发送文件 {name} 失败：{e:#}");
            let _ = client.reply(&msg.message_id, "text", &json!({ "text": format!("文件「{name}」发送失败：{e}") })).await;
        }
    }
}

async fn find_conversation(pool: &DbPool, id: &str) -> anyhow::Result<Conversation> {
    use sqlx::Row;
    let row = sqlx::query("SELECT id, session_id, mode, pending_run_id FROM im_conversations WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(Conversation {
        id: row.try_get(0)?,
        session_id: crate::db::text_column(&row, 1)?,
        mode: serde_json::from_value(json!(crate::db::text_column(&row, 2)?.unwrap_or_default())).unwrap_or(AgentMode::Craft),
        pending_run_id: crate::db::text_column(&row, 3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_need_slash_or_exact_word() {
        assert_eq!(parse_command("/help"), Command::Help);
        assert_eq!(parse_command("帮助"), Command::Help);
        assert_eq!(parse_command("帮助我写一份周报"), Command::Text("帮助我写一份周报".into()));
        assert_eq!(parse_command("停止 吧"), Command::Text("停止 吧".into()));
        assert_eq!(parse_command(" /new "), Command::New);
        assert_eq!(parse_command("/stop"), Command::Stop);
        assert_eq!(parse_command("状态"), Command::Status);
        assert_eq!(parse_command("/mode 规划"), Command::Mode(Some(AgentMode::Plan)));
        assert_eq!(parse_command("/mode ask"), Command::Mode(Some(AgentMode::Ask)));
        assert_eq!(parse_command("/mode 随便"), Command::Mode(None));
        assert_eq!(parse_command("模式 问答"), Command::Mode(Some(AgentMode::Ask)));
        assert_eq!(parse_command("绑定 123456"), Command::Bind("123456".into()));
        assert_eq!(parse_command("/bind 654321"), Command::Bind("654321".into()));
        assert_eq!(parse_command("绑定 12345"), Command::Text("绑定 12345".into()));
        assert_eq!(parse_command("绑定"), Command::Text("绑定".into()));
    }

    #[test]
    fn plan_replies() {
        assert_eq!(plan_reply("确认").0, Decision::Approve);
        assert_eq!(plan_reply("OK!").0, Decision::Approve);
        assert_eq!(plan_reply("取消。").0, Decision::Reject);
        let (d, feedback) = plan_reply("第二步改成生成表格");
        assert_eq!(d, Decision::Revise);
        assert_eq!(feedback.as_deref(), Some("第二步改成生成表格"));
    }

    #[test]
    fn rate_limit_per_key() {
        for _ in 0..RATE_LIMIT_PER_MINUTE {
            assert!(allow("test-key"));
        }
        assert!(!allow("test-key"));
        assert!(allow("other-key"));
    }

    #[test]
    fn progress_tracks_steps_and_text() {
        let ev = |event: &str, data: Value| TaskEvent { seq: 0, event: event.into(), data };
        let mut p = Progress::default();
        assert!(p.apply(&ev("tool_start", json!({ "call_id": "c1", "tool": "web_search" }))));
        assert!(!p.apply(&ev("tool_call", json!({ "call_id": "c1", "tool": "web_search" }))));
        p.apply(&ev("tool_result", json!({ "call_id": "c1", "success": true })));
        p.apply(&ev("tool_start", json!({ "call_id": "c2", "tool": "doc_generate" })));
        p.apply(&ev("message", json!({ "text": "正在写" })));
        assert_eq!(p.running_body(), "✅ 联网搜索\n⏳ 生成 Word 文档\n\n正在写");
        assert!(!p.is_finished());
        p.apply(&ev("done", json!({ "summary": "好了" })));
        assert!(p.is_finished());
    }
}
