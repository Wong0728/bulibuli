//! 视频附件接口：弹幕/评论下载与读取、封面下载与图片代理。

use crate::error::{ApiResponse, AppError};
use crate::services::danmaku::SidecarArchivePolicy;
use crate::services::file_safety::{ensure_existing_within_root, validate_uid};
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use crate::state::media::MediaState;
use crate::state::SharedState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// 将 DanmakuService 的 `{success, message, ...}` 返回值转换为统一 API 信封。
fn danmaku_result_to_envelope(mut value: Value) -> Result<Json<ApiResponse<Value>>, AppError> {
    let ok = value
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(object) = value.as_object_mut() {
        object.remove("success");
        object.remove("message");
    }
    if ok {
        Ok(Json(ApiResponse::with_message(value, message)))
    } else {
        Err(AppError::BadRequest(message))
    }
}

#[derive(Deserialize)]
pub(super) struct DownloadDanmakuRequest {
    bvid: String,
    uid: Option<String>,
    source: Option<String>,
    page: Option<i32>,
    history_id: Option<i32>,
}

pub(super) async fn download_danmaku(
    State(state): State<SharedState>,
    Json(req): Json<DownloadDanmakuRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = req.bvid.trim();
    let cookies = state.infra.settings_service.cookie_header().await?;
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }
    if let Some(uid) = req.uid.as_deref() {
        validate_uid(uid)?;
    }
    let settings = state.infra.settings_service.current();
    let archive_policy = SidecarArchivePolicy::new(
        &settings.danmaku_comment.sidecar_archive_mode,
        settings.danmaku_comment.sidecar_archive_limit as i64,
    );
    // 解析保存目录：优先使用视频所在目录，确保弹幕和视频位于同一位置。
    let save_dir = resolve_sidecar_dir(
        &state.media,
        &state.business,
        &state.infra,
        bvid,
        req.source.as_deref(),
        req.history_id,
        req.page,
    )
    .await;
    let result = state
        .media
        .danmaku_service
        .download_danmaku_to(
            bvid,
            req.page,
            Some(&cookies),
            req.uid.as_deref(),
            archive_policy,
            save_dir.as_deref(),
        )
        .await?;
    danmaku_result_to_envelope(result)
}

#[derive(Deserialize)]
pub(super) struct DownloadCommentsRequest {
    bvid: String,
    uid: Option<String>,
    source: Option<String>,
    history_id: Option<i32>,
}

pub(super) async fn download_comments(
    State(state): State<SharedState>,
    Json(req): Json<DownloadCommentsRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = req.bvid.trim();
    let cookies = state.infra.settings_service.cookie_header().await?;
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }
    if let Some(uid) = req.uid.as_deref() {
        validate_uid(uid)?;
    }
    let settings = state.infra.settings_service.current();
    let main_limit = settings.danmaku_comment.comments_main_limit as usize;
    let reply_mode = settings.danmaku_comment.comments_reply_mode.as_str();
    let filter_regex = settings.danmaku_comment.comments_filter_regex.as_str();
    let archive_policy = SidecarArchivePolicy::new(
        &settings.danmaku_comment.sidecar_archive_mode,
        settings.danmaku_comment.sidecar_archive_limit as i64,
    );
    // 解析保存目录：优先使用视频所在目录
    let save_dir = resolve_sidecar_dir(
        &state.media,
        &state.business,
        &state.infra,
        bvid,
        req.source.as_deref(),
        req.history_id,
        None,
    )
    .await;
    let result = state
        .media
        .danmaku_service
        .download_comments_to(
            bvid,
            Some(&cookies),
            req.uid.as_deref(),
            main_limit,
            reply_mode,
            filter_regex,
            archive_policy,
            save_dir.as_deref(),
        )
        .await?;
    danmaku_result_to_envelope(result)
}

/// 解析弹幕/评论的保存目录：优先使用视频文件所在目录，确保弹幕和视频在同一位置。
/// 如果视频在 manual/ 目录下，弹幕/评论也保存在那里。
async fn resolve_sidecar_dir(
    media: &MediaState,
    business: &BusinessState,
    infra: &InfraState,
    bvid: &str,
    source: Option<&str>,
    history_id: Option<i32>,
    page: Option<i32>,
) -> Option<std::path::PathBuf> {
    // DB 错误（锁超时/磁盘故障）不应与"记录不存在"混淆：失败时记日志并按无记录
    // 处理（sidecar 目录解析有多个候选来源，降级而非 500 对用户更友好）。
    let h = match history_id {
        Some(id) => match business.history_service.find_by_id(id).await {
            Ok(value) => value.filter(|history| history.bvid == bvid),
            Err(error) => {
                tracing::warn!(%error, "按 id 查询历史记录失败，跳过侧车目录解析");
                None
            }
        },
        None => match business.history_service.find_by_bvid(bvid).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "按 bvid 查询历史记录失败，跳过侧车目录解析");
                None
            }
        },
    };
    // When the caller explicitly selects a source, do not let a history row
    // from the other source redirect the sidecar into the wrong artifact dir.
    let history_matches_source = source
        .map(|selected| h.as_ref().is_some_and(|history| history.source == selected))
        .unwrap_or(true);
    if history_matches_source {
        if let Some(h) = h.as_ref() {
            if let Some(fp) = h.file_path.as_deref() {
                if let Some(parent) = std::path::Path::new(fp).parent() {
                    if parent.exists() {
                        let download_dir = media.download_manager.download_dir(None).await;
                        // DB 中的 file_path 可能被污染（security.toml 注入等纵深场景），
                        // 与 cover.rs 对 cover_local_path 的防御一致：校验写入目录在 download_dir 内。
                        if ensure_existing_within_root(parent, &download_dir)
                            .await
                            .is_ok()
                        {
                            return Some(parent.to_path_buf());
                        }
                    }
                }
            }
        }
    }
    if let Some(source @ ("manual" | "auto")) = source {
        if let Some(directory) = media
            .download_manager
            .artifact_dir_for_bvid_page(
                bvid,
                source,
                page.or_else(|| h.as_ref().and_then(|value| value.page)),
            )
            .await
        {
            return Some(directory);
        }
    }
    if source == Some("manual") {
        let files = business
            .history_service
            .scan_files(
                bvid,
                h.as_ref().and_then(|value| value.uid.as_deref()),
                h.as_ref().and_then(|value| value.file_path.as_deref()),
            )
            .await;
        if let Some(file) = files
            .iter()
            .filter(|file| file.file_type == "video" && file.location == "manual")
            .max_by_key(|file| file.modified_at)
        {
            if let Some(path) = business
                .history_service
                .resolve_download_relative_path(&file.path)
            {
                return path.parent().map(std::path::Path::to_path_buf);
            }
        }
        return Some(infra.paths.download_dir.join("manual"));
    }
    if source.is_none() {
        if let Some(h) = h {
            if let Some(ref fp) = h.file_path {
                let path = std::path::Path::new(fp);
                if let Some(parent) = path.parent() {
                    if parent.exists() {
                        return Some(parent.to_path_buf());
                    }
                }
            }
        }
    }
    if page.is_some() {
        return Some(infra.paths.download_dir.clone());
    }
    // 未找到视频文件记录，返回 None 让 DanmakuService 使用默认逻辑
    None
}

#[derive(Deserialize)]
pub(super) struct GetCommentsQuery {
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
pub(super) async fn get_comments(
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
pub(super) struct GetDanmakuQuery {
    bvid: String,
    path: String,
    history_id: Option<i32>,
}

/// 读取 scan_files 已发现的某个弹幕版本。JSON 返回结构化列表，XML/TXT 返回文本。
pub(super) async fn get_danmaku(
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
