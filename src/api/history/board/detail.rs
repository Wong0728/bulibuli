//! 单视频详情响应（抽屉用）：history 记录、侧车状态、任务进度与文件列表的完整组装。

use super::aggregate::{aggregate_task_progress, display_path_for};
use crate::error::AppError;
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use serde_json::{json, Value};

/// 单视频详情响应（抽屉用）。
pub(super) async fn build_single_video_response(
    business: &BusinessState,
    infra: &InfraState,
    video_processor: std::sync::Arc<crate::services::video_processor::VideoProcessor>,
    bvid: &str,
    history_id: Option<i32>,
    server_time: i64,
    can_open_directory: bool,
) -> Result<Value, AppError> {
    // DB 错误（锁超时/磁盘故障）应返回 500 而非混入 404，避免排障被误导。
    let h = match history_id {
        Some(id) => business
            .history_service
            .find_by_id(id)
            .await
            .map(|value| value.filter(|history| history.bvid == bvid))?,
        None => business.history_service.find_by_bvid(bvid).await?,
    };
    let Some(h) = h else {
        return Err(AppError::NotFound("未找到该视频记录".to_string()));
    };

    let sidecar = business
        .history_service
        .sidecar_status(&h.bvid, h.uid.as_deref(), h.file_path.as_deref())
        .await;

    // 查询博主信息；未监控博主时使用 history 的 owner 快照兜底。
    let blogger_info = if let Some(uid) = h.uid.as_deref() {
        business
            .blogger_service
            .find_by_uid(uid)
            .await
            .ok()
            .flatten()
            .map(|b| {
                json!({
                    "uid": b.uid,
                    "name": b.name,
                    "face": b.face,
                    "sign": b.sign,
                    "level": b.level,
                    "last_seen_name": b.last_seen_name,
                    "last_seen_face": b.last_seen_face,
                    "last_seen_at": b.last_seen_at.map(|t| t.to_rfc3339()),
                    "notice_visible": b.last_seen_at.is_some(),
                })
            })
            .or_else(|| {
                (h.owner_name.is_some() || h.owner_face.is_some()).then(|| {
                    json!({
                        "uid": uid,
                        "name": h.owner_name.clone(),
                        "face": h.owner_face.clone(),
                        "notice_visible": false,
                    })
                })
            })
    } else {
        None
    };

    // 查最新 download_task
    let tasks = business
        .history_service
        .download_tasks_for_history(&h)
        .await;
    let task = aggregate_task_progress(Some(&tasks), &h);
    let task_info = json!({
        "progress_percent": task.progress,
        "speed": task.speed,
        "status": task.status,
        "downloaded_size": task.downloaded_size,
        "total_size": task.total_size,
        "task_id": task.task_id,
        "version": task.version,
        "priority": task.priority,
        "type": task.task_type,
        // 失败元数据：失败原因冒泡到抽屉，前端可展示「为什么失败」详情。
        "error": task.error,
        "error_kind": task.error_kind,
        "fallback_reason": task.fallback_reason,
    });

    let files = business
        .history_service
        .scan_files(&h.bvid, h.uid.as_deref(), h.file_path.as_deref())
        .await;

    let settings = infra.settings_service.current();
    let can_browser_download = settings.board.browser_download_enabled;
    // 烧录能力：探测当前 FFmpeg 是否含 ass 滤镜与视频编码器（结果带缓存），
    // 不支持时抽屉会把烧录按钮置灰，避免点击后才失败。
    let custom_ffmpeg = {
        let path = settings.ffmpeg.custom_path.trim().to_string();
        (!path.is_empty()).then_some(path)
    };
    let can_burn = crate::services::subtitle_burner::ffmpeg_supports_burn(
        video_processor,
        settings.ffmpeg.mode.as_str(),
        custom_ffmpeg.as_deref(),
    )
    .await;
    let configured_path_display_mode =
        if settings.board.path_display_mode == "hidden" && settings.board.show_relative_path {
            "relative".to_string()
        } else {
            settings.board.path_display_mode.clone()
        };
    let path_display_mode = if configured_path_display_mode == "absolute" && !can_open_directory {
        "relative".to_string()
    } else {
        configured_path_display_mode
    };
    let file_path = display_path_for(
        &business.history_service,
        h.file_path.as_deref(),
        &path_display_mode,
    );
    let cover_local_path = display_path_for(
        &business.history_service,
        h.cover_local_path.as_deref(),
        &path_display_mode,
    );
    let relative_path = h
        .file_path
        .as_deref()
        .and_then(|path| business.history_service.to_relative_path(path));
    let files = files
        .into_iter()
        .map(|file| file_entry_view(&business.history_service, file, &path_display_mode))
        .collect::<Vec<_>>();

    Ok(json!({
        "server_time": server_time,
        "video": {
            "bvid": h.bvid,
            "history_id": h.id,
            "version": h.version,
            "cid": h.cid,
            "page": h.page,
            "part_title": h.part_title,
            "title": h.title,
            "source": h.source,
            "pub_timestamp": h.pub_timestamp,
            "pub_date": h.pub_date,
            "pic": h.pic,
            "duration": h.duration,
            "view": h.view,
            "state": h.state,
            "cover_local_path": cover_local_path,
            "file_path": file_path,
            "relative_path": relative_path,
            "can_open_directory": can_open_directory,
            "can_browser_download": can_browser_download,
            "can_burn": can_burn,
            "reupload_of": h.reupload_of,
            "pay_note": h.pay_note,
            "md5": h.md5,
            "md5_last_checked_at": h.md5_last_checked_at.map(|t| t.to_rfc3339()),
            "sha256": h.sha256,
            "sha256_last_checked_at": h.sha256_last_checked_at.map(|t| t.to_rfc3339()),
            "download_time": h.download_time.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            "view_refreshed_at": h.view_refreshed_at.map(|t| t.to_rfc3339()),
            "sidecar": sidecar,
            "task": task_info,
            "blogger": blogger_info,
            "files": files,
            "burned": {
                "danmaku": h.burned_danmaku.unwrap_or(false),
                "subtitle": h.burned_subtitle.unwrap_or(false),
            },
        }
    }))
}

fn file_entry_view(
    history_service: &crate::services::history::HistoryService,
    file: crate::services::history::FileEntry,
    mode: &str,
) -> Value {
    let display_path = display_path_for(history_service, Some(&file.path), mode);
    json!({
        "file_type": file.file_type,
        "name": file.name,
        "path": file.path,
        "display_path": display_path,
        "size": file.size,
        "format": file.format,
        "location": file.location,
        "is_current": file.is_current,
        "version": file.version,
        "modified_at": file.modified_at,
    })
}
