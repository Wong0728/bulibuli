//! 任务重试与移除：手动/自动重试入口（退避与重试额度管理）、任务移除时的
//! 引擎回收与缓存清理，以及入队占位 history 记录的回滚清理。

use crate::domain::{DownloadStage, DownloadStatus};
use crate::models::{download_task, history};
use anyhow::Result;
use chrono::{DateTime, Local};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use tracing::warn;

use super::queue::page_info_from_task;
use super::{backoff_key, task_cache_key, DownloadManager, PageInfo, RetryTrigger, TaskOutcome};

impl DownloadManager {
    /// 回滚手动任务时同步清理入队时写入的 history 占位记录：
    /// 占位行 state=pending 且无完成时间，残留会让看板「下载中」
    /// 永远挂着一张不会推进的卡片。
    pub(super) async fn cleanup_history_placeholder(&self, bvid: &str, cid: Option<i64>) {
        let mut query = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .filter(history::Column::State.eq("pending"))
            .filter(history::Column::DownloadTime.is_null());
        query = match cid {
            Some(cid) => query.filter(history::Column::Cid.eq(cid)),
            None => query.filter(history::Column::Cid.is_null()),
        };
        let rows = match query.all(&self.db).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("查询 history 占位记录失败 {bvid}: {e}");
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        if let Err(e) = history::Entity::delete_many()
            .filter(history::Column::Id.is_in(rows.iter().map(|h| h.id)))
            .exec(&self.db)
            .await
        {
            warn!("清理 history 占位记录失败 {bvid}: {e}");
        }
    }

    pub async fn retry_task(
        &self,
        bvid: &str,
        task_type: &str,
        trigger: RetryTrigger,
    ) -> Result<TaskOutcome> {
        // 多P：同一 (bvid, task_type) 可能对应多个分P任务，逐个重试；单P时恰为一行，行为不变。
        let tasks = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(download_task::Column::TaskType.eq(task_type))
            .all(&self.db)
            .await?;
        if tasks.is_empty() {
            return Ok(TaskOutcome::rejected("未找到任务"));
        }
        // 重试使用当前登录态。
        let cookies = self
            .settings_service
            .cookie_header()
            .await
            .unwrap_or_default();
        let uid = self.get_blogger_uid_from_history(bvid).await;
        let mut last: Option<TaskOutcome> = None;
        for task in tasks {
            // 退避键按分P隔离：单P为 `{bvid}_{task_type}`（不变），多P为 `{bvid}#{cid}_{task_type}`。
            let key = backoff_key(bvid, task.cid, task_type);
            if let Some(wait_secs) = self.check_backoff(&key).await {
                last = Some(TaskOutcome::rejected(format!(
                    "重试过于频繁，请 {wait_secs} 秒后再试"
                )));
                continue;
            }
            if task.status == "failed" {
                self.state_service
                    .transition(
                        task.id,
                        task.generation,
                        DownloadStatus::Retrying,
                        DownloadStage::Resolving,
                    )
                    .await?;
                let mut retrying: download_task::ActiveModel = task.clone().into();
                retrying.priority = Set(200);
                // 人工重试重新发放额度并立即再试；自动重试只累加计数并保留 video_retry
                // 刚写入的退避时间，否则计数被抹平、退避退化成一轮 30 秒的紧密循环。
                retrying.attempts = Set(next_attempts(trigger, task.attempts));
                retrying.next_retry_at = Set(match trigger {
                    RetryTrigger::Manual => None,
                    RetryTrigger::Auto => task.next_retry_at,
                });
                retrying.update(&self.db).await?;
            }
            let url = self
                .resolve_resume_url(&task)
                .await
                .filter(|value| !value.is_empty())
                .or_else(|| task.url.clone())
                .unwrap_or_default();
            if url.is_empty() {
                last = Some(TaskOutcome::rejected("无法解析下载链接"));
                continue;
            }
            // 重试保留任务原有来源，避免手动任务重试后日志混入博主日志
            let task_source = task.source.clone().unwrap_or_else(|| "auto".to_string());
            let page_info: Option<PageInfo> = page_info_from_task(&task);
            let result = self
                .add_task(
                    bvid,
                    task.title.as_deref().unwrap_or(bvid),
                    &url,
                    &cookies,
                    task.quality,
                    task_type,
                    uid.as_deref(),
                    &task_source,
                    page_info.as_ref(),
                    None,
                )
                .await?;
            let success = result.ok;
            self.update_backoff(&key, success).await;
            if !success {
                self.persist_retry_schedule(bvid, task.cid, task_type, "transient")
                    .await;
            }
            last = Some(result);
        }
        Ok(last.unwrap_or_else(|| TaskOutcome::rejected("未找到任务")))
    }

    pub async fn retry_all_failed(&self, since: Option<i64>) -> Result<TaskOutcome> {
        let mut query =
            download_task::Entity::find().filter(download_task::Column::Status.eq("failed"));
        if let Some(ts) = since {
            let since_dt: DateTime<Local> = DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.into())
                .unwrap_or_else(|| Local::now() - chrono::Duration::days(1));
            query = query.filter(download_task::Column::UpdatedAt.gte(since_dt));
        }
        let tasks = query.all(&self.db).await?;
        // 批量重试统一使用当前登录态。
        let cookies = self
            .settings_service
            .cookie_header()
            .await
            .unwrap_or_default();
        let mut count = 0;
        let mut skipped_count = 0;
        let mut failed_count = 0;
        for task in tasks {
            if task.url.is_some() {
                let key = backoff_key(&task.bvid, task.cid, &task.task_type);
                if self.check_backoff(&key).await.is_some() {
                    skipped_count += 1;
                    continue;
                }
                let uid = self.get_blogger_uid_from_history(&task.bvid).await;
                let task_source = task.source.clone().unwrap_or_else(|| "auto".to_string());
                let page_info = page_info_from_task(&task);
                let url = self
                    .resolve_resume_url(&task)
                    .await
                    .filter(|value| !value.is_empty())
                    .or_else(|| task.url.clone());
                let Some(url) = url else {
                    failed_count += 1;
                    continue;
                };
                let result = self
                    .add_task(
                        &task.bvid,
                        task.title.as_deref().unwrap_or(&task.bvid),
                        &url,
                        &cookies,
                        task.quality,
                        &task.task_type,
                        uid.as_deref(),
                        &task_source,
                        page_info.as_ref(),
                        None,
                    )
                    .await?;
                let success = result.ok;
                self.update_backoff(&key, success).await;
                if !success {
                    self.persist_retry_schedule(&task.bvid, task.cid, &task.task_type, "transient")
                        .await;
                }
                if success {
                    count += 1;
                } else {
                    failed_count += 1;
                }
            }
        }
        Ok(TaskOutcome::done(format!(
            "已重试 {count} 个失败的下载任务，{skipped_count} 个因退避跳过，{failed_count} 个重试失败"
        )))
    }

    pub async fn remove_task(&self, bvid: &str, task_type: &str) -> Result<TaskOutcome> {
        // 多P：同一 (bvid, task_type) 可能对应多个分P任务，需逐行移除；单P时恰为一行，行为不变。
        let tasks = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(download_task::Column::TaskType.eq(task_type))
            .all(&self.db)
            .await?;
        let mut touched_cids: Vec<Option<i64>> = Vec::new();
        for task in tasks {
            if !touched_cids.contains(&task.cid) {
                touched_cids.push(task.cid);
            }
            // 原生兜底任务：取消传输（后台任务感知取消后自行退出，不再写终态）
            if let Some(token) = self.native_tasks.lock().await.remove(&task.id) {
                token.cancel();
            }
            if let Some(gid) = &task.gid {
                if let Err(error) = self.aria2.remove(gid).await {
                    warn!("移除 aria2 任务失败 gid={gid}: {error}");
                }
            }
            download_task::Entity::delete_by_id(task.id)
                .exec(&self.db)
                .await?;
            // 清理关联的内存缓存：避免移除后残留的进度/退避/合并标记影响后续操作
            // 注：retry_backoff 按 `{cache_key}_{task_type}` 索引（单P为 `{bvid}_{task_type}`），需精确移除
            {
                let mut backoff = self.retry_backoff.lock().await;
                backoff.remove(&format!("{}_{}", task_cache_key(bvid, task.cid), task_type));
            }
        }
        // 入队时写入的 history 占位行（state=pending、无完成时间）随任务一并回收，
        // 否则 cleanup_history 的排序会让它永远落在删除窗口之外，成为只增不减的
        // 孤儿行。仅回收占位行，已完成的真实记录不受影响。
        for cid in touched_cids {
            self.cleanup_history_placeholder(bvid, cid).await;
        }
        // 若该 bvid 下已无任何任务，则一并清理 bvid 级缓存
        let remaining = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .count(&self.db)
            .await?;
        if remaining == 0 {
            // 进度缓存按 task_cache_key 索引：单P键为 bvid，多P键为 `{bvid}#{cid}`，统一按前缀清理。
            {
                let mut cache = self.progress_cache.lock().await;
                let prefix = format!("{bvid}#");
                let keys: Vec<String> = cache
                    .keys()
                    .filter(|k| k.as_str() == bvid || k.starts_with(&prefix))
                    .cloned()
                    .collect();
                for k in keys {
                    cache.remove(&k);
                }
            }
            {
                let mut guard = self.lock_merge_set();
                let prefix = format!("{bvid}#");
                let keys: Vec<String> = guard
                    .iter()
                    .filter(|k| k.as_str() == bvid || k.starts_with(&prefix))
                    .cloned()
                    .collect();
                for k in keys {
                    guard.remove(&k);
                }
            }
        }
        Ok(TaskOutcome::done("已移除下载记录"))
    }
}

/// 本次重试后任务应持有的自动重试计数。
///
/// 曾经无条件在重新派发时清零，导致 `video_retry` 的 `MAX_VIDEO_RETRIES` 永不触顶，
/// 永久失败的任务每 30 秒无限重打 B 站接口。
fn next_attempts(trigger: RetryTrigger, current: i32) -> i32 {
    match trigger {
        RetryTrigger::Manual => 0,
        RetryTrigger::Auto => current.saturating_add(1),
    }
}

#[cfg(test)]
mod tests {
    use super::next_attempts;
    use super::RetryTrigger;

    #[test]
    fn auto_retry_accumulates_until_it_reaches_the_cap() {
        // 自动重试必须单调累加，否则 MAX_VIDEO_RETRIES(3) 永不触顶。
        let mut attempts = 0;
        for expected in [1, 2, 3] {
            attempts = next_attempts(RetryTrigger::Auto, attempts);
            assert_eq!(attempts, expected);
        }
        assert!(attempts >= 3, "触顶后 video_retry 应停止自动重试");
    }

    #[test]
    fn manual_retry_rearms_the_budget() {
        assert_eq!(next_attempts(RetryTrigger::Manual, 9), 0);
    }
}
