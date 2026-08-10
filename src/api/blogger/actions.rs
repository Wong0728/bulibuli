use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

#[derive(Deserialize)]
pub(super) struct CleanupNowRequest {
    uid: String,
}

pub(super) async fn cleanup_now(
    State(state): State<SharedState>,
    Json(req): Json<CleanupNowRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = req.uid.trim();
    if uid.is_empty() {
        return Err(AppError::BadRequest("请提供博主UID".to_string()));
    }
    info!("[API] /api/blogger/cleanup-now 触发博主 {uid} 保留数清理");
    state
        .business
        .blogger_service
        .enforce_retain(uid)
        .await
        .map_err(|error| {
            warn!("[API] /api/blogger/cleanup-now 失败 {uid}: {error}");
            AppError::from(error)
        })?;
    Ok(Json(ApiResponse::with_message(json!({}), "整理完成")))
}

#[derive(Deserialize)]
pub(super) struct AcknowledgeRequest {
    uid: String,
}

pub(super) async fn acknowledge_profile_change(
    State(state): State<SharedState>,
    Json(req): Json<AcknowledgeRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = req.uid.trim();
    if uid.is_empty() {
        return Err(AppError::BadRequest("请提供博主UID".to_string()));
    }
    state
        .business
        .blogger_service
        .acknowledge_profile_change(uid)
        .await?;
    Ok(Json(ApiResponse::with_message(json!({}), "已确认")))
}
