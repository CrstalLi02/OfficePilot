//! 用编辑器套件（suite/，OfficePilot Suite）打开工作区文件，保存后同步回工作区。只在桌面版开放。
//!
//! - 工作区文件在磁盘上以 uuid 命名，所以先复制成「工作副本」`数据目录/open/<文件 id>/<原文件名>`，
//!   编辑器打开的是工作副本；同目录的 `.officepilot-open.json` 记录同步状态。
//! - 后台每秒检查一次：工作副本内容稳定 1.5 秒后视为一次保存。工作区文件在复制之后没被改过时，
//!   按 E1 的快照方式写回（可撤销）；被 AI 或其它途径改过时两边都保留，编辑器的版本另存为
//!   「原名（编辑器版本）」，之后的保存继续写进这份另存的文件。
//! - 工作副本没有未同步的修改、而工作区文件变了（AI 修改、撤销）时，用新内容刷新工作副本；
//!   编辑器下次保存时由套件自己提示外部修改。
//! - 已同步且 7 天没动的工作副本自动删除。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::db::file_repo::{self, FileRow};
use crate::db::DbPool;
use crate::error::{AppError, AppResult};
use crate::workspace::{self, Tree};

/// 编辑器套件能打开的类型
pub const SUPPORTED: &[&str] = &["docx", "xlsx", "pptx", "pdf", "md", "markdown", "html", "htm"];
const MANIFEST: &str = ".officepilot-open.json";
/// 工作副本内容保持不变这么久才算保存完成（编辑器可能分几次写入）
const SETTLE: Duration = Duration::from_millis(1500);
const POLL: Duration = Duration::from_secs(1);
const KEEP_DAYS: i64 = 7;

fn bad(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

/// 桌面版，或本机开发时设置 WALIOFFICE_LOCAL_EDITOR=1
pub fn enabled() -> bool {
    crate::config::is_desktop() || std::env::var("WALIOFFICE_LOCAL_EDITOR").map(|v| v == "1").unwrap_or(false)
}

pub fn supported(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    SUPPORTED.contains(&ext.as_str())
}

pub fn open_root() -> PathBuf {
    PathBuf::from(&crate::config::config().data_dir).join("open")
}

/// 编辑器套件的位置：桌面版由环境变量告知（随安装包附带）；本机开发时用仓库里构建好的 .app
pub fn suite_app() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OFFICEPILOT_SUITE_APP").map(PathBuf::from).filter(|p| p.exists()) {
        return Some(p);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../suite/apps/shell/release/mac-arm64/OfficePilot Suite.app").canonicalize().ok()
}

/// 用编辑器套件打开文件；套件已经在运行时，由它的单实例机制转给现有窗口
pub fn launch(app: &Path, file: &Path) -> AppResult<()> {
    let mut cmd = if cfg!(target_os = "macos") && app.extension().is_some_and(|e| e == "app") {
        let mut c = std::process::Command::new("open");
        c.arg("-a").arg(app).arg(file);
        c
    } else {
        let mut c = std::process::Command::new(app);
        c.arg(file);
        c
    };
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().map_err(|e| bad(format!("启动编辑器失败：{e}")))?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Conflict {
    /// 另存的「编辑器版本」文件
    file_id: String,
    /// 最近一次写进去的内容哈希；之后被别人改过就不再写入
    hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    owner_id: String,
    file_id: String,
    /// 工作副本的文件名（打开时的原文件名）
    name: String,
    /// 工作区文件在上次同步时的内容哈希
    base_hash: String,
    /// 工作副本在上次同步时的内容哈希
    synced_hash: String,
    touched_at: String,
    #[serde(default)]
    conflict: Option<Conflict>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// 文件名里不能出现路径分隔符；以点开头会变成隐藏文件
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':') || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() { "文件".into() } else { cleaned }
}

fn copy_dir(root: &Path, file_id: &str) -> AppResult<PathBuf> {
    if file_id.is_empty() || !file_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(bad("文件 id 不合法"));
    }
    Ok(root.join(file_id))
}

async fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = tokio::fs::read_to_string(dir.join(MANIFEST)).await.ok()?;
    serde_json::from_str(&text).ok()
}

async fn write_manifest(dir: &Path, m: &Manifest) -> AppResult<()> {
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(m)?).await?;
    tokio::fs::rename(&tmp, dir.join(MANIFEST)).await?;
    Ok(())
}

/// 写工作副本：先写临时文件再改名，编辑器不会读到写了一半的文件
async fn write_copy(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let tmp = path.with_file_name(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 编辑操作串行执行：打开文件的请求和后台同步不会同时改一份工作副本
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 准备工作副本并返回它的路径。已有工作副本且里面有未同步的修改时直接沿用。
pub async fn prepare_copy(root: &Path, owner_id: &str, file: &FileRow) -> AppResult<PathBuf> {
    let _guard = LOCK.lock().await;
    let dir = copy_dir(root, &file.id)?;
    if let Some(mut m) = read_manifest(&dir).await {
        if m.owner_id != owner_id {
            return Err(bad("这个文件不属于当前用户"));
        }
        let working = dir.join(&m.name);
        if let Ok(bytes) = tokio::fs::read(&working).await {
            let dirty = sha256_hex(&bytes) != m.synced_hash;
            // 文件改名后，没有未同步修改的工作副本换成新名字
            if dirty || m.name == safe_name(&file.name) {
                if !dirty {
                    let current = workspace::read_bytes(file).await?;
                    let hash = sha256_hex(&current);
                    // 冲突后工作副本跟着另存的那份；用户重新打开原文件时换回原文件的内容
                    if hash != m.base_hash || m.conflict.take().is_some() {
                        write_copy(&working, &current).await?;
                        m.base_hash = hash.clone();
                        m.synced_hash = hash;
                    }
                }
                m.touched_at = now();
                write_manifest(&dir, &m).await?;
                return Ok(working);
            }
            let _ = tokio::fs::remove_file(&working).await;
        }
    }
    tokio::fs::create_dir_all(&dir).await?;
    let bytes = workspace::read_bytes(file).await?;
    let name = safe_name(&file.name);
    let working = dir.join(&name);
    write_copy(&working, &bytes).await?;
    let hash = sha256_hex(&bytes);
    let m = Manifest { owner_id: owner_id.into(), file_id: file.id.clone(), name, base_hash: hash.clone(), synced_hash: hash, touched_at: now(), conflict: None };
    write_manifest(&dir, &m).await?;
    Ok(working)
}

/// 一次同步的结果
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// 编辑器的保存已写回原文件
    Saved { owner_id: String, path: String, op_id: Option<String> },
    /// 原文件在此期间被改过，编辑器的版本另存了一份
    Conflict { owner_id: String, path: String, original: String, op_id: Option<String> },
    /// 原文件已被删除，编辑器的版本另存了一份
    Recovered { owner_id: String, path: String, op_id: Option<String> },
    /// 工作区文件变了，工作副本已刷新
    Refreshed,
}

fn editor_copy_name(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}（编辑器版本）{}", &name[..i], &name[i..]),
        _ => format!("{name}（编辑器版本）"),
    }
}

pub fn mime_of(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).as_deref() {
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("pdf") => "application/pdf",
        Some("md" | "markdown") => "text/markdown",
        Some("html" | "htm") => "text/html",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
}

/// 另存为新文件，返回 (虚拟路径, 文件 id, op id)
async fn save_new(pool: &DbPool, owner_id: &str, path: &str, bytes: &[u8]) -> AppResult<(String, String, Option<String>)> {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let (final_path, op_id) = workspace::save_bytes_as(pool, owner_id, path, bytes, mime_of(&name), None).await?;
    let tree = Tree::load(pool, owner_id).await?;
    let id = tree.find_file(&final_path).map_err(bad)?.map(|f| f.id.clone()).ok_or_else(|| bad("另存的文件没有找到"))?;
    Ok((final_path, id, op_id))
}

/// 检查一份工作副本：编辑器保存过就写回工作区，工作区变了就刷新工作副本
pub async fn sync_one(pool: &DbPool, dir: &Path) -> AppResult<Option<Outcome>> {
    let _guard = LOCK.lock().await;
    let Some(mut m) = read_manifest(dir).await else { return Ok(None) };
    let working = dir.join(&m.name);
    let Ok(edited) = tokio::fs::read(&working).await else { return Ok(None) };
    let edited_hash = sha256_hex(&edited);
    let file = file_repo::get_file(pool, &m.owner_id, &m.file_id).await?;

    if edited_hash == m.synced_hash {
        // 编辑器没有保存过；工作区文件变了（AI 修改、撤销）就刷新工作副本。
        // 冲突后工作副本已经跟着另存的那份，不再随原文件变化
        if m.conflict.is_some() {
            return Ok(None);
        }
        let Some(file) = file else { return Ok(None) };
        let Ok(current) = tokio::fs::read(&file.file_path).await else { return Ok(None) };
        let hash = sha256_hex(&current);
        if hash == m.base_hash {
            return Ok(None);
        }
        write_copy(&working, &current).await?;
        m.base_hash = hash.clone();
        m.synced_hash = hash;
        write_manifest(dir, &m).await?;
        return Ok(Some(Outcome::Refreshed));
    }

    m.touched_at = now();
    let owner_id = m.owner_id.clone();
    let outcome = match file {
        None => {
            let (path, id, op_id) = save_new(pool, &owner_id, &format!("/{}", editor_copy_name(&m.name)), &edited).await?;
            // 之后的保存写进这份新文件
            m.file_id = id;
            m.base_hash = edited_hash.clone();
            m.conflict = None;
            Outcome::Recovered { owner_id: owner_id.clone(), path, op_id }
        }
        Some(file) => {
            let tree = Tree::load(pool, &owner_id).await?;
            let original = tree.file_path(&file);
            let current = workspace::read_bytes(&file).await?;
            let current_hash = sha256_hex(&current);
            if m.conflict.is_none() && (current_hash == m.base_hash || current_hash == edited_hash) {
                let op_id = if current_hash == edited_hash {
                    None
                } else {
                    workspace::replace_content(pool, &owner_id, &file, &edited, &format!("编辑器保存 {original}"), None).await?
                };
                m.base_hash = edited_hash.clone();
                Outcome::Saved { owner_id: owner_id.clone(), path: original, op_id }
            } else {
                // 原文件在此期间被改过（或之前已经冲突过）：两边都保留，原文件不动。
                // 已经另存过且那份没被别人改过时，继续写进那份，否则再另存一份
                m.base_hash = current_hash;
                let previous = match &m.conflict {
                    Some(c) => file_repo::get_file(pool, &owner_id, &c.file_id).await?.filter(|f| std::fs::read(&f.file_path).map(|b| sha256_hex(&b) == c.hash).unwrap_or(false)),
                    None => None,
                };
                let (path, op_id) = match previous {
                    Some(copy) => {
                        let path = tree.file_path(&copy);
                        let op = workspace::replace_content(pool, &owner_id, &copy, &edited, &format!("编辑器保存 {path}"), None).await?;
                        (path, op)
                    }
                    None => {
                        let dir_path = tree.folder_path(file.folder_id.as_deref());
                        let target = format!("{}/{}", dir_path.trim_end_matches('/'), editor_copy_name(&file.name));
                        let (path, id, op) = save_new(pool, &owner_id, &target, &edited).await?;
                        m.conflict = Some(Conflict { file_id: id, hash: String::new() });
                        (path, op)
                    }
                };
                if let Some(c) = m.conflict.as_mut() {
                    c.hash = edited_hash.clone();
                }
                Outcome::Conflict { owner_id: owner_id.clone(), path, original, op_id }
            }
        }
    };
    m.synced_hash = edited_hash;
    write_manifest(dir, &m).await?;
    Ok(Some(outcome))
}

/// 删除已同步且很久没动的工作副本，以及原文件已删除、也没有未同步修改的工作副本
pub async fn cleanup(pool: &DbPool, root: &Path, now: chrono::DateTime<chrono::Utc>) -> usize {
    let _guard = LOCK.lock().await;
    let Ok(mut entries) = tokio::fs::read_dir(root).await else { return 0 };
    let mut removed = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let dir = entry.path();
        let Some(m) = read_manifest(&dir).await else { continue };
        let clean = tokio::fs::read(dir.join(&m.name)).await.map(|b| sha256_hex(&b) == m.synced_hash).unwrap_or(true);
        if !clean {
            continue;
        }
        let stale = chrono::DateTime::parse_from_rfc3339(&m.touched_at).map(|t| now.signed_duration_since(t) > chrono::Duration::days(KEEP_DAYS)).unwrap_or(true);
        let gone = matches!(file_repo::get_file(pool, &m.owner_id, &m.file_id).await, Ok(None));
        if (stale || gone) && tokio::fs::remove_dir_all(&dir).await.is_ok() {
            removed += 1;
        }
    }
    removed
}

/// 这个文件当前的工作副本（用编辑器打开过且还在）
pub async fn working_copy(root: &Path, owner_id: &str, file_id: &str) -> Option<PathBuf> {
    let dir = copy_dir(root, file_id).ok()?;
    let m = read_manifest(&dir).await?;
    let path = dir.join(&m.name);
    (m.owner_id == owner_id && path.is_file()).then_some(path)
}

/// 正在用编辑器打开的文件（工作副本）
#[derive(Debug, Serialize)]
pub struct OpenCopy {
    pub file_id: String,
    pub name: String,
    pub path: String,
    /// 有还没同步回工作区的修改
    pub dirty: bool,
    pub conflict_file_id: Option<String>,
}

pub async fn list_open(root: &Path, owner_id: &str) -> Vec<OpenCopy> {
    let mut out = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(root).await else { return out };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let dir = entry.path();
        let Some(m) = read_manifest(&dir).await else { continue };
        if m.owner_id != owner_id {
            continue;
        }
        let working = dir.join(&m.name);
        let Ok(bytes) = tokio::fs::read(&working).await else { continue };
        out.push(OpenCopy {
            file_id: m.file_id,
            name: m.name,
            path: working.to_string_lossy().into_owned(),
            dirty: sha256_hex(&bytes) != m.synced_hash,
            conflict_file_id: m.conflict.map(|c| c.file_id),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

async fn notify(pool: &DbPool, outcome: &Outcome) {
    let (owner, title, content) = match outcome {
        Outcome::Saved { owner_id, path, op_id: Some(_) } => (owner_id, format!("已从编辑器保存到 {path}"), "可以在「我的文件」的整理记录里撤销".to_string()),
        Outcome::Conflict { owner_id, path, original, .. } => (
            owner_id,
            format!("{original} 在编辑期间被修改过"),
            format!("两边都已保留：编辑器的版本已另存为 {path}"),
        ),
        Outcome::Recovered { owner_id, path, .. } => (owner_id, "原文件已被删除".to_string(), format!("编辑器的版本已另存为 {path}")),
        _ => return,
    };
    if let Err(e) = crate::db::notification_repo::create(pool, owner, "editor", &title, &content, Some("/files")).await {
        tracing::warn!("[Editor] 写入通知失败: {e:?}");
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

async fn stamp(path: &Path) -> Option<Stamp> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some(Stamp { modified: meta.modified().ok(), len: meta.len() })
}

/// 每份工作副本的观察记录：上次看到的状态、从何时起保持不变、对应的工作区文件状态
#[derive(Default)]
struct Watch {
    copy: Option<Stamp>,
    since: Option<Instant>,
    handled: Option<Stamp>,
    original: Option<Stamp>,
}

/// 后台同步：只看文件的修改时间和大小，变化并稳定后才读内容比哈希
pub fn spawn_sync() {
    tokio::spawn(async move {
        let root = open_root();
        let mut watches: HashMap<PathBuf, Watch> = HashMap::new();
        let mut last_cleanup = Instant::now() - Duration::from_secs(3600);
        loop {
            tokio::time::sleep(POLL).await;
            let pool = crate::state::db_pool();
            if last_cleanup.elapsed() > Duration::from_secs(3600) {
                last_cleanup = Instant::now();
                let n = cleanup(&pool, &root, chrono::Utc::now()).await;
                if n > 0 {
                    tracing::info!("[Editor] 清理了 {n} 份工作副本");
                }
            }
            let Ok(mut entries) = tokio::fs::read_dir(&root).await else { continue };
            let mut seen = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let dir = entry.path();
                let Some(m) = read_manifest(&dir).await else { continue };
                seen.push(dir.clone());
                let copy = stamp(&dir.join(&m.name)).await;
                let original = match file_repo::get_file(&pool, &m.owner_id, &m.file_id).await {
                    Ok(Some(f)) => stamp(Path::new(&f.file_path)).await,
                    _ => None,
                };
                let w = watches.entry(dir.clone()).or_default();
                if w.copy != copy {
                    w.copy = copy;
                    w.since = Some(Instant::now());
                }
                let first = w.handled.is_none();
                let copy_changed = w.handled != copy;
                let copy_settled = copy_changed && w.since.is_some_and(|t| t.elapsed() >= SETTLE);
                // 编辑器正在写入时先不处理工作区的变化，等写完一起处理
                let original_changed = !first && !copy_changed && w.original != original;
                if !(first || copy_settled || original_changed) {
                    continue;
                }
                match sync_one(&pool, &dir).await {
                    Ok(Some(outcome)) => {
                        tracing::info!("[Editor] {}: {outcome:?}", m.name);
                        notify(&pool, &outcome).await;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("[Editor] 同步 {} 失败: {e:?}", m.name),
                }
                // 同步可能改写了两边的文件，记录同步之后的状态
                w.copy = stamp(&dir.join(&m.name)).await;
                w.handled = w.copy;
                w.original = match file_repo::get_file(&pool, &m.owner_id, &m.file_id).await {
                    Ok(Some(f)) => stamp(Path::new(&f.file_path)).await,
                    _ => None,
                };
            }
            watches.retain(|k, _| seen.contains(k));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_pool() -> DbPool {
        sqlx::any::install_default_drivers();
        let pool = sqlx::any::AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        for sql in [
            include_str!("../../migrations/001_init.sql"),
            include_str!("../../migrations/004_workspace.sql"),
            "INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES ('u1', 'u', 'x', 't', 't');",
        ] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("officepilot-open-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    async fn file(pool: &DbPool, path: &str) -> FileRow {
        Tree::load(pool, "u1").await.unwrap().find_file(path).unwrap().unwrap().clone()
    }

    async fn undo_last(pool: &DbPool) -> workspace::UndoReport {
        let id: String = sqlx::query_scalar("SELECT id FROM workspace_ops ORDER BY created_at DESC LIMIT 1").fetch_one(pool).await.unwrap();
        workspace::undo(pool, "u1", &id).await.unwrap()
    }

    #[test]
    fn names() {
        assert!(supported("报告.DOCX") && supported("a.md") && !supported("a.zip") && !supported("docx"));
        assert_eq!(safe_name("../a/b:c.docx"), "_a_b_c.docx");
        assert_eq!(safe_name(".."), "文件");
        assert_eq!(editor_copy_name("报告.docx"), "报告（编辑器版本）.docx");
        assert_eq!(editor_copy_name("README"), "README（编辑器版本）");
        assert!(copy_dir(Path::new("/x"), "../etc").is_err());
    }

    #[tokio::test]
    async fn open_edit_sync_back_and_undo() {
        let pool = memory_pool().await;
        let root = temp_root();
        workspace::write_text(&pool, "u1", "/项目/报告.md", "第一版", None).await.unwrap();
        let f = file(&pool, "/项目/报告.md").await;

        let working = prepare_copy(&root, "u1", &f).await.unwrap();
        assert_eq!(working.file_name().unwrap(), "报告.md", "工作副本用原文件名");
        assert_eq!(std::fs::read_to_string(&working).unwrap(), "第一版");
        assert!(prepare_copy(&root, "u2", &f).await.is_err(), "别人的工作副本不能用");
        let dir = working.parent().unwrap().to_path_buf();
        assert_eq!(sync_one(&pool, &dir).await.unwrap(), None, "没改过就什么都不做");

        // 编辑器保存
        std::fs::write(&working, "编辑器改的").unwrap();
        let open = list_open(&root, "u1").await;
        assert!(open[0].dirty);
        match sync_one(&pool, &dir).await.unwrap() {
            Some(Outcome::Saved { path, op_id: Some(_), .. }) => assert_eq!(path, "/项目/报告.md"),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&f.file_path).unwrap(), "编辑器改的");
        assert!(!list_open(&root, "u1").await[0].dirty);
        assert_eq!(sync_one(&pool, &dir).await.unwrap(), None, "同一次保存只同步一次");

        // 未同步的修改在重新打开时保留
        std::fs::write(&working, "还没同步").unwrap();
        prepare_copy(&root, "u1", &f).await.unwrap();
        assert_eq!(std::fs::read_to_string(&working).unwrap(), "还没同步");
        sync_one(&pool, &dir).await.unwrap();

        // 撤销编辑器的保存：工作区恢复，工作副本随之刷新
        assert_eq!(undo_last(&pool).await.undone, 1);
        assert_eq!(std::fs::read_to_string(&f.file_path).unwrap(), "编辑器改的");
        assert_eq!(sync_one(&pool, &dir).await.unwrap(), Some(Outcome::Refreshed));
        assert_eq!(std::fs::read_to_string(&working).unwrap(), "编辑器改的");
        assert_eq!(undo_last_two(&pool).await, "第一版");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&f.file_path);
    }

    /// 再撤销更早那次编辑器保存，返回原文件内容
    async fn undo_last_two(pool: &DbPool) -> String {
        let id: String = sqlx::query_scalar("SELECT id FROM workspace_ops WHERE undone_at IS NULL AND summary LIKE '编辑器保存%' ORDER BY created_at DESC LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap();
        workspace::undo(pool, "u1", &id).await.unwrap();
        let f = file(pool, "/项目/报告.md").await;
        std::fs::read_to_string(&f.file_path).unwrap()
    }

    #[tokio::test]
    async fn ai_change_refreshes_clean_copy_and_conflict_keeps_both() {
        let pool = memory_pool().await;
        let root = temp_root();
        workspace::write_text(&pool, "u1", "/报告.md", "原文", None).await.unwrap();
        let f = file(&pool, "/报告.md").await;
        let working = prepare_copy(&root, "u1", &f).await.unwrap();
        let dir = working.parent().unwrap().to_path_buf();

        // AI 改了文件，编辑器那边没改：刷新工作副本
        workspace::replace_content(&pool, "u1", &f, "AI 第一次".as_bytes(), "AI", None).await.unwrap();
        assert_eq!(sync_one(&pool, &dir).await.unwrap(), Some(Outcome::Refreshed));
        assert_eq!(std::fs::read_to_string(&working).unwrap(), "AI 第一次");

        // 两边都改了：原文件保留 AI 的版本，编辑器的版本另存
        workspace::replace_content(&pool, "u1", &f, "AI 第二次".as_bytes(), "AI", None).await.unwrap();
        std::fs::write(&working, "人改的").unwrap();
        match sync_one(&pool, &dir).await.unwrap() {
            Some(Outcome::Conflict { path, original, .. }) => {
                assert_eq!(path, "/报告（编辑器版本）.md");
                assert_eq!(original, "/报告.md");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&f.file_path).unwrap(), "AI 第二次");
        let copy = file(&pool, "/报告（编辑器版本）.md").await;
        assert_eq!(std::fs::read_to_string(&copy.file_path).unwrap(), "人改的");

        // 之后的保存继续写进另存的那份，不再新建
        std::fs::write(&working, "人又改了").unwrap();
        match sync_one(&pool, &dir).await.unwrap() {
            Some(Outcome::Conflict { path, .. }) => assert_eq!(path, "/报告（编辑器版本）.md"),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&copy.file_path).unwrap(), "人又改了");
        assert_eq!(Tree::load(&pool, "u1").await.unwrap().files().len(), 2);
        assert_eq!(list_open(&root, "u1").await[0].conflict_file_id.as_deref(), Some(copy.id.as_str()));

        // 冲突后原文件再变（AI 又改、服务重启后重新检查），编辑器里的内容不被替换
        workspace::replace_content(&pool, "u1", &f, "AI 第三次".as_bytes(), "AI", None).await.unwrap();
        assert_eq!(sync_one(&pool, &dir).await.unwrap(), None);
        assert_eq!(std::fs::read_to_string(&working).unwrap(), "人又改了");

        // 另存的那份被别人改过：再另存一份，不覆盖
        std::fs::write(&copy.file_path, "别人改了另存的那份").unwrap();
        std::fs::write(&working, "人第三次改").unwrap();
        match sync_one(&pool, &dir).await.unwrap() {
            Some(Outcome::Conflict { path, .. }) => assert_eq!(path, "/报告（编辑器版本） (2).md"),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&f.file_path).unwrap(), "AI 第三次");

        // 用户重新打开原文件：工作副本换回原文件的内容，之后的保存写回原文件
        prepare_copy(&root, "u1", &f).await.unwrap();
        assert_eq!(std::fs::read_to_string(&working).unwrap(), "AI 第三次");
        assert!(list_open(&root, "u1").await[0].conflict_file_id.is_none());
        std::fs::write(&working, "回到原文件").unwrap();
        assert!(matches!(sync_one(&pool, &dir).await.unwrap(), Some(Outcome::Saved { .. })));
        assert_eq!(std::fs::read_to_string(&f.file_path).unwrap(), "回到原文件");

        for f in Tree::load(&pool, "u1").await.unwrap().files() {
            let _ = std::fs::remove_file(&f.file_path);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn deleted_original_is_recovered_and_cleanup() {
        let pool = memory_pool().await;
        let root = temp_root();
        workspace::write_text(&pool, "u1", "/a/笔记.md", "x", None).await.unwrap();
        workspace::write_text(&pool, "u1", "/旧.md", "y", None).await.unwrap();
        let f = file(&pool, "/a/笔记.md").await;
        let old = file(&pool, "/旧.md").await;
        let working = prepare_copy(&root, "u1", &f).await.unwrap();
        prepare_copy(&root, "u1", &old).await.unwrap();

        file_repo::delete_file(&pool, "u1", &f.id).await.unwrap();
        std::fs::write(&working, "删除后保存").unwrap();
        // 有未同步修改的工作副本不清理
        assert_eq!(cleanup(&pool, &root, chrono::Utc::now()).await, 0);
        match sync_one(&pool, working.parent().unwrap()).await.unwrap() {
            Some(Outcome::Recovered { path, .. }) => assert_eq!(path, "/笔记（编辑器版本）.md"),
            other => panic!("{other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&file(&pool, "/笔记（编辑器版本）.md").await.file_path).unwrap(), "删除后保存");

        // 7 天后：已同步的工作副本都清理掉
        assert_eq!(cleanup(&pool, &root, chrono::Utc::now()).await, 0);
        assert_eq!(cleanup(&pool, &root, chrono::Utc::now() + chrono::Duration::days(8)).await, 2);
        assert!(list_open(&root, "u1").await.is_empty());
        for f in Tree::load(&pool, "u1").await.unwrap().files() {
            let _ = std::fs::remove_file(&f.file_path);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
