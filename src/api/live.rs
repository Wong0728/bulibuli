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
    let info = state
        .media
        .live_recorder
        .start_with_options(body.room_id, RecordingTrigger::Manual, mode)
        .await?;
    state
        .business
        .live_source_service
        .set_manual_latch(info.room_id, false)
        .await?;
    Ok(Json(ApiResponse::with_message(json!(info), "录制已开始")))
}

async fn stop_recording(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let info = state
        .media
        .live_recorder
        .stop(body.room_id)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    state
        .business
        .live_source_service
        .set_manual_latch(body.room_id, true)
        .await?;
    Ok(Json(ApiResponse::with_message(
        json!(info),
        "录制已停止，本场直播不会自动重启",
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
        "sources": items, "sessions": sessions, "monitor_running": true,
        "risk_notice": state.business.live_monitor.risk_snapshot().await,
        "synced_at": chrono::Utc::now().to_rfc3339(), "poll_interval_secs": 30,
    }))))
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
            auto_record_enabled: body.auto_record_enabled.unwrap_or(true),
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
