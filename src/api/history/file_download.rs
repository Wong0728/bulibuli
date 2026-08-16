//! 抽屉“服务器到本机”：把服务器上已下载的视频产物流式发送给浏览器（Content-Disposition 附件）。
//!
//! 安全约束：
//! - 总开关 `board.browser_download_enabled` 关闭时返回 403。
//! - `path` 不直接拼盘符：必须精确命中 `scan_files` 为该 bvid 扫描出的产物路径，
//!   再经 `resolve_download_relative_path` 解析到下载目录内，防止路径穿越。

use crate::error::AppError;
use crate::state::SharedState;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::response::Response;
use futures::Stream;
use serde::Deserialize;
use tokio::io::AsyncReadExt;

const FILE_DOWNLOAD_CHUNK: usize = 256 * 1024;

#[derive(Deserialize)]
pub(super) struct FileDownloadQuery {
    bvid: String,
    path: String,
    history_id: Option<i32>,
}

/// 按扩展名推断 Content-Type，未知扩展名统一 octet-stream。
fn content_type_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4a" => "video/mp4",
        "mkv" => "video/x-matroska",
        "flv" => "video/x-flv",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ec3" => "audio/eac3",
        "wav" => "audio/wav",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "json" => "application/json",
        "xml" => "application/xml",
        "ass" | "srt" | "vtt" => "text/plain; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn attachment_disposition(filename: &str) -> String {
    if filename.is_ascii() {
        format!("attachment; filename=\"{filename}\"")
    } else {
        format!(
            "attachment; filename*=UTF-8''{}",
            urlencoding::encode(filename)
        )
    }
}

/// 读取产物限制在下载目录内，且必须命中 scan_files 的结果。
async fn resolve_scanned_file(
    state: &SharedState,
    bvid: &str,
    requested_path: &str,
    history_id: Option<i32>,
) -> Option<crate::services::history::FileEntry> {
    let history_service = &state.business.history_service;
    // DB 错误与“记录不存在”分开：失败时记日志，对用户表现为未找到（此处返回
    // Option 由调用方转 404，无法表达 500；日志足以支撑排障）。
    let history = match history_id {
        Some(id) => match history_service.find_by_id(id).await {
            Ok(value) => value.filter(|history| history.bvid == bvid),
            Err(error) => {
                tracing::warn!(%error, "按 id 查询历史记录失败");
                None
            }
        },
        None => match history_service.find_by_bvid(bvid).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "按 bvid 查询历史记录失败");
                None
            }
        },
    }?;
    let files = history_service
        .scan_files(bvid, history.uid.as_deref(), history.file_path.as_deref())
        .await;
    files
        .into_iter()
        .find(|file| file.path == requested_path)
        .filter(|file| {
            history_service
                .resolve_download_relative_path(&file.path)
                .is_some()
        })
}

struct FileReadState {
    file: Option<tokio::fs::File>,
}

/// 分块读取本地文件；读失败时发出错误项并终止流，避免截断文件被当作完整下载。
fn chunked_file_stream(
    state: FileReadState,
) -> impl Stream<Item = Result<axum::body::Bytes, AppError>> {
    futures::stream::unfold(state, |mut st| async move {
        let file = st.file.as_mut()?;
        let mut buf = vec![0u8; FILE_DOWNLOAD_CHUNK];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(axum::body::Bytes::from(buf)), st))
            }
            Err(error) => {
                st.file = None;
                Some((Err(AppError::Io(error)), st))
            }
        }
    })
}

pub(super) async fn download_history_file(
    State(state): State<SharedState>,
    Query(q): Query<FileDownloadQuery>,
) -> Result<Response, AppError> {
    use axum::http::{header, StatusCode};

    if !state
        .infra
        .settings_service
        .current()
        .board
        .browser_download_enabled
    {
        return Err(AppError::Forbidden(
            "浏览器下载已在设置中关闭（看板显示 → 抽屉浏览器下载）".to_string(),
        ));
    }
    let bvid = q.bvid.trim();
    let requested_path = q.path.trim();
    if bvid.is_empty() || requested_path.is_empty() {
        return Err(AppError::BadRequest(
            "请提供视频 BV 号和文件路径".to_string(),
        ));
    }

    let entry = resolve_scanned_file(&state, bvid, requested_path, q.history_id)
        .await
        .ok_or_else(|| AppError::NotFound("未找到该视频的指定产物文件".to_string()))?;
    let path = state
        .business
        .history_service
        .resolve_download_relative_path(&entry.path)
        .ok_or_else(|| AppError::NotFound("文件不在下载目录内".to_string()))?;
    let metadata = tokio::fs::metadata(&path).await.map_err(AppError::from)?;

    let file = tokio::fs::File::open(&path).await.map_err(AppError::from)?;
    let stream = chunked_file_stream(FileReadState { file: Some(file) });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type_for(&path))
        .header(
            header::CONTENT_DISPOSITION,
            attachment_disposition(&entry.name),
        )
        .header(header::CONTENT_LENGTH, metadata.len())
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .map_err(|e| AppError::Internal(format!("构建下载响应失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_escapes_non_ascii() {
        assert_eq!(
            attachment_disposition("video.mp4"),
            "attachment; filename=\"video.mp4\""
        );
        assert!(attachment_disposition("视频.mp4").contains("filename*=UTF-8''"));
    }
}
