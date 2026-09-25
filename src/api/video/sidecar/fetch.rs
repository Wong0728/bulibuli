//! 触发弹幕/评论下载并保存到视频所在目录（侧车文件写入入口）。

use crate::error::{ApiResponse, AppError};
use crate::services::danmaku::SidecarArchivePolicy;
use crate::services::file_safety::validate_uid;
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use crate::state::media::MediaState;
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::Value;

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
pub(crate) struct DownloadDanmakuRequest {
    bvid: String,
    uid: Option<String>,
    source: Option<String>,
    page: Option<i32>,
    history_id: Option<i32>,
}

pub(crate) async fn download_danmaku(
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
pub(crate) struct DownloadCommentsRequest {
    bvid: String,
    uid: Option<String>,
    source: Option<String>,
    history_id: Option<i32>,
}

pub(crate) async fn download_comments(
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
                        if crate::services::file_safety::ensure_existing_within_root(
                            &download_dir,
                            parent,
                        )
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
                        // 与上面显式选择来源的分支同一道纵深防御：DB 里的 file_path 可能被
                        // 污染，缺这一步就等于让该分支绕过下载根目录约束写盘。
                        let download_dir = media.download_manager.download_dir(None).await;
                        if crate::services::file_safety::ensure_existing_within_root(
                            &download_dir,
                            parent,
                        )
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
    if page.is_some() {
        return Some(infra.paths.download_dir.clone());
    }
    // 未找到视频文件记录，返回 None 让 DanmakuService 使用默认逻辑
    None
}
