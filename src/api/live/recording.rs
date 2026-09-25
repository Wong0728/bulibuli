//! 直播录制控制接口：房间信息查询、录制启动/停止与全局状态查询。

use super::RoomBody;
use crate::error::{ApiResponse, AppError};
use crate::models::operation_log::OperationTarget;
use crate::services::live_recorder::RecordingTrigger;
use crate::services::live_source::CaptureMode;
use crate::state::SharedState;
use axum::{
    extract::{Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub(crate) struct RoomQuery {
    room_id: i64,
}

pub(super) async fn room_info(
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
        .get_user_info(init.uid, &cookies, false)
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

pub(super) async fn start_recording(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    // 放入独立任务执行：客户端断开（刷新页面/前端超时）会丢弃本 handler 的
    // future，但不能打断录制启动流程和 ConflictGuard 的 commit/rollback，
    // 否则 sessions 里会留下永久卡死的 Starting 条目（房间无法再次启动）。
    let task = tokio::spawn(async move { run_start_recording(state, body).await });
    match task.await {
        Ok(result) => result,
        Err(error) => Err(AppError::Internal(format!("录制启动任务异常退出: {error}"))),
    }
}

async fn run_start_recording(
    state: SharedState,
    body: RoomBody,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let source_before = state
        .business
        .live_source_service
        .find(body.room_id)
        .await?;
    let guard = match source_before.as_ref() {
        Some(source) => Some(
            state
                .infra
                .conflict_guard
                .check_and_bump(
                    OperationTarget::LiveSource,
                    &source.id.to_string(),
                    body.expected_version,
                )
                .await?,
        ),
        None if body.expected_version.is_some() => {
            return Err(AppError::NotFound("直播源不存在".to_string()));
        }
        None => None,
    };
    let mode = source_before
        .as_ref()
        .map(|source| CaptureMode::parse(&source.capture_mode).unwrap_or_default())
        .unwrap_or(CaptureMode::Standard);
    if source_before.is_some() {
        if let Err(error) = state
            .business
            .live_source_service
            .set_manual_latch(body.room_id, false)
            .await
        {
            if let Some(guard) = guard {
                if let Err(rollback_error) = guard.rollback().await {
                    tracing::warn!(%rollback_error, "直播源版本守卫回滚失败");
                }
            }
            return Err(error.into());
        }
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
                if let Err(restore_error) = state
                    .business
                    .live_source_service
                    .set_manual_stop_session(body.room_id, source.manual_stop_session_key)
                    .await
                {
                    tracing::warn!(
                        room_id = body.room_id,
                        %restore_error,
                        "恢复手动停止会话状态失败"
                    );
                }
            }
            if let Some(guard) = guard {
                if let Err(rollback_error) = guard.rollback().await {
                    tracing::warn!(%rollback_error, "直播源版本守卫回滚失败");
                }
            }
            return Err(error.into());
        }
    };
    if let Some(guard) = guard {
        guard.commit();
    }
    Ok(Json(ApiResponse::with_message(json!(info), "录制已开始")))
}

pub(super) async fn stop_recording(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    // 同 start_recording：断开连接不能打断停止收尾与 guard 收尾。
    let task = tokio::spawn(async move { run_stop_recording(state, body).await });
    match task.await {
        Ok(result) => result,
        Err(error) => Err(AppError::Internal(format!("录制停止任务异常退出: {error}"))),
    }
}

async fn run_stop_recording(
    state: SharedState,
    body: RoomBody,
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
    let guard = match source_before.as_ref() {
        Some(source) => Some(
            state
                .infra
                .conflict_guard
                .check_and_bump(
                    OperationTarget::LiveSource,
                    &source.id.to_string(),
                    body.expected_version,
                )
                .await?,
        ),
        None if body.expected_version.is_some() => {
            return Err(AppError::NotFound("直播源不存在".to_string()));
        }
        None => None,
    };
    if session_key.is_some() {
        if let Err(error) = state
            .business
            .live_source_service
            .set_manual_stop_session(body.room_id, session_key.clone())
            .await
        {
            if let Some(guard) = guard {
                if let Err(rollback_error) = guard.rollback().await {
                    tracing::warn!(%rollback_error, "直播源版本守卫回滚失败");
                }
            }
            return Err(error.into());
        }
    } else {
        if let Err(error) = state
            .business
            .live_source_service
            .set_manual_latch(body.room_id, true)
            .await
        {
            if let Some(guard) = guard {
                if let Err(rollback_error) = guard.rollback().await {
                    tracing::warn!(%rollback_error, "直播源版本守卫回滚失败");
                }
            }
            return Err(error.into());
        }
    }
    let job = match state.media.live_recorder.request_stop(body.room_id).await {
        Ok(job) => job,
        Err(error) => {
            if let Some(source) = source_before.clone() {
                if source.manual_stop_session_key.is_some() {
                    if let Err(restore_error) = state
                        .business
                        .live_source_service
                        .set_manual_stop_session(body.room_id, source.manual_stop_session_key)
                        .await
                    {
                        tracing::warn!(
                            room_id = body.room_id,
                            %restore_error,
                            "恢复手动停止会话状态失败"
                        );
                    }
                } else if let Err(restore_error) = state
                    .business
                    .live_source_service
                    .set_manual_latch(body.room_id, source.manual_stop_latched)
                    .await
                {
                    tracing::warn!(room_id = body.room_id, %restore_error, "恢复手动停止闩锁失败");
                }
            }
            if let Some(guard) = guard {
                if let Err(rollback_error) = guard.rollback().await {
                    tracing::warn!(%rollback_error, "直播源版本守卫回滚失败");
                }
            }
            return Err(AppError::BadRequest(
                crate::services::live_recorder::ffmpeg_session::redact_diagnostics(
                    &error.to_string(),
                ),
            ));
        }
    };
    if let Some(guard) = guard {
        guard.commit();
    }
    Ok(Json(ApiResponse::with_message(
        json!({"operation_id": job.id, "recording_id": job.recording_id, "status": job.status, "progress": job.progress}),
        "停止请求已接受，正在后台收尾与合并",
    )))
}

pub(super) async fn recording_status(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let sessions = state.media.live_recorder.status_all().await;
    Ok(Json(ApiResponse::success(
        json!({"count": sessions.len(), "sessions": sessions}),
    )))
}
