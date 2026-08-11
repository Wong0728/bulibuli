use crate::error::AppError;
use crate::state::SharedState;
use axum::{
    extract::Path,
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde_json::json;
use std::path::PathBuf;
use tracing::{info, warn};

/// 封面路由：本地优先，未命中则下载并落盘。
///
/// 路由：`GET /api/cover/{bvid}`
///
/// 查找顺序：
/// 1. 查 `history.cover_local_path` → 文件存在 → 200 返回字节流。
/// 2. 在 `downloads/{uid}/` 与 `downloads/` 下扫 `{bvid}_cover.*` → 命中 → 200。
/// 3. 本地都没有 → 调 `DownloadManager::ensure_cover_local` 下载并落盘 → 200。
/// 4. 下载失败 → 404 `{"state": "missing"}`。
pub fn router() -> Router<SharedState> {
    Router::new().route("/api/cover/{bvid}", get(get_cover))
}

async fn get_cover(
    State(state): State<SharedState>,
    Path(bvid): Path<String>,
) -> Result<Response, AppError> {
    let bvid = bvid.trim();
    if bvid.is_empty() {
        return Ok(missing_response("bvid 为空"));
    }

    // 1. 查 history.cover_local_path
    let uid_opt: Option<String> = match state.business.history_service.find_by_bvid(bvid).await {
        Ok(Some(h)) => {
            if let Some(p) = h.cover_local_path.as_deref() {
                let path = PathBuf::from(p);
                // 防御：DB 字段可能被污染，路径必须位于下载根目录内，否则忽略并重新拉取
                let within_root = crate::services::file_safety::ensure_within_root(
                    &state.infra.paths.download_dir,
                    &path,
                );
                match within_root {
                    Ok(()) => {
                        if path.exists() {
                            return Ok(serve_image(&path).await);
                        }
                    }
                    Err(_) => {
                        warn!(
                            "[cover] {bvid} 本地封面路径超出下载根目录，已忽略: {}",
                            path.display()
                        );
                    }
                }
            }
            h.uid
        }
        _ => None,
    };

    // 2 & 3. 本地扫描 + 下载兜底，统一走 ensure_cover_local
    match state
        .media
        .download_manager
        .ensure_cover_local(bvid, uid_opt.as_deref())
        .await
    {
        Ok(Some(path)) => Ok(serve_image(&path).await),
        Ok(None) => Ok(missing_response("封面不存在且无法下载")),
        Err(e) => {
            warn!("[cover] {bvid} 下载封面失败: {e}");
            Ok(missing_response("封面下载失败"))
        }
    }
}

/// 读取本地图片文件并返回字节流，带 Content-Type 与缓存头。
async fn serve_image(path: &std::path::Path) -> Response {
    match tokio::fs::File::open(path).await {
        Ok(file) => {
            let content_type = guess_content_type(path);
            let size = file
                .metadata()
                .await
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            info!(
                "[cover] 命中本地封面: {} ({}, {} bytes)",
                path.display(),
                content_type,
                size
            );
            let stream = tokio_util::io::ReaderStream::new(file);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CACHE_CONTROL, "public, max-age=86400")
                .body(axum::body::Body::from_stream(stream))
                .unwrap_or_else(|e| {
                    warn!("[cover] 构建图片响应失败: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                })
        }
        Err(e) => {
            warn!("[cover] 读取本地封面失败 {}: {e}", path.display());
            missing_response("读取封面文件失败")
        }
    }
}

/// 404 响应：前端可据此显示占位图。
fn missing_response(reason: &str) -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(
            json!({ "success": false, "state": "missing", "message": reason }).to_string(),
        ))
        .unwrap_or_else(|e| {
            warn!("[cover] 构建缺失响应失败: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })
}

/// 根据扩展名猜测 Content-Type。
fn guess_content_type(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "image/jpeg",
    }
}
