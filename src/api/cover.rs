use crate::error::AppError;
use crate::state::SharedState;
use axum::{
    extract::Path,
    extract::Query,
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
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

#[derive(Deserialize, Default)]
struct CoverQuery {
    history_id: Option<i32>,
}

/// bvid 格式校验：与 `services/download.rs` 的私有 `is_valid_bvid` 同规则
/// （该函数未导出且所属模块正被并行修改，这里镜像一份保持单一语义）：
/// BV + 10 位 base58 字符（去 0/O/I/l）。在进入 DB 查询、目录扫描和
/// 封面落盘前先行拦截，防止恶意 bvid 构造文件名注入或路径穿越。
fn is_valid_bvid(bvid: &str) -> bool {
    bvid.len() == 12
        && bvid.starts_with("BV")
        && bvid[2..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() && !matches!(c, '0' | 'O' | 'I' | 'l'))
}

async fn get_cover(
    State(state): State<SharedState>,
    Path(bvid): Path<String>,
    Query(query): Query<CoverQuery>,
) -> Result<Response, AppError> {
    let bvid = bvid.trim();
    if bvid.is_empty() {
        return Ok(missing_response("bvid 为空"));
    }
    // 落盘/扫描前的入口校验：非法 bvid 直接 404，不进入后续查询与下载链路。
    if !is_valid_bvid(bvid) {
        return Ok(missing_response("bvid 格式无效"));
    }

    // 1. 查 history.cover_local_path
    let history = match query.history_id {
        Some(id) => state
            .business
            .history_service
            .find_by_id(id)
            .await?
            .filter(|history| history.bvid == bvid),
        None => state.business.history_service.find_by_bvid(bvid).await?,
    };
    let uid_opt: Option<String> = match history {
        Some(h) => {
            if let Some(p) = h.cover_local_path.as_deref() {
                let path = PathBuf::from(p);
                // 防御：DB 字段可能被污染，路径必须位于下载根目录内，否则忽略并重新拉取
                let within_root = crate::services::file_safety::ensure_within_root(
                    &state.infra.paths.download_dir,
                    &path,
                );
                match within_root {
                    Ok(()) => {
                        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
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
        None => None,
    };

    // 2 & 3. 本地扫描 + 下载兜底，统一走 ensure_cover_local。
    // 记录尚无封面（多为手动下载进行中）时优先用下载任务的实际目录：
    // 手动任务的目录是 manual/{标题}，按 uid/日期推导会下错位置，
    // 完成路径再下一份造成同一封面两存。
    let task_dir = state
        .business
        .history_service
        .download_tasks_for_bvids(&[bvid.to_string()])
        .await?
        .iter()
        .filter_map(|t| t.download_dir.as_deref())
        .find_map(|dir| {
            let path = PathBuf::from(dir);
            path.starts_with(&state.infra.paths.download_dir)
                .then_some(path)
        });
    let cover_dir = match (&task_dir, &uid_opt) {
        (Some(dir), _) => Some(dir.clone()),
        (None, Some(uid)) => Some(state.infra.paths.download_dir.join(uid)),
        (None, None) => None,
    };
    match state
        .media
        .download_manager
        .ensure_cover_local_in(bvid, uid_opt.as_deref(), cover_dir.as_deref())
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
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CACHE_CONTROL, "public, max-age=86400");
            // 已读取 metadata：直接设置 Content-Length，客户端可显示进度/复用连接
            //（此前读了长度却仍发 chunked，字段仅用于日志）。
            if size > 0 {
                builder = builder.header(header::CONTENT_LENGTH, size);
            }
            builder
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bvid_validation_rejects_path_like_and_ambiguous_values() {
        // 与 services/download.rs 的 is_valid_bvid 单测同口径。
        assert!(is_valid_bvid("BV1xx411c7mD"));
        assert!(!is_valid_bvid("../etc/passwd"));
        assert!(!is_valid_bvid("BV1xx411c7m0D"));
        assert!(!is_valid_bvid("bv1xx411c7mD"));
        assert!(!is_valid_bvid("BV1xx411c7mD\"); rm -rf /"));
        assert!(!is_valid_bvid("BV1xx411c7mD\r\nSet-Cookie: x=1"));
        assert!(!is_valid_bvid(""));
    }
}
