//! 重试退避与任务缓存管理：内存指数退避、退避持久化与 bvid 级缓存清理。

use crate::models::download_task;
use chrono::Local;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use super::{backoff_key, task_cache_key, DownloadManager, RetryBackoff};

impl DownloadManager {
    /// 持久化任务的错误分类，并清除待重试时间（用于不可重试错误终止自动重试）。
    pub(super) async fn persist_task_error_kind(&self, bvid: &str, task_type: &str, kind: &str) {
        if let Ok(Some(task)) = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(download_task::Column::TaskType.eq(task_type))
            .one(&self.db)
            .await
        {
            let mut model: download_task::ActiveModel = task.into();
            model.error_kind = Set(Some(kind.to_string()));
            model.next_retry_at = Set(None);
            if let Err(e) = model.update(&self.db).await {
                warn!("持久化错误分类失败 {bvid} ({task_type}): {e}");
            }
        }
    }

    /// 把内存退避同步回数据库，使进程重启后仍能向用户说明等待原因。
    pub(super) async fn persist_retry_schedule(
        &self,
        bvid: &str,
        cid: Option<i64>,
        task_type: &str,
        error_kind: &str,
    ) {
        let key = backoff_key(bvid, cid, task_type);
        let Some(backoff) = self.retry_backoff.lock().await.get(&key).cloned() else {
            return;
        };
        let seconds = backoff
            .next_retry_at
            .saturating_duration_since(Instant::now())
            .as_secs() as i64;
        let next = Local::now() + chrono::Duration::seconds(seconds);
        if let Ok(Some(task)) = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(match cid {
                Some(cid) => download_task::Column::Cid.eq(cid),
                None => download_task::Column::Cid.is_null(),
            })
            .filter(download_task::Column::TaskType.eq(task_type))
            .one(&self.db)
            .await
        {
            let mut model: download_task::ActiveModel = task.into();
            model.attempts = Set(backoff.attempts as i32);
            model.next_retry_at = Set(Some(next));
            model.error_kind = Set(Some(error_kind.to_string()));
            model.priority = Set(200);
            if let Err(e) = model.update(&self.db).await {
                warn!("持久化重试计划失败 {bvid} ({task_type}): {e}");
            }
        }
    }

    /// 清理指定分P任务的所有内存缓存：进度缓存、重试退避、合并标记。
    /// 在重下/重试/移除任务时调用，避免旧的脏数据污染新一轮下载。
    /// `cid` 为 None（单P）时按 bvid 键清理；多P时按 `{bvid}#{cid}` 键隔离清理。
    pub(super) async fn cleanup_task_caches(&self, bvid: &str, cid: Option<i64>) {
        let cache_key = task_cache_key(bvid, cid);
        // 1. 进度缓存（按 task_cache_key 索引）
        self.progress_cache.lock().await.remove(&cache_key);
        // 2. 重试退避（按 `{cache_key}_{task_type}` 索引，需扫描所有 task_type）
        {
            let mut backoff = self.retry_backoff.lock().await;
            let prefix = format!("{cache_key}_");
            let keys_to_remove: Vec<String> = backoff
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            for k in keys_to_remove {
                backoff.remove(&k);
            }
        }
        // 3. 合并标记（按 task_cache_key 索引，使用 poison-safe 锁）
        {
            let mut guard = self.lock_merge_set();
            guard.remove(&cache_key);
        }
    }

    /// 检查任务是否处于重试退避期。返回 Some(剩余秒数) 表示需要等待。
    pub(super) async fn check_backoff(&self, key: &str) -> Option<u64> {
        let backoff = self.retry_backoff.lock().await;
        if let Some(entry) = backoff.get(key) {
            let now = Instant::now();
            if now < entry.next_retry_at {
                return Some(entry.next_retry_at.duration_since(now).as_secs().max(1));
            }
        }
        None
    }

    /// 更新重试退避状态。成功后清除记录，失败后增加退避时间（指数退避）。
    pub(super) async fn update_backoff(&self, key: &str, success: bool) {
        let mut backoff = self.retry_backoff.lock().await;
        if success {
            backoff.remove(key);
        } else {
            let attempts = backoff.get(key).map(|e| e.attempts).unwrap_or(0) + 1;
            let delay_secs = (2_u64.pow(attempts.min(6))).min(3600); // 最大 1 小时
            backoff.insert(
                key.to_string(),
                RetryBackoff {
                    attempts,
                    next_retry_at: Instant::now() + Duration::from_secs(delay_secs),
                },
            );
            debug!(
                operation = "download_backoff",
                retry_count = attempts,
                retry_delay_secs = delay_secs,
                "下载任务进入重试退避"
            );
        }
    }
}
