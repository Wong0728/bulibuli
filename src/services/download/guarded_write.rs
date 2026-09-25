//! generation 守卫写库：仅当数据库中任务的 generation 与轮询快照一致时才写入，
//! 丢弃用户重试等操作递增 generation 之后的陈旧快照。

use crate::models::download_task;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};
use std::collections::HashSet;
use tracing::{error, warn};

use super::DownloadManager;

impl DownloadManager {
    /// 按 generation 守卫应用一次任务字段更新：仅当数据库中该任务的 generation
    /// 仍等于轮询快照时才写入。用户重试会递增 generation，此守卫用于丢弃陈旧快照。
    /// 返回是否成功写入。
    pub(super) async fn apply_guarded_update(
        &self,
        bvid: &str,
        task_id: i32,
        expected_generation: i64,
        model: download_task::ActiveModel,
    ) -> bool {
        match download_task::Entity::update_many()
            .set(model)
            .filter(download_task::Column::Id.eq(task_id))
            .filter(download_task::Column::Generation.eq(expected_generation))
            .exec(&self.db)
            .await
        {
            Ok(result) => {
                if result.rows_affected == 1 {
                    true
                } else {
                    warn!("跳过陈旧任务状态写入（generation 已变化）: {bvid}");
                    false
                }
            }
            Err(sea_orm::DbErr::RecordNotUpdated) => {
                warn!("跳过陈旧任务状态写入（generation 已变化）: {bvid}");
                false
            }
            Err(error) => {
                error!("更新下载任务失败 {bvid}: {error}");
                false
            }
        }
    }

    pub(super) async fn apply_guarded_updates(
        &self,
        updates: Vec<(String, i32, i64, download_task::ActiveModel)>,
    ) -> HashSet<i32> {
        if updates.is_empty() {
            return HashSet::new();
        }
        let mut updated_ids = HashSet::new();
        let transaction = match self.db.begin().await {
            Ok(transaction) => transaction,
            Err(error) => {
                error!("开始下载任务批量更新事务失败: {error}");
                return HashSet::new();
            }
        };
        for (bvid, task_id, generation, model) in updates {
            match download_task::Entity::update_many()
                .set(model)
                .filter(download_task::Column::Id.eq(task_id))
                .filter(download_task::Column::Generation.eq(generation))
                .exec(&transaction)
                .await
            {
                Ok(result) if result.rows_affected == 1 => {
                    updated_ids.insert(task_id);
                }
                Ok(_) => {
                    warn!("跳过陈旧任务批量写入（generation 已变化）: {bvid}");
                }
                Err(sea_orm::DbErr::RecordNotUpdated) => {
                    warn!("跳过陈旧任务状态写入（generation 已变化）: {bvid}");
                }
                Err(error) => {
                    error!("批量更新下载任务失败 {bvid}: {error}");
                    return HashSet::new();
                }
            }
        }
        if let Err(error) = transaction.commit().await {
            error!("提交下载任务批量更新事务失败: {error}");
            return HashSet::new();
        }
        updated_ids
    }
}
