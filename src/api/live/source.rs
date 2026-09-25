//! 直播源管理接口：添加（含拉取主播资料）、更新与删除，均带乐观锁版本守卫。

use super::RoomBody;
use crate::error::{ApiResponse, AppError};
use crate::models::operation_log::OperationTarget;
use crate::services::live_source::{NewLiveSource, UpdateLiveSource};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub(crate) struct AddSourceBody {
    room_id: i64,
    auto_record_enabled: Option<bool>,
    weekly_schedule: Option<crate::services::live_source::WeeklySchedule>,
    capture_mode: Option<crate::services::live_source::CaptureMode>,
    max_qn: Option<i32>,
}

pub(super) async fn add_source(
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
        .get_user_info(init.uid, &cookies, false)
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
            max_qn: body
                .max_qn
                .unwrap_or(crate::services::live_source::default_max_qn()),
        })
        .await
        .map_err(|e| AppError::Conflict(e.to_string()))?;
    state.business.live_monitor.wake_room(source.room_id).await;
    Ok(Json(ApiResponse::with_message(
        json!(source),
        "直播源已添加",
    )))
}

pub(super) async fn update_source(
    State(state): State<SharedState>,
    Json(body): Json<UpdateLiveSource>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let current = state
        .business
        .live_source_service
        .find(body.room_id)
        .await?
        .ok_or_else(|| AppError::NotFound("直播源不存在".to_string()))?;
    let guard = state
        .infra
        .conflict_guard
        .check_and_bump(
            OperationTarget::LiveSource,
            &current.id.to_string(),
            body.expected_version,
        )
        .await?;
    let source = state
        .business
        .live_source_service
        .update(body)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()));
    let source = match source {
        Ok(source) => source,
        Err(error) => {
            if let Err(rollback_error) = guard.rollback().await {
                tracing::warn!(%rollback_error, "直播源版本守卫回滚失败");
            }
            return Err(error);
        }
    };
    guard.commit();
    state.business.live_monitor.wake_room(source.room_id).await;
    Ok(Json(ApiResponse::with_message(
        json!(source),
        "直播源设置已保存",
    )))
}

pub(super) async fn delete_source(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    if state
        .media
        .live_recorder
        .status(body.room_id)
        .await
        .is_some()
    {
        return Err(AppError::Conflict("请先停止当前录制再删除直播源".into()));
    }
    let current = state
        .business
        .live_source_service
        .find(body.room_id)
        .await?
        .ok_or_else(|| AppError::NotFound("直播源不存在".to_string()))?;
    let guard = state
        .infra
        .conflict_guard
        .check_and_bump(
            OperationTarget::LiveSource,
            &current.id.to_string(),
            body.expected_version,
        )
        .await?;
    let deleted = match state
        .business
        .live_source_service
        .delete(body.room_id)
        .await
    {
        Ok(deleted) => deleted,
        Err(error) => {
            if let Err(rollback_error) = guard.rollback().await {
                tracing::warn!(%rollback_error, "直播源版本守卫回滚失败");
            }
            return Err(error.into());
        }
    };
    if !deleted {
        if let Err(rollback_error) = guard.rollback().await {
            tracing::warn!(%rollback_error, "直播源版本守卫回滚失败");
        }
        return Err(AppError::NotFound("直播源不存在".to_string()));
    }
    guard.commit();
    Ok(Json(ApiResponse::success(json!({"deleted": deleted}))))
}
