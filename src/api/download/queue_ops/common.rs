//! 队列操作子模块的共享工具：响应信封转换与批量乐观锁守卫。

use crate::error::{ApiResponse, AppError};
use crate::models::{download_task, operation_log::OperationTarget};
use crate::services::conflict_guard::ConflictGuard;
use crate::services::download::TaskOutcome;
use crate::state::SharedState;
use axum::Json;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{json, Value};

/// 把任务入队/重试/移除的强类型结果转成前端信封。
/// `ok=false` 是业务拒绝（重复任务、退避中、非法参数），映射为 400 让前端走失败提示，
/// 与旧 `{success:false}` 契约经中间件归一为 code=400 的行为一致。
pub(super) fn outcome_to_response(
    outcome: TaskOutcome,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if outcome.ok {
        Ok(Json(ApiResponse::with_message(
            json!({ "download_id": outcome.download_id }),
            outcome.message,
        )))
    } else {
        Err(AppError::BadRequest(outcome.message))
    }
}

pub(super) async fn rollback_task_guards(guards: Vec<ConflictGuard>) {
    for guard in guards {
        if let Err(error) = guard.rollback().await {
            tracing::error!("下载任务版本回滚失败: {error}");
        }
    }
}

pub(super) fn commit_task_guards(guards: Vec<ConflictGuard>) {
    for guard in guards {
        guard.commit();
    }
}

/// 下载队列的 bvid/type 操作可能同时覆盖多个分P；把它们作为一个版本快照校验。
pub(super) async fn guard_task_batch(
    state: &SharedState,
    bvid: &str,
    task_type: &str,
    expected_version: Option<i32>,
) -> Result<Vec<ConflictGuard>, AppError> {
    let tasks = download_task::Entity::find()
        .filter(download_task::Column::Bvid.eq(bvid))
        .filter(download_task::Column::TaskType.eq(task_type))
        .all(&state.infra.db)
        .await?;
    if tasks.is_empty() {
        return Err(AppError::NotFound("未找到下载任务".to_string()));
    }
    let mut guards = Vec::with_capacity(tasks.len());
    for task in tasks {
        match state
            .infra
            .conflict_guard
            .check_and_bump(
                OperationTarget::Task,
                &task.id.to_string(),
                expected_version,
            )
            .await
        {
            Ok(guard) => guards.push(guard),
            Err(error) => {
                rollback_task_guards(guards).await;
                return Err(error);
            }
        }
    }
    Ok(guards)
}
