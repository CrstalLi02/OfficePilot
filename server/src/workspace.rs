//! 工作区：把用户的文件 / 文件夹（`files`、`folders` 表 + 磁盘）以虚拟路径的形式提供给 Agent。
//!
//! - 路径形如 `/项目A/合同/报价单.docx`，`/` 是根目录；也可以直接用文件 id。
//! - 只读：列目录、全文搜索、读取（提取文本带缓存）。
//! - 写入：新建文本文件、导出产物、批量整理（建目录 / 移动 / 重命名 / 转换），
//!   每次都记录逆操作，可整体撤销。不提供删除。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;

use crate::db::file_repo::{self, FileRow, FolderRow};
use crate::db::{text_column, DbPool};
use crate::error::{AppError, AppResult};

/// 单次读取返回的最大字符数
pub const MAX_READ_CHARS: usize = 20_000;
/// 一次整理最多的操作数
pub const MAX_ORGANIZE_OPS: usize = 200;

fn bad(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

// ───────────────────────── 路径 ─────────────────────────

/// 规范化虚拟路径为各级名称；拒绝 `..`、反斜杠、控制字符
pub fn normalize(path: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    for part in path.split('/') {
        let part = part.trim();
        match part {
            "" | "." => continue,
            ".." => return Err("路径不能包含 ..".to_string()),
            _ => {}
        }
        if part.contains('\\') || part.chars().any(char::is_control) {
            return Err(format!("名称不合法：{part}"));
        }
        if part.chars().count() > 200 {
            return Err("名称过长".to_string());
        }
        parts.push(part.to_string());
    }
    Ok(parts)
}

/// 文件夹内名称冲突时追加序号：报价单.docx → 报价单 (2).docx
pub fn unique_name(taken: &HashSet<String>, name: &str) -> String {
    if !taken.contains(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("unbounded")
}

/// 整个工作区的快照
pub struct Tree {
    folders: HashMap<String, FolderRow>,
    files: Vec<FileRow>,
}

impl Tree {
    pub async fn load(pool: &DbPool, owner_id: &str) -> AppResult<Self> {
        let folder_rows = sqlx::query("SELECT id, name, parent_id, created_at, updated_at FROM folders WHERE owner_id = ?")
            .bind(owner_id)
            .fetch_all(pool)
            .await?;
        let mut folders = HashMap::new();
        for row in folder_rows {
            let folder = FolderRow {
                id: row.try_get(0)?,
                owner_id: owner_id.to_string(),
                name: text_column(&row, 1)?.unwrap_or_default(),
                parent_id: text_column(&row, 2)?,
                created_at: text_column(&row, 3)?.unwrap_or_default(),
                updated_at: text_column(&row, 4)?.unwrap_or_default(),
            };
            folders.insert(folder.id.clone(), folder);
        }
        let files = file_repo::search_files(pool, owner_id, None).await?;
        Ok(Self { folders, files })
    }

    #[cfg(test)]
    pub fn from_parts(folders: Vec<FolderRow>, files: Vec<FileRow>) -> Self {
        Self {
            folders: folders.into_iter().map(|f| (f.id.clone(), f)).collect(),
            files,
        }
    }

    pub fn files(&self) -> &[FileRow] {
        &self.files
    }

    pub fn folder_path(&self, folder_id: Option<&str>) -> String {
        let mut names = Vec::new();
        let mut current = folder_id.map(str::to_string);
        let mut guard = 0;
        while let Some(id) = current {
            let Some(folder) = self.folders.get(&id) else { break };
            names.push(folder.name.clone());
            current = folder.parent_id.clone();
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        names.reverse();
        format!("/{}", names.join("/"))
    }

    pub fn file_path(&self, file: &FileRow) -> String {
        let dir = self.folder_path(file.folder_id.as_deref());
        if dir == "/" {
            format!("/{}", file.name)
        } else {
            format!("{dir}/{}", file.name)
        }
    }

    fn child_folder(&self, parent: Option<&str>, name: &str) -> Option<&FolderRow> {
        self.folders
            .values()
            .filter(|f| f.parent_id.as_deref() == parent && f.name == name)
            .min_by(|a, b| a.created_at.cmp(&b.created_at))
    }

    /// 解析文件夹路径；`Some(None)` 表示根目录
    pub fn find_folder(&self, path: &str) -> Result<Option<Option<String>>, String> {
        let mut current: Option<String> = None;
        for part in normalize(path)? {
            match self.child_folder(current.as_deref(), &part) {
                Some(folder) => current = Some(folder.id.clone()),
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    /// 按 id 或路径找文件；同名文件取最近修改的
    pub fn find_file(&self, path_or_id: &str) -> Result<Option<&FileRow>, String> {
        let key = path_or_id.trim();
        if let Some(file) = self.files.iter().find(|f| f.id == key) {
            return Ok(Some(file));
        }
        let mut parts = normalize(key)?;
        let Some(name) = parts.pop() else { return Ok(None) };
        let Some(folder) = self.find_folder(&parts.join("/"))? else { return Ok(None) };
        Ok(self
            .files
            .iter()
            .filter(|f| f.folder_id == folder && f.name == name)
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at)))
    }

    /// 文件夹内已占用的名称（文件 + 子文件夹）
    pub fn names_in(&self, folder: Option<&str>) -> HashSet<String> {
        self.files
            .iter()
            .filter(|f| f.folder_id.as_deref() == folder)
            .map(|f| f.name.clone())
            .chain(self.folders.values().filter(|d| d.parent_id.as_deref() == folder).map(|d| d.name.clone()))
            .collect()
    }

    /// folder 是否位于 ancestor 之内（含自身）
    fn is_within(&self, folder: Option<&str>, ancestor: &str) -> bool {
        let mut current = folder.map(str::to_string);
        let mut guard = 0;
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            current = self.folders.get(&id).and_then(|f| f.parent_id.clone());
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        false
    }

    fn descendants(&self, folder: Option<&str>) -> HashSet<Option<String>> {
        let mut out: HashSet<Option<String>> = HashSet::from([folder.map(str::to_string)]);
        loop {
            let before = out.len();
            for f in self.folders.values() {
                if out.contains(&f.parent_id) {
                    out.insert(Some(f.id.clone()));
                }
            }
            if out.len() == before {
                return out;
            }
        }
    }
}

// ───────────────────────── 只读：列目录 / 读取 / 搜索 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub path: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    pub updated_at: String,
}

/// 简单的通配符匹配（`*` 任意串，`?` 单个字符），不区分大小写
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn rec(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => rec(&p[1..], n) || (!n.is_empty() && rec(p, &n[1..])),
            (Some('?'), Some(_)) => rec(&p[1..], &n[1..]),
            (Some(a), Some(b)) => a == b && rec(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    rec(&p, &n)
}

pub fn list(tree: &Tree, path: &str, recursive: bool, pattern: Option<&str>, limit: usize) -> Result<(Vec<Entry>, usize), String> {
    let Some(folder) = tree.find_folder(path)? else {
        return Err(format!("目录不存在：{path}"));
    };
    let scope = if recursive { tree.descendants(folder.as_deref()) } else { HashSet::from([folder.clone()]) };
    let matches = |name: &str| pattern.map_or(true, |p| glob_match(p, name));
    let mut entries: Vec<Entry> = tree
        .folders
        .values()
        .filter(|f| scope.contains(&f.parent_id) && matches(&f.name))
        .map(|f| Entry {
            path: tree.folder_path(Some(&f.id)) + "/",
            kind: "folder",
            id: None,
            size: None,
            updated_at: f.updated_at.clone(),
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut files: Vec<Entry> = tree
        .files
        .iter()
        .filter(|f| scope.contains(&f.folder_id) && matches(&f.name))
        .map(|f| Entry {
            path: tree.file_path(f),
            kind: "file",
            id: Some(f.id.clone()),
            size: Some(f.file_size),
            updated_at: f.updated_at.clone(),
        })
        .collect();
    files.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    entries.extend(files);
    let total = entries.len();
    entries.truncate(limit);
    Ok((entries, total))
}

/// 文件的提取文本（优先读缓存）
pub async fn text_of(pool: &DbPool, owner_id: &str, file: &FileRow) -> AppResult<String> {
    if let Some(row) = sqlx::query("SELECT text FROM file_texts WHERE file_id = ? AND owner_id = ?")
        .bind(&file.id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await?
    {
        return Ok(text_column(&row, 0)?.unwrap_or_default());
    }
    let bytes = tokio::fs::read(&file.file_path)
        .await
        .map_err(|e| bad(format!("读取文件失败：{e}")))?;
    let mime = mime_guess::from_path(&file.name).first_or_octet_stream().to_string();
    let name = file.name.clone();
    let extracted = tokio::task::spawn_blocking(move || crate::file_extract::extract_text_from_bytes(&name, &mime, &bytes))
        .await
        .map_err(|e| bad(format!("解析文件失败：{e}")))?;
    sqlx::query("DELETE FROM file_texts WHERE file_id = ?").bind(&file.id).execute(pool).await?;
    sqlx::query("INSERT INTO file_texts (file_id, owner_id, text, parser, extracted_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&file.id)
        .bind(owner_id)
        .bind(&extracted.text)
        .bind(&extracted.parser)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
    Ok(extracted.text)
}

/// 按字符截取一段，返回 (片段, 总字符数, 下一段起点)
pub fn slice_chars(text: &str, offset: usize, limit: usize) -> (String, usize, Option<usize>) {
    let total = text.chars().count();
    let limit = limit.clamp(1, MAX_READ_CHARS);
    let piece: String = text.chars().skip(offset).take(limit).collect();
    let next = (offset + limit < total).then_some(offset + limit);
    (piece, total, next)
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub id: String,
    /// name | content
    pub matched: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// 关键词在文本中的上下文片段
pub fn snippet(text: &str, terms: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    let pos = terms.iter().filter_map(|t| lower.find(&t.to_lowercase())).min()?;
    let char_pos = lower[..pos].chars().count();
    let start = char_pos.saturating_sub(40);
    let piece: String = text.chars().skip(start).take(120).collect();
    Some(piece.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn is_text_like(file: &FileRow) -> bool {
    matches!(file.file_type.as_str(), "doc" | "excel" | "ppt" | "drawio") && file.file_size < 10 * 1024 * 1024
}

/// 文件名 + 正文搜索。每次最多为 20 个尚未建索引的文件补齐提取文本
pub async fn search(pool: &DbPool, owner_id: &str, tree: &Tree, query: &str, limit: usize) -> AppResult<(Vec<SearchHit>, usize, usize)> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_string).filter(|t| !t.is_empty()).collect();
    if terms.is_empty() {
        return Err(bad("搜索词不能为空"));
    }
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    for file in tree.files() {
        let path = tree.file_path(file).to_lowercase();
        if terms.iter().any(|t| path.contains(&t.to_lowercase())) {
            seen.insert(file.id.clone());
            hits.push(SearchHit { path: tree.file_path(file), id: file.id.clone(), matched: "name", snippet: None });
        }
    }

    let indexed: HashSet<String> = sqlx::query("SELECT file_id FROM file_texts WHERE owner_id = ?")
        .bind(owner_id)
        .fetch_all(pool)
        .await?
        .iter()
        .filter_map(|r| r.try_get::<String, _>(0).ok())
        .collect();
    for file in tree.files().iter().filter(|f| is_text_like(f) && !indexed.contains(&f.id)).take(20) {
        if let Err(e) = text_of(pool, owner_id, file).await {
            tracing::debug!("[Workspace] 索引 {} 失败: {e:?}", file.name);
        }
    }
    let candidates = tree.files().iter().filter(|f| is_text_like(f)).count();

    let mut sql = String::from("SELECT file_id, text FROM file_texts WHERE owner_id = ?");
    for _ in &terms {
        sql.push_str(" AND LOWER(text) LIKE ?");
    }
    sql.push_str(" LIMIT 100");
    let mut q = sqlx::query(&sql).bind(owner_id);
    for term in &terms {
        q = q.bind(format!("%{}%", term.to_lowercase()));
    }
    let by_id: HashMap<&str, &FileRow> = tree.files().iter().map(|f| (f.id.as_str(), f)).collect();
    for row in q.fetch_all(pool).await? {
        let id: String = row.try_get(0)?;
        let Some(file) = by_id.get(id.as_str()) else { continue };
        if !seen.insert(id.clone()) {
            continue;
        }
        let text = text_column(&row, 1)?.unwrap_or_default();
        hits.push(SearchHit { path: tree.file_path(file), id, matched: "content", snippet: snippet(&text, &terms) });
    }
    let total = hits.len();
    hits.truncate(limit);
    let indexed_now = sqlx::query("SELECT COUNT(*) FROM file_texts WHERE owner_id = ?")
        .bind(owner_id)
        .fetch_one(pool)
        .await?
        .try_get::<i64, _>(0)? as usize;
    Ok((hits, total, indexed_now.min(candidates)))
}

// ───────────────────────── 写入：日志与撤销 ─────────────────────────

/// 撤销时执行的逆操作
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Inverse {
    /// 删除 Agent 新建的文件
    DeleteFile { file_id: String },
    /// 删除 Agent 新建的文件夹（仍为空时）
    DeleteFolder { folder_id: String },
    /// 把文件放回原位置、改回原名
    RestoreFile { file_id: String, folder_id: Option<String>, name: String },
    /// 把文件夹放回原位置、改回原名
    RestoreFolder { folder_id: String, parent_id: Option<String>, name: String },
}

/// 一次写入 / 整理的记录器：边执行边累积逆操作
pub struct Journal {
    pub inverse: Vec<Inverse>,
    pub lines: Vec<String>,
}

impl Journal {
    pub fn new() -> Self {
        Self { inverse: Vec::new(), lines: Vec::new() }
    }

    /// 保存记录，返回可用于撤销的 op id；没有任何改动时返回 None
    pub async fn commit(self, pool: &DbPool, owner_id: &str, session_id: Option<&str>) -> AppResult<Option<String>> {
        if self.inverse.is_empty() {
            return Ok(None);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut inverse = self.inverse;
        inverse.reverse();
        let summary: String = self.lines.join("；").chars().take(900).collect();
        sqlx::query("INSERT INTO workspace_ops (id, owner_id, session_id, summary, inverse, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(owner_id)
            .bind(session_id)
            .bind(&summary)
            .bind(serde_json::to_string(&inverse)?)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(pool)
            .await?;
        Ok(Some(id))
    }
}

async fn create_folder(pool: &DbPool, owner_id: &str, name: &str, parent: Option<&str>, journal: &mut Journal) -> AppResult<String> {
    let folder = file_repo::create_folder(pool, owner_id, name, parent).await?;
    journal.inverse.push(Inverse::DeleteFolder { folder_id: folder.id.clone() });
    Ok(folder.id)
}

/// mkdir -p：逐级查找或创建，返回最终文件夹 id（None 为根目录）
pub async fn ensure_folder(pool: &DbPool, owner_id: &str, tree: &mut Tree, path: &str, journal: &mut Journal) -> AppResult<Option<String>> {
    let mut current: Option<String> = None;
    for part in normalize(path).map_err(bad)? {
        if let Some(existing) = tree.child_folder(current.as_deref(), &part) {
            current = Some(existing.id.clone());
            continue;
        }
        let id = create_folder(pool, owner_id, &part, current.as_deref(), journal).await?;
        let now = chrono::Utc::now().to_rfc3339();
        tree.folders.insert(
            id.clone(),
            FolderRow { id: id.clone(), owner_id: owner_id.to_string(), name: part, parent_id: current.clone(), created_at: now.clone(), updated_at: now },
        );
        current = Some(id);
    }
    Ok(current)
}

/// 新建文本文件（同名自动编号，不覆盖）
pub async fn write_text(pool: &DbPool, owner_id: &str, path: &str, content: &str, session_id: Option<&str>) -> AppResult<(String, Option<String>)> {
    let mut tree = Tree::load(pool, owner_id).await?;
    let mut parts = normalize(path).map_err(bad)?;
    let name = parts.pop().ok_or_else(|| bad("缺少文件名"))?;
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    let mime = match ext.as_str() {
        "md" | "markdown" => "text/markdown",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        _ => return Err(bad("只能新建 .md / .txt / .csv / .json / .html 文件；Office 格式请先生成产物再导出")),
    };
    let mut journal = Journal::new();
    let folder = ensure_folder(pool, owner_id, &mut tree, &parts.join("/"), &mut journal).await?;
    let name = unique_name(&tree.names_in(folder.as_deref()), &name);
    let row = crate::file_store::save_file_bytes(
        pool,
        owner_id,
        folder.as_deref(),
        &name,
        content.as_bytes(),
        mime,
        "智能助手写入",
        json!({ "source": "agent_write" }),
    )
    .await
    .map_err(|e| bad(format!("写入失败：{e}")))?
    .ok_or_else(|| bad("内容为空"))?;
    let final_path = format!("{}/{}", tree.folder_path(folder.as_deref()).trim_end_matches('/'), row.name);
    journal.inverse.push(Inverse::DeleteFile { file_id: row.id.clone() });
    journal.lines.push(format!("新建 {final_path}"));
    let op_id = journal.commit(pool, owner_id, session_id).await?;
    Ok((final_path, op_id))
}

/// 把会话产物导出为文件存到指定目录
pub async fn export_artifact(
    pool: &DbPool,
    owner_id: &str,
    artifact: &crate::models::Artifact,
    folder_path: &str,
    name: Option<&str>,
    session_id: Option<&str>,
) -> AppResult<(String, Option<String>)> {
    let mut tree = Tree::load(pool, owner_id).await?;
    let mut journal = Journal::new();
    let folder = ensure_folder(pool, owner_id, &mut tree, folder_path, &mut journal).await?;
    let taken = tree.names_in(folder.as_deref());
    // 先按期望文件名渲染，再处理重名
    let row = crate::file_store::save_artifact_to_files(
        pool,
        owner_id,
        artifact,
        crate::file_store::SaveTarget::Export { folder_id: folder.as_deref(), name },
    )
    .await
    .ok_or_else(|| bad(format!("《{}》无法导出为文件", artifact.title)))?;
    if taken.contains(&row.name) {
        let renamed = unique_name(&taken, &row.name);
        rename_file_row(pool, owner_id, &row.id, folder.as_deref(), &renamed).await?;
        journal.lines.push(format!("导出 {}/{}", tree.folder_path(folder.as_deref()).trim_end_matches('/'), renamed));
    } else {
        journal.lines.push(format!("导出 {}/{}", tree.folder_path(folder.as_deref()).trim_end_matches('/'), row.name));
    }
    journal.inverse.push(Inverse::DeleteFile { file_id: row.id.clone() });
    let line = journal.lines.last().cloned().unwrap_or_default();
    let op_id = journal.commit(pool, owner_id, session_id).await?;
    Ok((line.trim_start_matches("导出 ").to_string(), op_id))
}

async fn rename_file_row(pool: &DbPool, owner_id: &str, file_id: &str, folder: Option<&str>, name: &str) -> AppResult<()> {
    sqlx::query("UPDATE files SET folder_id = ?, name = ?, updated_at = ? WHERE id = ? AND owner_id = ?")
        .bind(folder)
        .bind(name)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(file_id)
        .bind(owner_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn rename_folder_row(pool: &DbPool, owner_id: &str, folder_id: &str, parent: Option<&str>, name: &str) -> AppResult<()> {
    sqlx::query("UPDATE folders SET parent_id = ?, name = ?, updated_at = ? WHERE id = ? AND owner_id = ?")
        .bind(parent)
        .bind(name)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(folder_id)
        .bind(owner_id)
        .execute(pool)
        .await?;
    Ok(())
}

// ───────────────────────── 批量整理 ─────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OrganizeOp {
    /// 创建目录（逐级）
    Mkdir { path: String },
    /// 把文件或文件夹移动到目标目录（目录不存在会自动创建）
    Move { from: String, to: String },
    /// 重命名文件或文件夹
    Rename { path: String, name: String },
    /// 把文档转换为 md / txt，存到源文件旁边或 dest 目录
    Convert { path: String, to: String, #[serde(default)] dest: Option<String> },
}

/// 被操作的对象
enum Target {
    File(FileRow),
    Folder(FolderRow),
}

fn resolve_target(tree: &Tree, path: &str) -> Result<Target, String> {
    if let Some(file) = tree.find_file(path)? {
        return Ok(Target::File(file.clone()));
    }
    match tree.find_folder(path)? {
        Some(Some(id)) => Ok(Target::Folder(tree.folders[&id].clone())),
        Some(None) => Err("不能移动或重命名根目录".to_string()),
        None => Err(format!("找不到：{path}")),
    }
}

fn is_spreadsheet(name: &str) -> bool {
    matches!(name.rsplit('.').next().map(str::to_lowercase).as_deref(), Some("xlsx" | "xlsm" | "xls" | "xlsb" | "ods"))
}

fn check_name(name: &str) -> Result<(), String> {
    let parts = normalize(name)?;
    if parts.len() != 1 || name.contains('/') {
        return Err(format!("名称不能包含 /：{name}"));
    }
    Ok(())
}

/// 先校验全部操作（来源存在、名称合法、不会把目录移进自身），再依次执行
pub async fn organize(pool: &DbPool, owner_id: &str, ops: &[OrganizeOp], session_id: Option<&str>) -> AppResult<(Vec<String>, Option<String>)> {
    if ops.is_empty() {
        return Err(bad("没有任何整理操作"));
    }
    if ops.len() > MAX_ORGANIZE_OPS {
        return Err(bad(format!("一次最多 {MAX_ORGANIZE_OPS} 个操作")));
    }
    let mut tree = Tree::load(pool, owner_id).await?;

    // 1. 校验。前面的操作会创建的目录（mkdir、移动 / 转换的目标目录）后面可以直接引用
    fn plan_dirs(planned: &mut HashSet<String>, path: &str) {
        if let Ok(parts) = normalize(path) {
            for i in 1..=parts.len() {
                planned.insert(parts[..i].join("/"));
            }
        }
    }
    let mut planned: HashSet<String> = HashSet::new();
    let mut planned_snapshot: Vec<HashSet<String>> = Vec::with_capacity(ops.len());
    for op in ops {
        planned_snapshot.push(planned.clone());
        match op {
            OrganizeOp::Mkdir { path } => plan_dirs(&mut planned, path),
            OrganizeOp::Move { to, .. } => plan_dirs(&mut planned, to),
            OrganizeOp::Convert { dest: Some(d), .. } => plan_dirs(&mut planned, d),
            _ => {}
        }
    }
    let resolve_or_planned = |path: &str, planned: &HashSet<String>| -> Result<Option<Target>, String> {
        match resolve_target(&tree, path) {
            Ok(target) => Ok(Some(target)),
            Err(e) => {
                if planned.contains(&normalize(path)?.join("/")) {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    };
    for (i, op) in ops.iter().enumerate() {
        let n = i + 1;
        let planned = &planned_snapshot[i];
        let checked: Result<(), String> = match op {
            OrganizeOp::Mkdir { path } => normalize(path).map(|_| ()),
            OrganizeOp::Move { from, to } => resolve_or_planned(from, planned).and_then(|t| {
                let Some(t) = t else { return normalize(to).map(|_| ()) };
                normalize(to)?;
                if let (Target::Folder(folder), Ok(Some(Some(dest)))) = (&t, tree.find_folder(to)) {
                    if tree.is_within(Some(&dest), &folder.id) {
                        return Err(format!("不能把文件夹移动到它自己里面：{from}"));
                    }
                }
                Ok(())
            }),
            OrganizeOp::Rename { path, name } => resolve_or_planned(path, planned).and_then(|_| check_name(name)),
            OrganizeOp::Convert { path, to, dest } => {
                let is_sheet = |p: &str| tree.find_file(p).ok().flatten().is_some_and(|f| is_spreadsheet(&f.name));
                if !matches!(to.as_str(), "md" | "txt" | "csv") {
                    Err(format!("只支持转换为 md / txt / csv，不支持 {to}"))
                } else if to == "csv" && !is_sheet(path) {
                    Err(format!("只有 Excel / ODS 表格可以转换为 csv：{path}"))
                } else {
                    match tree.find_file(path) {
                        Ok(Some(_)) => dest.as_deref().map_or(Ok(()), |d| normalize(d).map(|_| ())),
                        Ok(None) => Err(format!("找不到文件：{path}")),
                        Err(e) => Err(e),
                    }
                }
            }
        };
        checked.map_err(|e| bad(format!("第 {n} 个操作无效：{e}")))?;
    }

    // 2. 执行（中途失败时，已完成的部分照常记录，可撤销）
    let mut journal = Journal::new();
    let result = run_ops(pool, owner_id, &mut tree, ops, &mut journal).await;
    let lines = journal.lines.clone();
    let op_id = journal.commit(pool, owner_id, session_id).await?;
    match result {
        Ok(()) => Ok((lines, op_id)),
        Err(e) => Err(bad(format!(
            "执行到一半失败：{e}；已完成 {} 项{}",
            lines.len(),
            op_id.map(|id| format!("，可用 op {id} 撤销")).unwrap_or_default()
        ))),
    }
}

async fn run_ops(pool: &DbPool, owner_id: &str, tree: &mut Tree, ops: &[OrganizeOp], journal: &mut Journal) -> AppResult<()> {
    for op in ops {
        match op {
            OrganizeOp::Mkdir { path } => {
                let id = ensure_folder(pool, owner_id, tree, path, journal).await?;
                journal.lines.push(format!("目录 {}", tree.folder_path(id.as_deref())));
            }
            OrganizeOp::Move { from, to } => {
                let target = resolve_target(tree, from).map_err(bad)?;
                let dest = ensure_folder(pool, owner_id, tree, to, journal).await?;
                let taken = tree.names_in(dest.as_deref());
                match target {
                    Target::File(file) => {
                        if file.folder_id == dest {
                            continue;
                        }
                        let name = unique_name(&taken, &file.name);
                        rename_file_row(pool, owner_id, &file.id, dest.as_deref(), &name).await?;
                        journal.inverse.push(Inverse::RestoreFile { file_id: file.id.clone(), folder_id: file.folder_id.clone(), name: file.name.clone() });
                        journal.lines.push(format!("{} → {}/", tree.file_path(&file), tree.folder_path(dest.as_deref()).trim_end_matches('/')));
                        if let Some(slot) = tree.files.iter_mut().find(|f| f.id == file.id) {
                            slot.folder_id = dest.clone();
                            slot.name = name;
                        }
                    }
                    Target::Folder(folder) => {
                        if folder.parent_id == dest {
                            continue;
                        }
                        if dest.as_deref().is_some_and(|d| tree.is_within(Some(d), &folder.id)) {
                            return Err(bad(format!("不能把文件夹移动到它自己里面：{from}")));
                        }
                        let name = unique_name(&taken, &folder.name);
                        let old_path = tree.folder_path(Some(&folder.id));
                        rename_folder_row(pool, owner_id, &folder.id, dest.as_deref(), &name).await?;
                        journal.inverse.push(Inverse::RestoreFolder { folder_id: folder.id.clone(), parent_id: folder.parent_id.clone(), name: folder.name.clone() });
                        journal.lines.push(format!("{old_path}/ → {}/", tree.folder_path(dest.as_deref()).trim_end_matches('/')));
                        if let Some(slot) = tree.folders.get_mut(&folder.id) {
                            slot.parent_id = dest.clone();
                            slot.name = name;
                        }
                    }
                }
            }
            OrganizeOp::Rename { path, name } => match resolve_target(tree, path).map_err(bad)? {
                Target::File(file) => {
                    let mut taken = tree.names_in(file.folder_id.as_deref());
                    taken.remove(&file.name);
                    let new_name = unique_name(&taken, name.trim());
                    rename_file_row(pool, owner_id, &file.id, file.folder_id.as_deref(), &new_name).await?;
                    journal.inverse.push(Inverse::RestoreFile { file_id: file.id.clone(), folder_id: file.folder_id.clone(), name: file.name.clone() });
                    journal.lines.push(format!("{} 重命名为 {new_name}", tree.file_path(&file)));
                    if let Some(slot) = tree.files.iter_mut().find(|f| f.id == file.id) {
                        slot.name = new_name;
                    }
                }
                Target::Folder(folder) => {
                    let mut taken = tree.names_in(folder.parent_id.as_deref());
                    taken.remove(&folder.name);
                    let new_name = unique_name(&taken, name.trim());
                    let old_path = tree.folder_path(Some(&folder.id));
                    rename_folder_row(pool, owner_id, &folder.id, folder.parent_id.as_deref(), &new_name).await?;
                    journal.inverse.push(Inverse::RestoreFolder { folder_id: folder.id.clone(), parent_id: folder.parent_id.clone(), name: folder.name.clone() });
                    journal.lines.push(format!("{old_path}/ 重命名为 {new_name}"));
                    if let Some(slot) = tree.folders.get_mut(&folder.id) {
                        slot.name = new_name;
                    }
                }
            },
            OrganizeOp::Convert { path, to, dest } => {
                let file = tree.find_file(path).map_err(bad)?.cloned().ok_or_else(|| bad(format!("找不到文件：{path}")))?;
                let folder = match dest {
                    Some(d) => ensure_folder(pool, owner_id, tree, d, journal).await?,
                    None => file.folder_id.clone(),
                };
                let stem = file.name.rsplit_once('.').map_or(file.name.as_str(), |(s, _)| s).to_string();
                // 每个输出文件：(文件名, 内容, MIME)
                let outputs: Vec<(String, String, &str)> = if to == "csv" {
                    // 表格完整读取（每个 sheet 一个 csv），不受文本提取的行数限制
                    let disk = std::path::PathBuf::from(&file.file_path);
                    let sheets = tokio::task::spawn_blocking(move || crate::data_engine::excel_to_csv(&disk))
                        .await
                        .map_err(|e| bad(e.to_string()))?
                        .map_err(bad)?;
                    let single = sheets.len() == 1;
                    sheets
                        .into_iter()
                        .map(|(sheet, csv)| {
                            let name = if single { format!("{stem}.csv") } else { format!("{stem}-{sheet}.csv") };
                            // 带 BOM，Excel 打开中文不乱码
                            (name, format!("\u{feff}{csv}"), "text/csv")
                        })
                        .collect()
                } else {
                    let text = text_of(pool, owner_id, &file).await?;
                    let mime = if to == "md" { "text/markdown" } else { "text/plain" };
                    vec![(format!("{stem}.{to}"), text, mime)]
                };
                for (name, content, mime) in outputs {
                    let name = unique_name(&tree.names_in(folder.as_deref()), &name);
                    let row = crate::file_store::save_file_bytes(
                        pool,
                        owner_id,
                        folder.as_deref(),
                        &name,
                        content.as_bytes(),
                        mime,
                        &format!("由 {} 转换", file.name),
                        json!({ "source": "agent_convert", "from_file_id": file.id }),
                    )
                    .await
                    .map_err(|e| bad(format!("转换失败：{e}")))?
                    .ok_or_else(|| bad(format!("{} 没有可提取的内容", file.name)))?;
                    journal.inverse.push(Inverse::DeleteFile { file_id: row.id.clone() });
                    journal.lines.push(format!("{} 转换为 {}/{}", tree.file_path(&file), tree.folder_path(folder.as_deref()).trim_end_matches('/'), row.name));
                    tree.files.push(row);
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct UndoReport {
    pub undone: usize,
    pub skipped: Vec<String>,
}

/// 撤销一次写入 / 整理：按逆序执行逆操作；对象已被用户改动或删除时跳过并说明
pub async fn undo(pool: &DbPool, owner_id: &str, op_id: &str) -> AppResult<UndoReport> {
    let row = sqlx::query("SELECT inverse, undone_at FROM workspace_ops WHERE id = ? AND owner_id = ?")
        .bind(op_id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("操作记录不存在".into()))?;
    if text_column(&row, 1)?.is_some() {
        return Err(bad("这次操作已经撤销过了"));
    }
    let inverse: Vec<Inverse> = serde_json::from_str(&text_column(&row, 0)?.unwrap_or_default())?;
    let mut report = UndoReport { undone: 0, skipped: Vec::new() };
    for item in inverse {
        match &item {
            Inverse::DeleteFile { file_id } => match file_repo::delete_file(pool, owner_id, file_id).await? {
                Some(file) => {
                    let _ = tokio::fs::remove_file(&file.file_path).await;
                    sqlx::query("DELETE FROM file_texts WHERE file_id = ?").bind(file_id).execute(pool).await?;
                    report.undone += 1;
                }
                None => report.skipped.push(format!("文件 {file_id} 已不存在")),
            },
            Inverse::DeleteFolder { folder_id } => {
                let used = sqlx::query("SELECT (SELECT COUNT(*) FROM files WHERE folder_id = ?) + (SELECT COUNT(*) FROM folders WHERE parent_id = ?)")
                    .bind(folder_id)
                    .bind(folder_id)
                    .fetch_one(pool)
                    .await?
                    .try_get::<i64, _>(0)?;
                if used > 0 {
                    report.skipped.push("文件夹里已有其他内容，保留".to_string());
                } else {
                    sqlx::query("DELETE FROM folders WHERE id = ? AND owner_id = ?").bind(folder_id).bind(owner_id).execute(pool).await?;
                    report.undone += 1;
                }
            }
            Inverse::RestoreFile { file_id, folder_id, name } => {
                let tree = Tree::load(pool, owner_id).await?;
                if !tree.files.iter().any(|f| &f.id == file_id) {
                    report.skipped.push(format!("文件 {name} 已不存在"));
                    continue;
                }
                // 恢复成原来的位置和名称（原目录已被删除时放回根目录）
                let target_ok = folder_id.as_ref().map_or(true, |id| tree.folders.contains_key(id));
                let folder = if target_ok { folder_id.as_deref() } else { None };
                rename_file_row(pool, owner_id, file_id, folder, name).await?;
                report.undone += 1;
            }
            Inverse::RestoreFolder { folder_id, parent_id, name } => {
                let tree = Tree::load(pool, owner_id).await?;
                if !tree.folders.contains_key(folder_id) {
                    report.skipped.push(format!("文件夹 {name} 已不存在"));
                    continue;
                }
                let parent_ok = parent_id.as_ref().map_or(true, |id| tree.folders.contains_key(id) && !tree.is_within(Some(id), folder_id));
                let parent = if parent_ok { parent_id.as_deref() } else { None };
                rename_folder_row(pool, owner_id, folder_id, parent, name).await?;
                report.undone += 1;
            }
        }
    }
    sqlx::query("UPDATE workspace_ops SET undone_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(op_id)
        .execute(pool)
        .await?;
    Ok(report)
}

/// 系统提示词里的工作区概览：顶层目录、文件总数、最近修改的文件
pub async fn overview(pool: &DbPool, owner_id: &str) -> AppResult<String> {
    let tree = Tree::load(pool, owner_id).await?;
    if tree.files.is_empty() && tree.folders.is_empty() {
        return Ok(String::new());
    }
    let mut top: Vec<(String, usize)> = tree
        .folders
        .values()
        .filter(|f| f.parent_id.is_none())
        .map(|f| {
            let scope = tree.descendants(Some(&f.id));
            let count = tree.files.iter().filter(|file| scope.contains(&file.folder_id)).count();
            (f.name.clone(), count)
        })
        .collect();
    top.sort();
    let root_files = tree.files.iter().filter(|f| f.folder_id.is_none()).count();
    let mut out = format!(
        "## 用户工作区（共 {} 个文件）\n可用 workspace_list / workspace_search / workspace_read 查看，workspace_write / workspace_organize 写入和整理（可撤销）。\n顶层：",
        tree.files.len()
    );
    let mut items: Vec<String> = top.iter().take(15).map(|(name, n)| format!("/{name}/（{n}）")).collect();
    if root_files > 0 {
        items.push(format!("根目录文件 {root_files} 个"));
    }
    out.push_str(&items.join("、"));
    let mut recent: Vec<&FileRow> = tree.files.iter().collect();
    recent.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out.push_str("\n最近修改：");
    out.push_str(&recent.iter().take(8).map(|f| tree.file_path(f)).collect::<Vec<_>>().join("、"));
    Ok(out)
}

/// 用户在输入框 @ 引用的文件
pub fn referenced_files_note(tree: &Tree, file_ids: &[String]) -> String {
    let lines: Vec<String> = file_ids
        .iter()
        .filter_map(|id| tree.files.iter().find(|f| &f.id == id))
        .map(|f| format!("- {}（id={}，{} 字节）", tree.file_path(f), f.id, f.file_size))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!("## 用户本轮引用的工作区文件\n需要时用 workspace_read 读取内容：\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, name: &str, parent: Option<&str>) -> FolderRow {
        FolderRow { id: id.into(), owner_id: "u".into(), name: name.into(), parent_id: parent.map(str::to_string), created_at: "t".into(), updated_at: "t".into() }
    }

    fn file(id: &str, name: &str, folder: Option<&str>, updated: &str) -> FileRow {
        FileRow {
            id: id.into(),
            owner_id: "u".into(),
            name: name.into(),
            file_path: format!("/tmp/{id}"),
            file_type: "doc".into(),
            file_size: 10,
            folder_id: folder.map(str::to_string),
            description: None,
            metadata: None,
            created_at: updated.into(),
            updated_at: updated.into(),
        }
    }

    fn tree() -> Tree {
        Tree::from_parts(
            vec![folder("a", "项目A", None), folder("b", "合同", Some("a")), folder("c", "归档", None)],
            vec![
                file("f1", "报价单.docx", Some("b"), "2026-01-02"),
                file("f2", "报价单.docx", Some("b"), "2026-01-05"),
                file("f3", "readme.md", None, "2026-01-01"),
                file("f4", "预算.xlsx", Some("a"), "2026-01-03"),
            ],
        )
    }

    #[test]
    fn normalize_rejects_traversal_and_cleans_slashes() {
        assert_eq!(normalize("//项目A/./合同/").unwrap(), vec!["项目A", "合同"]);
        assert!(normalize("/a/../b").is_err());
        assert!(normalize("a\\b").is_err());
        assert!(normalize("/").unwrap().is_empty());
    }

    #[test]
    fn paths_resolve_both_ways() {
        let t = tree();
        assert_eq!(t.find_folder("/项目A/合同").unwrap(), Some(Some("b".to_string())));
        assert_eq!(t.find_folder("/").unwrap(), Some(None));
        assert_eq!(t.find_folder("/不存在").unwrap(), None);
        // 同名文件取最近修改的
        assert_eq!(t.find_file("/项目A/合同/报价单.docx").unwrap().unwrap().id, "f2");
        assert_eq!(t.find_file("f1").unwrap().unwrap().id, "f1");
        assert_eq!(t.file_path(t.find_file("f4").unwrap().unwrap()), "/项目A/预算.xlsx");
        assert_eq!(t.file_path(t.find_file("f3").unwrap().unwrap()), "/readme.md");
        assert!(t.is_within(Some("b"), "a"));
        assert!(!t.is_within(Some("c"), "a"));
    }

    #[test]
    fn unique_names_and_glob() {
        let taken: HashSet<String> = ["报价单.docx", "报价单 (2).docx", "README"].iter().map(|s| s.to_string()).collect();
        assert_eq!(unique_name(&taken, "报价单.docx"), "报价单 (3).docx");
        assert_eq!(unique_name(&taken, "README"), "README (2)");
        assert_eq!(unique_name(&taken, "新文件.md"), "新文件.md");
        assert!(glob_match("*.DOCX", "报价单.docx"));
        assert!(glob_match("预算?.xlsx", "预算1.xlsx"));
        assert!(!glob_match("*.md", "a.docx"));
    }

    #[test]
    fn list_recursive_with_pattern() {
        let t = tree();
        let (entries, total) = list(&t, "/项目A", true, Some("*.docx"), 10).unwrap();
        assert_eq!(total, 2);
        assert!(entries.iter().all(|e| e.path.starts_with("/项目A/合同/")));
        let (root, _) = list(&t, "/", false, None, 10).unwrap();
        let paths: Vec<_> = root.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["/归档/", "/项目A/", "/readme.md"]);
        assert!(list(&t, "/不存在", false, None, 10).is_err());
    }

    #[test]
    fn slices_and_snippets() {
        let (piece, total, next) = slice_chars("一二三四五", 1, 2);
        assert_eq!((piece.as_str(), total, next), ("二三", 5, Some(3)));
        let (_, _, next) = slice_chars("一二三", 1, 5);
        assert_eq!(next, None);
        let s = snippet("前文 很长的内容 关键词 在这里 后文", &["关键词".to_string()]).unwrap();
        assert!(s.contains("关键词"));
        assert!(snippet("没有", &["x".to_string()]).is_none());
    }

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

    async fn insert_file(pool: &DbPool, id: &str, name: &str, folder: Option<&str>) {
        sqlx::query("INSERT INTO files (id, owner_id, name, file_path, file_type, file_size, folder_id, created_at, updated_at) VALUES (?, 'u1', ?, ?, 'doc', 1, ?, 't', 't')")
            .bind(id)
            .bind(name)
            .bind(format!("/tmp/walioffice-test-{id}"))
            .bind(folder)
            .execute(pool)
            .await
            .unwrap();
    }

    fn paths(tree: &Tree) -> Vec<String> {
        let mut out: Vec<String> = tree.files().iter().map(|f| tree.file_path(f)).collect();
        out.sort();
        out
    }

    #[tokio::test]
    async fn organize_then_undo_restores_original_layout() {
        let pool = memory_pool().await;
        insert_file(&pool, "f1", "甲方合同.docx", None).await;
        insert_file(&pool, "f2", "会议纪要.md", None).await;
        insert_file(&pool, "f3", "甲方合同.docx", None).await; // 同名，用于触发自动编号
        let before = paths(&Tree::load(&pool, "u1").await.unwrap());

        let ops: Vec<OrganizeOp> = serde_json::from_value(json!([
            {"op": "move", "from": "f1", "to": "/项目A/合同"},
            {"op": "move", "from": "f3", "to": "/项目A/合同"},
            {"op": "rename", "path": "/会议纪要.md", "name": "2026-01 周会纪要.md"},
            {"op": "mkdir", "path": "/归档"},
            {"op": "move", "from": "/项目A", "to": "/归档"},
        ]))
        .unwrap();
        let (lines, op_id) = organize(&pool, "u1", &ops, Some("s1")).await.unwrap();
        assert_eq!(lines.len(), 5, "{lines:?}");
        let after = paths(&Tree::load(&pool, "u1").await.unwrap());
        assert_eq!(after, ["/2026-01 周会纪要.md", "/归档/项目A/合同/甲方合同 (2).docx", "/归档/项目A/合同/甲方合同.docx"]);

        // 非法操作整体拒绝，什么都不执行
        let bad_ops: Vec<OrganizeOp> = serde_json::from_value(json!([
            {"op": "mkdir", "path": "/新目录"},
            {"op": "move", "from": "/不存在.docx", "to": "/x"}
        ]))
        .unwrap();
        assert!(organize(&pool, "u1", &bad_ops, None).await.is_err());
        assert!(Tree::load(&pool, "u1").await.unwrap().find_folder("/新目录").unwrap().is_none());
        // 不能把目录移进自身
        let cycle: Vec<OrganizeOp> = serde_json::from_value(json!([{"op": "move", "from": "/归档", "to": "/归档/项目A"}])).unwrap();
        assert!(organize(&pool, "u1", &cycle, None).await.is_err());

        let report = undo(&pool, "u1", &op_id.unwrap()).await.unwrap();
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let restored = Tree::load(&pool, "u1").await.unwrap();
        assert_eq!(paths(&restored), before);
        assert!(restored.folders.is_empty(), "新建的空目录应被删除");
    }

    #[tokio::test]
    async fn convert_excel_to_csv_per_sheet_and_undo() {
        let pool = memory_pool().await;
        let xlsx = std::env::temp_dir().join(format!("walioffice-ws-{}.xlsx", uuid::Uuid::new_v4()));
        let mut wb = rust_xlsxwriter::Workbook::new();
        let s1 = wb.add_worksheet().set_name("一月").unwrap();
        s1.write(0, 0, "产品").unwrap();
        s1.write(1, 0, "A").unwrap();
        wb.add_worksheet().set_name("二月").unwrap().write(0, 0, "产品").unwrap();
        wb.save(&xlsx).unwrap();
        sqlx::query("INSERT INTO files (id, owner_id, name, file_path, file_type, file_size, created_at, updated_at) VALUES ('x1', 'u1', '销售.xlsx', ?, 'excel', 1, 't', 't')")
            .bind(xlsx.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();

        let ops: Vec<OrganizeOp> = serde_json::from_value(json!([{"op": "convert", "path": "/销售.xlsx", "to": "csv", "dest": "/CSV"}])).unwrap();
        let (lines, op_id) = organize(&pool, "u1", &ops, None).await.unwrap();
        assert_eq!(lines.len(), 2);
        let tree = Tree::load(&pool, "u1").await.unwrap();
        assert_eq!(paths(&tree), ["/CSV/销售-一月.csv", "/CSV/销售-二月.csv", "/销售.xlsx"]);
        let csv = tree.find_file("/CSV/销售-一月.csv").unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&csv.file_path).unwrap(), "\u{feff}产品\nA\n");

        // 只有表格能转 csv
        let bad_ops: Vec<OrganizeOp> = serde_json::from_value(json!([{"op": "convert", "path": "/CSV/销售-一月.csv", "to": "csv"}])).unwrap();
        assert!(organize(&pool, "u1", &bad_ops, None).await.is_err());

        undo(&pool, "u1", &op_id.unwrap()).await.unwrap();
        assert_eq!(paths(&Tree::load(&pool, "u1").await.unwrap()), ["/销售.xlsx"]);
        let _ = std::fs::remove_file(&xlsx);
    }

    #[tokio::test]
    async fn write_text_numbers_duplicates_and_undo_deletes() {
        let pool = memory_pool().await;
        let (p1, op1) = write_text(&pool, "u1", "/笔记/想法.md", "# 想法", None).await.unwrap();
        let (p2, _) = write_text(&pool, "u1", "/笔记/想法.md", "# 想法2", None).await.unwrap();
        assert_eq!(p1, "/笔记/想法.md");
        assert_eq!(p2, "/笔记/想法 (2).md");
        assert!(write_text(&pool, "u1", "/a.exe", "x", None).await.is_err());
        undo(&pool, "u1", op1.as_deref().unwrap()).await.unwrap();
        let tree = Tree::load(&pool, "u1").await.unwrap();
        assert_eq!(paths(&tree), ["/笔记/想法 (2).md"]);
        assert!(undo(&pool, "u1", op1.as_deref().unwrap()).await.is_err(), "不能重复撤销");
        for f in tree.files() {
            let _ = std::fs::remove_file(&f.file_path);
        }
    }
}
