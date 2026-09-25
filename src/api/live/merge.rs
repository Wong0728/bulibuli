//! 直播录制合并任务接口：重试合并、按任务号查询进度与请求取消。

use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde_json::{json, Value};

/// 对指定录制重新发起合并（合并失败后的重试入口）。
pub(super) async fn start_merge(
    State(state): State<SharedState>,
    axum::extract::Path(recording_id): axum::extract::Path<i32>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
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

pub(super) async fn merge_job(
    State(state): State<SharedState>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let job = state
        .media
        .live_recorder
        .merge_job(&job_id)
        .await
        .ok_or_else(|| AppError::NotFound("合并任务不存在".into()))?;
    Ok(Json(ApiResponse::success(json!(job))))
}

pub(super) async fn cancel_merge(
    State(state): State<SharedState>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
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
