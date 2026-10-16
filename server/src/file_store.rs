//! 文件存储：上传附件、Agent 产物落盘到用户的文件（工作区）

use base64::Engine;
use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use crate::db::file_repo::{self, FileRow};
use crate::db::{project_repo, DbPool};
use crate::models::{Artifact, ChatAttachment, PptProject};
use crate::render;

pub(crate) fn user_file_dir(owner_id: &str) -> PathBuf {
    PathBuf::from(&crate::config::config().data_dir)
        .join("files")
        .join(owner_id)
}

pub(crate) fn sanitize_filename(name: &str, fallback: &str) -> String {
    let cleaned = name
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect::<String>()
        .trim()
        .to_string();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

fn ensure_extension(name: &str, extension: &str) -> String {
    if name.to_lowercase().ends_with(&extension.to_lowercase()) {
        name.to_string()
    } else {
        format!("{name}{extension}")
    }
}

pub(crate) fn infer_file_type(name: &str, mime_type: &str) -> String {
    let extension = FsPath::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();
    match extension.as_str() {
        "ppt" | "pptx" => "ppt".into(),
        "doc" | "docx" | "md" | "markdown" | "txt" | "pdf" => "doc".into(),
        "xls" | "xlsx" | "csv" | "tsv" => "excel".into(),
        "drawio" | "xml" => "drawio".into(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" => "image".into(),
        "mp4" | "webm" | "mov" => "video".into(),
        _ if mime_type.starts_with("image/") => "image".into(),
        _ if mime_type.starts_with("video/") => "video".into(),
        _ if mime_type.contains("spreadsheet") || mime_type.contains("excel") => "excel".into(),
        _ if mime_type.contains("presentation") => "ppt".into(),
        _ if mime_type.contains("pdf")
            || mime_type.contains("word")
            || mime_type.starts_with("text/") =>
        {
            "doc".into()
        }
        _ => "other".into(),
    }
}

pub(crate) async fn save_file_bytes(
    pool: &DbPool,
    owner_id: &str,
    folder_id: Option<&str>,
    name: &str,
    data: &[u8],
    mime_type: &str,
    description: &str,
    metadata: serde_json::Value,
) -> anyhow::Result<Option<FileRow>> {
    if data.is_empty() {
        return Ok(None);
    }
    let safe_name = sanitize_filename(name, "walioffice-file.bin");
    let extension = FsPath::new(&safe_name)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let storage_name = format!("{}{}", uuid::Uuid::new_v4(), extension);
    let storage_dir = user_file_dir(owner_id);
    tokio::fs::create_dir_all(&storage_dir).await?;
    let storage_path = storage_dir.join(storage_name);
    tokio::fs::write(&storage_path, data).await?;
    let file_type = infer_file_type(&safe_name, mime_type);
    let row = file_repo::create_file(
        pool,
        owner_id,
        &safe_name,
        &storage_path.to_string_lossy(),
        &file_type,
        data.len() as i64,
        folder_id,
        Some(description),
        Some(metadata),
    ).await?;
    Ok(Some(row))
}

pub(crate) fn decode_data_url(value: &str) -> Option<(String, Vec<u8>)> {
    let (header, payload) = value.split_once(',')?;
    if !header.starts_with("data:") || !header.contains(";base64") {
        return None;
    }
    let mime_type = header
        .trim_start_matches("data:")
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .to_string();
    let data = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    Some((mime_type, data))
}

fn mime_extension(mime_type: &str, fallback: &str) -> String {
    match mime_type {
        "image/png" => ".png".into(),
        "image/jpeg" | "image/jpg" => ".jpg".into(),
        "image/webp" => ".webp".into(),
        "image/gif" => ".gif".into(),
        "video/mp4" => ".mp4".into(),
        "video/webm" => ".webm".into(),
        _ => fallback.into(),
    }
}

async fn download_remote_file(
    url: &str,
    expected_prefix: &str,
) -> anyhow::Result<(String, Vec<u8>)> {
    let response = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await?
        .error_for_status()?;
    let mime_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| value.starts_with(expected_prefix))
        .ok_or_else(|| anyhow::anyhow!("远程文件类型不匹配"))?
        .to_string();
    let data = response.bytes().await?.to_vec();
    Ok((mime_type, data))
}

pub(crate) async fn save_chat_attachments_to_files(
    pool: &DbPool,
    owner_id: &str,
    attachments: &[ChatAttachment],
) {
    for attachment in attachments {
        let description = format!("聊天上传附件：{}", attachment.name);
        if let Some(data_url) = attachment.data_url.as_deref() {
            if let Some((mime_type, data)) = decode_data_url(data_url) {
                let filename = ensure_extension(
                    &sanitize_filename(&attachment.name, "chat-image"),
                    &mime_extension(&mime_type, ".bin"),
                );
                let _ = save_file_bytes(pool, owner_id, None,
                    &filename,
                    &data,
                    &mime_type,
                    &description,
                    serde_json::json!({
                        "source": "chat_attachment",
                        "original_name": attachment.name,
                        "mime_type": mime_type,
                    }),
                )
                .await;
                continue;
            }
        }

        if let Some(text) = attachment.text_content.as_deref() {
            if !text.trim().is_empty() {
                let lower_name = attachment.name.to_lowercase();
                let filename = if lower_name.ends_with(".md")
                    || lower_name.ends_with(".markdown")
                    || lower_name.ends_with(".txt")
                    || lower_name.ends_with(".csv")
                    || lower_name.ends_with(".json")
                    || lower_name.ends_with(".tsv")
                {
                    sanitize_filename(&attachment.name, "chat-attachment.txt")
                } else {
                    ensure_extension(
                        &format!(
                            "{}-文本副本",
                            sanitize_filename(&attachment.name, "chat-attachment")
                        ),
                        ".txt",
                    )
                };
                let _ = save_file_bytes(pool, owner_id, None,
                    &filename,
                    text.as_bytes(),
                    attachment.mime_type.as_str(),
                    &description,
                    serde_json::json!({
                        "source": "chat_attachment_text_copy",
                        "original_name": attachment.name,
                        "mime_type": attachment.mime_type,
                    }),
                )
                .await;
            }
        }
    }
}


/// 产物渲染出的文件
pub(crate) struct RenderedFile {
    pub name: String,
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// 产物保存到哪里
pub(crate) enum SaveTarget<'a> {
    /// 自动保存到根目录：同一产物只保留一个文件，新版本覆盖旧版本
    Generated,
    /// 导出到指定文件夹：总是新建文件，可指定文件名（扩展名以产物格式为准）
    Export {
        folder_id: Option<&'a str>,
        name: Option<&'a str>,
    },
}

fn artifact_filename(artifact: &Artifact, fallback: &str, extension: &str) -> String {
    ensure_extension(&sanitize_filename(&artifact.title, fallback), extension)
}

async fn render_with(path_name: &str, render: impl FnOnce(&std::path::Path) -> anyhow::Result<()>) -> Option<Vec<u8>> {
    let path = render::output_path(path_name);
    render(&path).ok()?;
    tokio::fs::read(&path).await.ok()
}

async fn media_bytes(url: &str, expected_prefix: &str) -> Option<(String, Vec<u8>)> {
    if let Some(decoded) = decode_data_url(url) {
        return Some(decoded);
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return download_remote_file(url, expected_prefix).await.ok();
    }
    None
}

/// 把产物渲染成可下载的文件（docx / xlsx / pptx / md / drawio / 图片 / 视频 / json）
pub(crate) async fn render_artifact_file(owner_id: &str, artifact: &Artifact) -> Option<RenderedFile> {
    const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
    const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
    const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
    let c = &artifact.content;
    let markdown_file = |fallback: &str| {
        c.get("markdown").and_then(|v| v.as_str()).map(|markdown| RenderedFile {
            name: artifact_filename(artifact, fallback, ".md"),
            bytes: markdown.as_bytes().to_vec(),
            mime: "text/markdown".into(),
        })
    };
    match artifact.kind.as_str() {
        "document" => {
            let sections = serde_json::from_value::<Vec<render::docx_render::DocSection>>(
                c.get("sections").cloned().unwrap_or(serde_json::json!([])),
            );
            if let Some(sections) = sections.ok().filter(|s| !s.is_empty()) {
                let data = render::docx_render::DocData { title: artifact.title.clone(), sections };
                let name = artifact_filename(artifact, "document", ".docx");
                if let Some(bytes) = render_with(&name, |p| render::docx_render::render_docx(&data, p).map_err(Into::into)).await {
                    return Some(RenderedFile { name, bytes, mime: DOCX.into() });
                }
            }
            markdown_file("document")
        }
        "markdown" => markdown_file("document"),
        "sheet" => {
            let tables = serde_json::from_value::<Vec<render::xlsx_render::SheetTable>>(
                c.get("tables").cloned().unwrap_or(serde_json::json!([])),
            )
            .ok()?;
            let data = render::xlsx_render::SheetData { title: artifact.title.clone(), tables };
            let name = artifact_filename(artifact, "spreadsheet", ".xlsx");
            let bytes = render_with(&name, |p| render::xlsx_render::render_xlsx(&data, p).map_err(Into::into)).await?;
            Some(RenderedFile { name, bytes, mime: XLSX.into() })
        }
        "drawio" => c.get("xml").and_then(|v| v.as_str()).map(|xml| RenderedFile {
            name: artifact_filename(artifact, "diagram", ".drawio"),
            bytes: xml.as_bytes().to_vec(),
            mime: "application/xml".into(),
        }),
        "ppt" => {
            let project_id = c.get("project_id").and_then(|v| v.as_str());
            let project = project_id
                .and_then(|id| project_repo::load_ppt_project(id).ok().flatten())
                .or_else(|| {
                    let slides = serde_json::from_value(c.get("slides")?.clone()).ok()?;
                    Some(PptProject {
                        id: project_id.unwrap_or(&artifact.id).to_string(),
                        title: artifact.title.clone(),
                        theme: c.get("theme").and_then(|v| v.as_str()).unwrap_or("default").to_string(),
                        slides,
                        history: None,
                        layout: "16x9".to_string(),
                        created_at: artifact.created_at.clone(),
                        updated_at: artifact.updated_at.clone(),
                        owner_id: owner_id.to_string(),
                    })
                })?;
            let name = artifact_filename(artifact, "presentation", ".pptx");
            let bytes = render_with(&name, |p| render::pptx_render::render_pptx(&project, p).map_err(Into::into)).await?;
            Some(RenderedFile { name, bytes, mime: PPTX.into() })
        }
        "image" => {
            let url = c.get("images").and_then(|v| v.as_array()).and_then(|items| items.first()).and_then(|v| v.as_str())?;
            let (mime, bytes) = media_bytes(url, "image/").await?;
            Some(RenderedFile { name: artifact_filename(artifact, "image", &mime_extension(&mime, ".png")), bytes, mime })
        }
        "video" => {
            let url = c.get("video_url").and_then(|v| v.as_str())?;
            let (mime, bytes) = media_bytes(url, "video/").await?;
            Some(RenderedFile { name: artifact_filename(artifact, "video", &mime_extension(&mime, ".mp4")), bytes, mime })
        }
        _ => Some(RenderedFile {
            name: artifact_filename(artifact, "artifact", ".json"),
            bytes: serde_json::to_vec_pretty(artifact).ok()?,
            mime: "application/json".into(),
        }),
    }
}

/// 产物落盘并登记到用户文件
pub(crate) async fn save_artifact_to_files(
    pool: &DbPool,
    owner_id: &str,
    artifact: &Artifact,
    target: SaveTarget<'_>,
) -> Option<FileRow> {
    let rendered = render_artifact_file(owner_id, artifact).await?;
    let extension = FsPath::new(&rendered.name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let (folder_id, name, source) = match target {
        SaveTarget::Generated => (None, rendered.name, "generated_artifact"),
        SaveTarget::Export { folder_id, name } => (
            folder_id,
            name.map(|n| ensure_extension(&sanitize_filename(n, &rendered.name), &extension))
                .unwrap_or(rendered.name),
            "export",
        ),
    };
    tracing::info!("保存产物到文件: kind={}, title={}, artifact_id={}, v{}", artifact.kind, artifact.title, artifact.id, artifact.version);
    let metadata = serde_json::json!({
        "source": source,
        "artifact_id": artifact.id,
        "artifact_kind": artifact.kind,
        "artifact_version": artifact.version,
        "tool_kind": artifact.tool_kind,
    });
    save_file_bytes(
        pool,
        owner_id,
        folder_id,
        &name,
        &rendered.bytes,
        &rendered.mime,
        &format!("智能助手生成：{}", artifact.title),
        metadata,
    )
    .await
    .map_err(|e| tracing::warn!("保存产物文件失败: {e}"))
    .ok()
    .flatten()
}
