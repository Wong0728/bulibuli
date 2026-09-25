//! 下载任务队列操作接口：状态/健康度/指标查询、优先级、重试、移除与暂停恢复。

use super::common::{
    commit_task_guards, guard_task_batch, outcome_to_response, rollback_task_guards,
};
use crate::error::{ApiResponse, AppError};
use crate::models::operation_log::OperationTarget;
use crate::services::download::RetryTrigger;
use crate::state::SharedState;
use axum::{extract::Query, extract::State, Json};
use serde::Deserialize;
use serde_json::Value;
use tracing::error;

pub(crate) async fn queue_metrics(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    Ok(Json(ApiResponse::success(
        state.media.download_manager.queue_metrics().await?,
    )))
}

pub(crate) async fn get_health(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    Ok(Json(ApiResponse::success(
        state.media.download_manager.get_health().await,
    )))
}

#[derive(Deserialize)]
pub(crate) struct PriorityRequest {
    bvid: String,
    #[serde(rename = "type")]
    task_type: String,
    priority: i32,
    #[serde(default)]
    expected_version: Option<i32>,
}

pub(crate) async fn set_priority(
    State(state): State<SharedState>,
    Json(request): Json<PriorityRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let guards = guard_task_batch(
        &state,
        &request.bvid,
        &request.task_type,
        request.expected_version,
    )
    .await?;
    let result = state
        .media
        .download_manager
        .set_priority(&request.bvid, &request.task_type, request.priority)
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()));
    match result {
        Ok(result) => {
            commit_task_guards(guards);
            Ok(Json(ApiResponse::success(result)))
        }
        Err(error) => {
            rollback_task_guards(guards).await;
            Err(error)
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct RetryDownloadRequest {
    bvid: String,
    #[serde(rename = "type")]
    task_type: String,
    #[serde(default)]
    expected_version: Option<i32>,
}

pub(crate) async fn retry_download(
    State(state): State<SharedState>,
    Json(req): Json<RetryDownloadRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!(
        "[API] /api/download/retry 请求: bvid={}, type={}",
        req.bvid, req.task_type
    );

    let guards = guard_task_batch(&state, &req.bvid, &req.task_type, req.expected_version).await?;
    let outcome = state
        .media
        .download_manager
        .retry_task(&req.bvid, &req.task_type, RetryTrigger::Manual)
        .await;

    let outcome = match outcome {
        Ok(outcome) => {
            if outcome.ok {
                commit_task_guards(guards);
            } else {
                rollback_task_guards(guards).await;
            }
            outcome
        }
        Err(error) => {
            rollback_task_guards(guards).await;
            return Err(error.into());
        }
    };

    if outcome.ok {
        info!(
            "[API] /api/download/retry 成功: bvid={}, type={}",
            req.bvid, req.task_type
        );
    } else {
        info!(
            "[API] /api/download/retry 拒绝: bvid={}, type={}, message={}",
            req.bvid, req.task_type, outcome.message
        );
    }

    outcome_to_response(outcome)
}

#[derive(Deserialize)]
pub(crate) struct RetryAllQuery {
    since: Option<i64>,
}

pub(crate) async fn retry_all(
    State(state): State<SharedState>,
    Query(q): Query<RetryAllQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!("[API] /api/download/retry-all 请求 since={:?}", q.since);
    let outcome = state
        .media
        .download_manager
        .retry_all_failed(q.since)
        .await?;

    info!(
        "[API] /api/download/retry-all 完成: message={}",
        outcome.message
    );

    outcome_to_response(outcome)
}

#[derive(Deserialize)]
pub(crate) struct RemoveDownloadRequest {
    bvid: String,
    #[serde(rename = "type")]
    task_type: String,
    #[serde(default)]
    expected_version: Option<i32>,
}

pub(crate) async fn remove_download(
    State(state): State<SharedState>,
    Json(req): Json<RemoveDownloadRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!(
        "[API] /api/download/remove 请求: bvid={}, type={}",
        req.bvid, req.task_type
    );

    let guards = guard_task_batch(&state, &req.bvid, &req.task_type, req.expected_version).await?;
    let outcome = state
        .media
        .download_manager
        .remove_task(&req.bvid, &req.task_type)
        .await;

    let outcome = match outcome {
        Ok(outcome) => {
            if outcome.ok {
                commit_task_guards(guards);
            } else {
                rollback_task_guards(guards).await;
            }
            outcome
        }
        Err(error) => {
            rollback_task_guards(guards).await;
            return Err(error.into());
        }
    };

    info!(
        "[API] /api/download/remove 完成: bvid={}, type={}",
        req.bvid, req.task_type
    );

    outcome_to_response(outcome)
}

#[derive(Deserialize)]
pub(crate) struct StatusQuery {
    uid: Option<String>,
}

pub(crate) async fn get_status(
    State(state): State<SharedState>,
    Query(q): Query<StatusQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    Ok(Json(ApiResponse::success(
        state
            .media
            .download_manager
            .get_status(q.uid.as_deref())
            .await?,
    )))
}

/// 暂停/恢复请求体：`task_id` 缺省时表示全局操作。
/// 与 retry/remove 不同，pause/resume 用 task_id 而非 (bvid, task_type)，
/// 因为单次操作只针对一个任务行（不展开多 P）。
#[derive(Deserialize)]
pub(crate) struct PauseResumeRequest {
    pub task_id: Option<i32>,
    #[serde(default)]
    pub expected_version: Option<i32>,
}

pub(crate) async fn pause_download(
    State(state): State<SharedState>,
    Json(req): Json<PauseResumeRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!("[API] /api/download/pause 请求: task_id={:?}", req.task_id);
    let guard = if let Some(task_id) = req.task_id {
        Some(
            state
                .infra
                .conflict_guard
                .check_and_bump(
                    OperationTarget::Task,
                    &task_id.to_string(),
                    req.expected_version,
                )
                .await?,
        )
    } else if req.expected_version.is_some() {
        return Err(AppError::BadRequest(
            "全局暂停不支持单任务 expected_version".to_string(),
        ));
    } else {
        None
    };
    let outcome = state
        .media
        .download_manager
        .pause_task(req.task_id)
        .await
        .map_err(|e| {
            error!("/api/download/pause 暂停下载任务失败: {e}");
            AppError::from(e)
        });
    let outcome = match outcome {
        Ok(outcome) => {
            if let Some(guard) = guard {
                if outcome.ok {
                    guard.commit();
                } else {
                    rollback_task_guards(vec![guard]).await;
                }
            }
            outcome
        }
        Err(error) => {
            if let Some(guard) = guard {
                rollback_task_guards(vec![guard]).await;
            }
            return Err(error);
        }
    };
    if outcome.ok {
        info!("[API] /api/download/pause 完成: {}", outcome.message);
    } else {
        info!("[API] /api/download/pause 拒绝: {}", outcome.message);
    }
    outcome_to_response(outcome)
}

pub(crate) async fn resume_download(
    State(state): State<SharedState>,
    Json(req): Json<PauseResumeRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!("[API] /api/download/resume 请求: task_id={:?}", req.task_id);
    let guard = if let Some(task_id) = req.task_id {
        Some(
            state
                .infra
                .conflict_guard
                .check_and_bump(
                    OperationTarget::Task,
                    &task_id.to_string(),
                    req.expected_version,
                )
                .await?,
        )
    } else if req.expected_version.is_some() {
        return Err(AppError::BadRequest(
            "全局恢复不支持单任务 expected_version".to_string(),
        ));
    } else {
        None
    };
    let outcome = state
        .media
        .download_manager
        .resume_task(req.task_id)
        .await
        .map_err(|e| {
            error!("/api/download/resume 恢复下载任务失败: {e}");
            AppError::from(e)
        });
    let outcome = match outcome {
        Ok(outcome) => {
            if let Some(guard) = guard {
                if outcome.ok {
                    guard.commit();
                } else {
                    rollback_task_guards(vec![guard]).await;
                }
            }
            outcome
        }
        Err(error) => {
            if let Some(guard) = guard {
                rollback_task_guards(vec![guard]).await;
            }
            return Err(error);
        }
    };
    if outcome.ok {
        info!("[API] /api/download/resume 完成: {}", outcome.message);
    } else {
        info!("[API] /api/download/resume 拒绝: {}", outcome.message);
    }
    outcome_to_response(outcome)
}
