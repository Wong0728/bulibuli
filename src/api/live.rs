use crate::error::{ApiResponse, AppError};
use crate::services::live_recorder::RecordingTrigger;
use crate::services::live_source::{
    schedule_from_json, CaptureMode, NewLiveSource, UpdateLiveSource, WeeklySchedule,
};
use crate::state::SharedState;
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/live/room-info", get(room_info))
        .route("/api/live/start", post(start_recording))
        .route("/api/live/stop", post(stop_recording))
        .route("/api/live/status", get(recording_status))
        .route("/api/live/dashboard", get(dashboard))
        .route("/api/live/source/add", post(add_source))
        .route("/api/live/source/update", post(update_source))
        .route("/api/live/source/delete", post(delete_source))
        .route("/api/live/events", get(events))
        .route("/api/live/history", get(history))
        .route("/api/live/history/{recording_id}", get(history_item))
        .route("/api/live/history/{recording_id}/merge", post(start_merge))
        .route(
            "/api/live/history/{recording_id}/open-directory",
            post(open_history_directory),
        )
        .route("/api/live/recovery", get(recovery))
        .route("/api/live/merge/{job_id}", get(merge_job))
        .route("/api/live/merge/{job_id}/cancel", post(cancel_merge))
}

#[derive(Deserialize)]
struct RoomQuery {
    room_id: i64,
}
#[derive(Deserialize)]
struct RoomBody {
    room_id: i64,
}
#[derive(Deserialize)]
struct EventsQuery {
    room_id: i64,
    #[serde(default)]
    after_seq: u64,
    limit: Option<usize>,
}
#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}
#[derive(Deserialize)]
struct AddSourceBody {
    room_id: i64,
    auto_record_enabled: Option<bool>,
    weekly_schedule: Option<WeeklySchedule>,
    capture_mode: Option<CaptureMode>,
}

async fn room_info(
    State(state): State<SharedState>,
    Query(query): Query<RoomQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if query.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    let cookies = state.infra.settings_service.cookie_header().await?;
    let init = state
        .bili
        .bili_api
        .live_room_init(query.room_id, &cookies)
        .await?;
    let info = state
        .bili
        .bili_api
        .live_get_info(init.room_id, &cookies)
        .await?;
    let profile = state
        .bili
        .bili_api
        .get_user_info(init.uid, &cookies)
        .await
        .ok();
    let recording = state.media.live_recorder.status(init.room_id).await;
    let source = state
        .business
        .live_source_service
        .find(init.room_id)
        .await?;
    Ok(Json(ApiResponse::success(json!({
        "room_id": init.room_id, "short_id": init.short_id, "uid": init.uid,
        "anchor_name": profile.as_ref().map(|p| p.name.as_str()).unwrap_or(""),
        "face": profile.as_ref().map(|p| p.face.as_str()).unwrap_or(""),
        "title": info.title, "live_status": init.live_status,
        "live_status_text": match init.live_status { 0 => "未开播", 1 => "直播中", 2 => "轮播中", _ => "未知" },
        "online": info.online, "user_cover": info.user_cover, "area_name": info.area_name,
        "parent_area_name": info.parent_area_name, "live_time": info.live_time, "tags": info.tags,
        "is_portrait": info.is_portrait, "encrypted": init.encrypted, "is_recording": recording.is_some(),
        "recording_status": recording.as_ref().map(|v| v.status.to_string()),
        "can_start": init.is_live() && recording.is_none(), "is_saved": source.is_some(),
    }))))
}

async fn start_recording(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    let mode = match state
        .business
        .live_source_service
        .find(body.room_id)
        .await?
    {
        Some(source) => CaptureMode::parse(&source.capture_mode).unwrap_or_default(),
        None => CaptureMode::Standard,
    };
    let source_before = state
        .business
        .live_source_service
        .find(body.room_id)
        .await?;
    if source_before.is_some() {
        state
            .business
            .live_source_service
            .set_manual_latch(body.room_id, false)
            .await?;
    }
    let info = match state
        .media
        .live_recorder
        .start_with_options(body.room_id, RecordingTrigger::Manual, mode)
        .await
    {
        Ok(info) => info,
        Err(error) => {
            if let Some(source) = source_before {
                let _ = state
                    .business
                    .live_source_service
                    .set_manual_stop_session(body.room_id, source.manual_stop_session_key)
                    .await;
            }
            return Err(error.into());
        }
    };
    Ok(Json(ApiResponse::with_message(json!(info), "录制已开始")))
}

async fn stop_recording(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let session_key = match state.infra.settings_service.cookie_header().await {
        Ok(cookies) => state
            .bili
            .bili_api
            .live_room_init(body.room_id, &cookies)
            .await
            .ok()
            .filter(|init| init.is_live() && init.live_time > 0)
            .map(|init| format!("{}:{}", init.room_id, init.live_time)),
        Err(_) => None,
    };
    let source_before = state
        .business
        .live_source_service
        .find(body.room_id)
        .await?;
    if session_key.is_some() {
        state
            .business
            .live_source_service
            .set_manual_stop_session(body.room_id, session_key.clone())
            .await?;
    } else {
        state
            .business
            .live_source_service
            .set_manual_latch(body.room_id, true)
            .await?;
    }
    let job = match state.media.live_recorder.request_stop(body.room_id).await {
        Ok(job) => job,
        Err(error) => {
            if let Some(source) = source_before.clone() {
                if source.manual_stop_session_key.is_some() {
                    let _ = state
                        .business
                        .live_source_service
                        .set_manual_stop_session(body.room_id, source.manual_stop_session_key)
                        .await;
                } else {
                    let _ = state
                        .business
                        .live_source_service
                        .set_manual_latch(body.room_id, source.manual_stop_latched)
                        .await;
                }
            }
            return Err(AppError::BadRequest(error.to_string()));
        }
    };
    Ok(Json(ApiResponse::with_message(
        json!({"operation_id": job.id, "recording_id": job.recording_id, "status": job.status, "progress": job.progress}),
        "停止请求已接受，正在后台收尾与合并",
    )))
}

async fn recording_status(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let sessions = state.media.live_recorder.status_all().await;
    Ok(Json(ApiResponse::success(
        json!({"count": sessions.len(), "sessions": sessions}),
    )))
}

async fn dashboard(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let sources = state.business.live_source_service.list().await?;
    let runtime = state.business.live_monitor.runtime_snapshot().await;
    let sessions = state.media.live_recorder.status_all().await;
    let items = sources.into_iter().map(|source| {
        let run = runtime.get(&source.room_id);
        json!({
            "id": source.id, "room_id": source.room_id, "short_id": source.short_id, "uid": source.uid,
            "anchor_name": source.anchor_name, "face": source.face, "title": source.title, "cover": source.cover,
            "auto_record_enabled": source.auto_record_enabled, "capture_mode": source.capture_mode,
            "weekly_schedule": schedule_from_json(source.weekly_schedule.as_deref()),
            "schedule_all_day": source.weekly_schedule.is_none(), "manual_stop_latched": source.manual_stop_latched,
            "runtime": run,
        })
    }).collect::<Vec<_>>();
    Ok(Json(ApiResponse::success(json!({
        "sources": items, "sessions": sessions,
        "monitor": state.business.live_monitor.health_snapshot().await,
        "risk_notice": state.business.live_monitor.risk_snapshot().await,
        "synced_at": chrono::Utc::now().to_rfc3339(), "server_now": chrono::Local::now().to_rfc3339(),
        "server_timezone": chrono::Local::now().format("%Z %:z").to_string(), "poll_interval_secs": 30,
        "merge_jobs": state.media.live_recorder.merge_jobs().await,
        "recovery": state.media.live_recorder.recovery_items().await?,
        "disk": disk_status(&state.infra.paths.download_dir),
    }))))
}

/// 录制目录所在磁盘的余量；读取失败时返回 null，前端按“未知”降级展示。
fn disk_status(dir: &Path) -> serde_json::Value {
    match (fs2::available_space(dir), fs2::total_space(dir)) {
        (Ok(available), Ok(total)) => json!({
            "available_bytes": available,
            "total_bytes": total,
            "path_hidden": true,
        }),
        _ => serde_json::Value::Null,
    }
}

async fn add_source(
    State(state): State<SharedState>,
    Json(body): Json<AddSourceBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    let cookies = state.infra.settings_service.cookie_header().await?;
    let init = state
        .bili
        .bili_api
        .live_room_init(body.room_id, &cookies)
        .await?;
    let info = state
        .bili
        .bili_api
        .live_get_info(init.room_id, &cookies)
        .await?;
    let profile = state
        .bili
        .bili_api
        .get_user_info(init.uid, &cookies)
        .await
        .ok();
    let source = state
        .business
        .live_source_service
        .add(NewLiveSource {
            room_id: init.room_id,
            short_id: init.short_id,
            uid: init.uid,
            anchor_name: profile
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| format!("UID {}", init.uid)),
            face: profile.as_ref().map(|p| p.face.clone()).unwrap_or_default(),
            title: info.title,
            cover: info.user_cover,
            auto_record_enabled: body.auto_record_enabled.unwrap_or(false),
            weekly_schedule: body.weekly_schedule,
            capture_mode: body.capture_mode.unwrap_or_default(),
        })
        .await
        .map_err(|e| AppError::Conflict(e.to_string()))?;
    state.business.live_monitor.wake_room(source.room_id).await;
    Ok(Json(ApiResponse::with_message(
        json!(source),
        "直播源已添加",
    )))
}

async fn update_source(
    State(state): State<SharedState>,
    Json(body): Json<UpdateLiveSource>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let source = state
        .business
        .live_source_service
        .update(body)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.business.live_monitor.wake_room(source.room_id).await;
    Ok(Json(ApiResponse::with_message(
        json!(source),
        "直播源设置已保存",
    )))
}

async fn delete_source(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if state
        .media
        .live_recorder
        .status(body.room_id)
        .await
        .is_some()
    {
        return Err(AppError::Conflict("请先停止当前录制再删除直播源".into()));
    }
    let deleted = state
        .business
        .live_source_service
        .delete(body.room_id)
        .await?;
    Ok(Json(ApiResponse::success(json!({"deleted": deleted}))))
}

async fn events(
    State(state): State<SharedState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 100);
    let events = state
        .media
        .live_recorder
        .events(query.room_id, query.after_seq, limit)
        .await;
    let next_seq = events
        .last()
        .map(|event| event.seq)
        .unwrap_or(query.after_seq);
    Ok(Json(ApiResponse::success(
        json!({"events": events, "next_seq": next_seq}),
    )))
}

async fn history(
    State(state): State<SharedState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let rows = state
        .media
        .live_recorder
        .history(query.limit.unwrap_or(30))
        .await?;
    Ok(Json(ApiResponse::success(json!({
        "items": rows.iter().map(history_view).collect::<Vec<_>>()
    }))))
}

async fn history_item(
    State(state): State<SharedState>,
    axum::extract::Path(recording_id): axum::extract::Path<i32>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = state
        .media
        .live_recorder
        .history_item(recording_id)
        .await?
        .ok_or_else(|| AppError::NotFound("录制历史不存在".into()))?;
    Ok(Json(ApiResponse::success(history_view(&row))))
}

async fn start_merge(
    State(state): State<SharedState>,
    axum::extract::Path(recording_id): axum::extract::Path<i32>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let job = state
        .media
        .live_recorder
        .retry_merge(recording_id)
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(Json(ApiResponse::with_message(
        json!(job),
        "录制合并任务已后台创建",
    )))
}

async fn merge_job(
    State(state): State<SharedState>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let job = state
        .media
        .live_recorder
        .merge_job(&job_id)
        .await
        .ok_or_else(|| AppError::NotFound("合并任务不存在".into()))?;
    Ok(Json(ApiResponse::success(json!(job))))
}

async fn cancel_merge(
    State(state): State<SharedState>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let job = state
        .media
        .live_recorder
        .cancel_merge(&job_id)
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(Json(ApiResponse::with_message(
        json!(job),
        "已请求取消合并任务",
    )))
}

async fn recovery(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let items = state.media.live_recorder.recovery_items().await?;
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

async fn open_history_directory(
    State(state): State<SharedState>,
    axum::extract::Path(recording_id): axum::extract::Path<i32>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = state
        .media
        .live_recorder
        .history_item(recording_id)
        .await?
        .ok_or_else(|| AppError::NotFound("录制历史不存在".into()))?;
    let output = row
        .output_path
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("该录制没有可打开的输出文件".into()))?;
    let directory = Path::new(output)
        .parent()
        .ok_or_else(|| AppError::BadRequest("录制输出路径无效".into()))?;
    if !directory.is_dir() {
        return Err(AppError::NotFound("录制输出目录不存在".into()));
    }
    open::that(directory)
        .map_err(|_| AppError::Internal("open recording directory failed".to_owned()))?;
    Ok(Json(ApiResponse::with_message(
        json!({"recording_id": recording_id}),
        "已打开录制目录",
    )))
}

fn history_view(row: &crate::models::live_recording::Model) -> serde_json::Value {
    json!({
        "id": row.id, "room_id": row.room_id, "title": row.title, "cover": row.cover,
        "status": row.status, "started_at": row.started_at, "ended_at": row.ended_at,
        "duration": row.duration, "file_size": row.file_size, "error_msg": row.error_msg.as_deref().map(public_error),
        "trigger": row.trigger, "capture_mode": row.capture_mode,
        "interaction_status": row.interaction_status, "interaction_error": row.interaction_error.as_deref().map(public_error),
        "danmaku_count": row.danmaku_count, "unique_user_count": row.unique_user_count,
        "segment_index": row.segment_index, "restart_attempts": row.restart_attempts,
        "stop_reason": row.stop_reason, "is_recoverable": row.is_recoverable,
        "has_output": row.output_path.as_deref().is_some_and(|path| Path::new(path).exists()),
        "has_events": row.event_path.as_deref().is_some_and(|path| Path::new(path).exists()),
    })
}

fn public_error(value: &str) -> String {
    let trimmed = value.trim();
    // 包含 URL 的错误信息：保留协议与主机便于诊断，但剥掉路径与签名参数，
    // 避免流地址 token 通过 API 泄露。
    if trimmed.contains("://") {
        return crate::services::live_recorder::ffmpeg_session::redact_diagnostics(trimmed);
    }
    let bytes = trimmed.as_bytes();
    let windows_absolute =
        bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/');
    if trimmed.starts_with('/')
        || trimmed.starts_with("\\\\")
        || windows_absolute
    {
        "diagnostic redacted".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_error_strips_signed_url_tokens() {
        let message = "获取流失败: https://cdn.example.com/live.flv?sign=abc&token=secret";
        let result = public_error(message);
        assert!(result.contains("https://cdn.example.com/live.flv"));
        assert!(!result.contains("token=secret"));
        assert!(!result.contains("sign=abc"));
    }

    #[test]
    fn public_error_redacts_absolute_paths() {
        assert_eq!(public_error("D:\\downloads\\live\\out.flv"), "diagnostic redacted");
        assert_eq!(public_error("/var/data/out.flv"), "diagnostic redacted");
    }

    #[test]
    fn public_error_keeps_plain_messages() {
        assert_eq!(public_error(" 认证被拒绝 "), "认证被拒绝");
    }
}
