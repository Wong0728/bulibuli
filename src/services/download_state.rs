use crate::domain::{DownloadStage, DownloadStatus};
use crate::error::{AppError, AppResult};
use crate::models::download_task;
use crate::services::db_operation::DbOperationGuard;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
    TransactionTrait,
};
use std::str::FromStr;

#[derive(Clone)]
pub struct DownloadStateService {
    db: DatabaseConnection,
}

impl DownloadStateService {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    pub async fn transition(
        &self,
        task_id: i32,
        expected_generation: i64,
        next: DownloadStatus,
        stage: DownloadStage,
    ) -> AppResult<download_task::Model> {
        let transaction = self.db.begin().await?;
        let mut guard = DbOperationGuard::new("download_state_transition");
        let task = download_task::Entity::find_by_id(task_id)
            .one(&transaction)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("download task {task_id}")))?;
        if task.generation != expected_generation {
            return Err(AppError::Conflict("stale task generation".to_string()));
        }
        let current = DownloadStatus::from_str(&task.status)
            .map_err(|message| AppError::Conflict(message.to_string()))?;
        if !current.can_transition_to(&next) {
            return Err(AppError::Conflict(format!(
                "illegal download transition: {current:?} -> {next:?}"
            )));
        }
        let bump_generation = matches!(next, DownloadStatus::Paused | DownloadStatus::Retrying)
            || matches!(current, DownloadStatus::Paused) && next == DownloadStatus::Downloading;
        let mut active: download_task::ActiveModel = task.into();
        active.status = Set(status_name(&next).to_string());
        active.stage = Set(stage_name(&stage).to_string());
        if bump_generation {
            active.generation = Set(expected_generation + 1);
            active.completion_triggered = Set(false);
        }
        let updated = active.update(&transaction).await?;
        transaction.commit().await?;
        guard.commit();
        Ok(updated)
    }

    pub async fn complete_once(&self, task_id: i32, expected_generation: i64) -> AppResult<bool> {
        let result = download_task::Entity::update_many()
            .col_expr(
                download_task::Column::Status,
                sea_orm::sea_query::Expr::value("completed"),
            )
            .col_expr(
                download_task::Column::Stage,
                sea_orm::sea_query::Expr::value("finalizing"),
            )
            .col_expr(
                download_task::Column::CompletionTriggered,
                sea_orm::sea_query::Expr::value(true),
            )
            .filter(download_task::Column::Id.eq(task_id))
            .filter(download_task::Column::Generation.eq(expected_generation))
            .filter(download_task::Column::CompletionTriggered.eq(false))
            .exec(&self.db)
            .await?;
        Ok(result.rows_affected == 1)
    }
}

fn status_name(status: &DownloadStatus) -> &'static str {
    match status {
        DownloadStatus::Pending => "pending",
        DownloadStatus::Downloading => "downloading",
        DownloadStatus::Paused => "paused",
        DownloadStatus::Retrying => "retrying",
        DownloadStatus::Merging => "merging",
        DownloadStatus::Completed => "completed",
        DownloadStatus::Failed => "failed",
        DownloadStatus::Cancelled => "cancelled",
    }
}

fn stage_name(stage: &DownloadStage) -> &'static str {
    match stage {
        DownloadStage::Queued => "queued",
        DownloadStage::Resolving => "resolving",
        DownloadStage::Transferring => "transferring",
        DownloadStage::Muxing => "muxing",
        DownloadStage::Finalizing => "finalizing",
        DownloadStage::Done => "done",
    }
}
