//! 读取已下载的弹幕/评论侧车文件（抽屉「弹幕」「评论」展示区）。

use crate::error::{ApiResponse, AppError};
use crate::services::file_safety::{ensure_existing_within_root, validate_uid};
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub(crate) struct GetCommentsQuery {
    bvid: String,
    uid: Option<String>,
    path: Option<String>,
    history_id: Option<i32>,
}

/// 通过 history 文件路径或明确 UID 定位附件，避免在请求路径遍历全部下载目录。
async fn find_download_file(
    business: &BusinessState,
    infra: &InfraState,
    bvid: &str,
    uid: Option<&str>,
    filename: &str,
    history_id: Option<i32>,
) -> Option<std::path::PathBuf> {
    let history = match history_id {
        Some(id) => match business.history_service.find_by_id(id).await {
            Ok(value) => value.filter(|history| history.bvid == bvid),
            Err(error) => {
                tracing::warn!(%error, "按 id 查询历史记录失败，跳过侧车文件查找");
                None
            }
        },
        None => match business.history_service.find_by_bvid(bvid).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "按 bvid 查询历史记录失败，跳过侧车文件查找");
                None
            }
        },
    };
    if let Some(history) = history {
        if let Some(parent) = history
            .file_path
            .as_deref()
            .and_then(|path| std::path::Path::new(path).parent())
        {
            let path = parent.join(filename);
            if ensure_existing_within_root(&infra.paths.download_dir, &path)
                .await
                .is_ok()
                && tokio::fs::try_exists(&path).await.unwrap_or(false)
            {
                return Some(path);
            }
        }
    }
    if let Some(u) = uid {
        if let Ok(validated) = validate_uid(u) {
            let p = infra
                .paths
                .download_dir
                .join(validated.as_str())
                .join(filename);
            if ensure_existing_within_root(&infra.paths.download_dir, &p)
                .await
                .is_ok()
                && tokio::fs::try_exists(&p).await.unwrap_or(false)
            {
                return Some(p);
            }
        }
    }
    let p = infra.paths.download_dir.join(filename);
    if ensure_existing_within_root(&infra.paths.download_dir, &p)
        .await
        .is_ok()
        && tokio::fs::try_exists(&p).await.unwrap_or(false)
    {
        return Some(p);
    }
    None
}

/// 从评论 HTML 中提取内嵌的 <script id="cmt-data"> JSON 数组。
fn extract_embedded_comments(html: &str) -> Value {
    let marker = "id=\"cmt-data\">";
    if let Some(start) = html.find(marker) {
        let rest = &html[start + marker.len()..];
        if let Some(end) = rest.find("</script>") {
            let json_str = rest[..end].replace("<\\/", "</");
            if let Ok(v) = serde_json::from_str::<Value>(&json_str) {
                return v;
            }
        }
    }
    json!([])
}

/// 读取已下载的评论（抽屉"评论"区用）。优先读 {bvid}_comments.html 并提取内嵌 JSON；
/// 兼容旧 {bvid}_comments.txt 返回纯文本。只读本地文件，不触发 B 站请求。
pub(crate) async fn get_comments(
    State(state): State<SharedState>,
    axum::extract::Query(q): axum::extract::Query<GetCommentsQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = q.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }

    // 定位 UID：优先使用传入值，否则查询 history。
    let uid = match q.uid.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(u) => {
            validate_uid(u)?;
            Some(u.to_string())
        }
        None => state
            .business
            .history_service
            .find_by_bvid(bvid)
            .await
            .ok()
            .flatten()
            .and_then(|h| h.uid),
    };
    // 显式路径只允许命中 scan_files 返回的本 BV 评论文件，防止路径穿越。
    if let Some(requested_path) = q.path.as_deref() {
        let Some(path) = resolve_scanned_sidecar(
            &state.business,
            bvid,
            requested_path,
            "comment",
            q.history_id,
        )
        .await
        else {
            return Err(AppError::NotFound("未找到指定评论版本".to_string()));
        };
        return read_comments_file(&path).await;
    }

    // 未指定版本时优先使用当前目录的结构化 HTML，再读取旧纯文本评论文件。
    if let Some(path) = find_download_file(
        &state.business,
        &state.infra,
        bvid,
        uid.as_deref(),
        &format!("{bvid}_comments.html"),
        q.history_id,
    )
    .await
    {
        return read_comments_file(&path).await;
    }
    if let Some(path) = find_download_file(
        &state.business,
        &state.infra,
        bvid,
        uid.as_deref(),
        &format!("{bvid}_comments.txt"),
        q.history_id,
    )
    .await
    {
        return read_comments_file(&path).await;
    }

    Ok(Json(ApiResponse::success(json!({
        "exists": false,
        "message": "未下载评论"
    }))))
}

const MAX_SIDECAR_VIEW_BYTES: u64 = 32 * 1024 * 1024;

async fn resolve_scanned_sidecar(
    business: &BusinessState,
    bvid: &str,
    requested_path: &str,
    file_type: &str,
    history_id: Option<i32>,
) -> Option<std::path::PathBuf> {
    let history = match history_id {
        Some(id) => match business.history_service.find_by_id(id).await {
            Ok(value) => value.filter(|history| history.bvid == bvid),
            Err(error) => {
                tracing::warn!(%error, "按 id 查询历史记录失败，跳过扫描侧车查找");
                None
            }
        },
        None => match business.history_service.find_by_bvid(bvid).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "按 bvid 查询历史记录失败，跳过扫描侧车查找");
                None
            }
        },
    };
    let files = business
        .history_service
        .scan_files(
            bvid,
            history.as_ref().and_then(|value| value.uid.as_deref()),
            history
                .as_ref()
                .and_then(|value| value.file_path.as_deref()),
        )
        .await;
    files
        .iter()
        .find(|file| file.file_type == file_type && file.path == requested_path)
        .and_then(|file| {
            business
                .history_service
                .resolve_download_relative_path(&file.path)
        })
}

async fn read_sidecar_text(path: &std::path::Path, label: &str) -> Result<String, AppError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| AppError::Internal(format!("读取{label}元数据失败: {error}")))?;
    if metadata.len() > MAX_SIDECAR_VIEW_BYTES {
        return Err(AppError::BadRequest(format!(
            "{label}文件超过 32 MiB，请复制路径后使用本地工具查看"
        )));
    }
    tokio::fs::read_to_string(path)
        .await
        .map_err(|error| AppError::Internal(format!("读取{label}失败: {error}")))
}

async fn read_comments_file(path: &std::path::Path) -> Result<Json<ApiResponse<Value>>, AppError> {
    let content = read_sidecar_text(path, "评论").await?;
    let is_html = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("html"));
    Ok(Json(ApiResponse::success(if is_html {
        json!({
            "exists": true,
            "format": "json",
            "comments": extract_embedded_comments(&content),
        })
    } else {
        json!({
            "exists": true,
            "format": "txt",
            "content": content,
        })
    })))
}

#[derive(Deserialize)]
pub(crate) struct GetDanmakuQuery {
    bvid: String,
    path: String,
    history_id: Option<i32>,
}

/// 读取 scan_files 已发现的某个弹幕版本。JSON 返回结构化列表，XML/TXT 返回文本。
pub(crate) async fn get_danmaku(
    State(state): State<SharedState>,
    axum::extract::Query(q): axum::extract::Query<GetDanmakuQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = q.bvid.trim();
    if bvid.is_empty() || q.path.trim().is_empty() {
        return Err(AppError::BadRequest(
            "请提供视频 BV 号和弹幕文件路径".to_string(),
        ));
    }
    let Some(path) = resolve_scanned_sidecar(
        &state.business,
        bvid,
        q.path.trim(),
        "danmaku",
        q.history_id,
    )
    .await
    else {
        return Err(AppError::NotFound("未找到指定弹幕版本".to_string()));
    };
    let content = read_sidecar_text(&path, "弹幕").await?;
    let format = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("txt")
        .to_ascii_lowercase();
    if format == "json" {
        let value: Value = serde_json::from_str(&content)
            .map_err(|error| AppError::Internal(format!("解析弹幕 JSON 失败: {error}")))?;
        return Ok(Json(ApiResponse::success(json!({
            "exists": true,
            "format": format,
            "video_info": value.get("video_info").cloned().unwrap_or(Value::Null),
            "danmaku": value
                .get("danmaku_list")
                .cloned()
                .unwrap_or_else(|| json!([])),
        }))));
    }
    Ok(Json(ApiResponse::success(json!({
        "exists": true,
        "format": format,
        "content": content,
    }))))
}
