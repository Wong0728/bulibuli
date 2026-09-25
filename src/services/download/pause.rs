//! 暂停控制：单任务/全局暂停、幂等判定与 aria2 pause 降级策略。

use crate::domain::{DownloadStage, DownloadStatus};
use crate::models::download_task;
use anyhow::{anyhow, Result};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use tracing::{debug, info, warn};

use super::{DownloadManager, TaskOutcome};

/// aria2 幂等性错误判定：`pause` 对"已是 paused"报 cannot be paused now，
/// 属于重复暂停而非真故障，应视为成功照常落库。
fn is_idempotent_pause_err(e: &anyhow::Error) -> bool {
    e.to_string().contains("cannot be paused now")
}

impl DownloadManager {
    /// 暂停下载任务。
    /// - `task_id = None`：全局暂停（逐任务暂停并持久化所有运行态任务为 paused）。
    /// - `task_id = Some(id)`：仅暂停指定任务。仅运行态（`MONITOR_STATUSES`：
    ///   downloading/pending/retrying）可暂停，其他状态返回可读拒绝信息。
    ///
    /// 可暂停集合与 `MONITOR_STATUSES` 同集：状态机里恰好这三个非终态
    /// 允许转入 paused（merging 由完成链路驱动、不可暂停）。
    ///
    /// 暂停后：速度归零、generation+=1（防陈旧回调覆盖）；释放并发额度
    /// （native 路径 token 取消后 spawn 退出自动 Drop permit；aria2 路径不持有 permit）。
    /// 程序重启后 paused 任务不在 `resume_pending_tasks` 查询范围内，保持暂停状态。
    pub async fn pause_task(&self, task_id: Option<i32>) -> Result<TaskOutcome> {
        match task_id {
            Some(id) => self.pause_single_task(id).await,
            None => self.pause_all_tasks().await,
        }
    }

    /// aria2 任务暂停：先优雅 `pause`，失败时降级 `forcePause`。
    /// 两者都失败说明 gid 已失效或 aria2 异常，调用方不得将任务落库为 paused
    ///（否则 monitor_loop 不再轮询该 gid，形成"实际仍在下载但状态为暂停"的孤儿任务）。
    pub(super) async fn pause_gid_with_fallback(&self, gid: &str) -> Result<()> {
        if let Err(e) = self.aria2.pause(gid).await {
            warn!("暂停 aria2 任务 gid={gid} 失败，尝试 forcePause: {e}");
            self.aria2.force_pause(gid).await?;
        }
        Ok(())
    }

    async fn pause_single_task(&self, task_id: i32) -> Result<TaskOutcome> {
        let task = download_task::Entity::find_by_id(task_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| anyhow!("任务 {task_id} 不存在"))?;

        // 仅运行态（downloading/pending/retrying）可暂停；paused 重复暂停视为成功（幂等）
        let status = task.status.as_str();
        if status == "paused" {
            return Ok(TaskOutcome::done("任务已处于暂停状态"));
        }
        if !DownloadStatus::MONITOR_STATUSES.contains(&status) {
            return Ok(TaskOutcome::rejected(format!(
                "当前状态为 {status}，仅下载中/等待中/重试中可暂停"
            )));
        }

        // Native：取消传输（spawn 感知取消后自行退出并删除 .downloading 临时文件，
        // 不再写终态）
        // retrying 的 gid 是上次失败后的残值（新 gid 与 downloading 同刻写回），对已停止
        // 的 gid 调 pause 必然报错，会把可暂停的任务误计成"暂停失败"而跳过落库；
        // downloading（active）与 pending（aria2 waiting，见 monitor 归一）才可能有存活传输。
        let live_gid = task.gid.as_ref().filter(|_| status != "retrying");
        if let Some(token) = self.native_tasks.lock().await.remove(&task.id) {
            token.cancel();
        } else if let Some(gid) = live_gid {
            // aria2 任务：pause 失败降级 forcePause；"cannot be paused now"
            // 表示任务已被暂停（幂等场景），视为成功照常落库 paused；
            // 其余失败则不落库，避免 monitor_loop 停止轮询后形成孤儿任务
            if let Err(e) = self.pause_gid_with_fallback(gid).await {
                if is_idempotent_pause_err(&e) {
                    debug!("aria2 任务 gid={gid} 已处于暂停状态");
                } else {
                    warn!("暂停 aria2 任务 gid={gid} 失败: {e}");
                    return Ok(TaskOutcome::rejected(format!(
                        "暂停任务 {} 失败: {e}",
                        task.bvid
                    )));
                }
            }
        }

        // 落库：status=paused，generation+=1（防 stale 回调覆盖）
        let updated = self
            .state_service
            .transition(
                task.id,
                task.generation,
                DownloadStatus::Paused,
                DownloadStage::Transferring,
            )
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        // 速度归零（保留 downloaded_size 供前端展示断点进度）
        let mut model: download_task::ActiveModel = updated.into();
        model.speed = Set(0);
        if let Err(e) = model.update(&self.db).await {
            warn!("持久化暂停速度归零失败 task_id={task_id}: {e}");
        }

        // 广播 paused 状态，前端立即把进度条置为已暂停
        self.broadcast_progress(
            &task,
            "paused",
            task.progress_percent,
            task.downloaded_size,
            task.total_size,
            0,
            None,
        )
        .await;

        info!(
            "[DownloadManager] 已暂停任务: {} ({})",
            task.bvid, task.task_type
        );
        Ok(TaskOutcome::done(format!("已暂停任务 {}", task.bvid)))
    }

    async fn pause_all_tasks(&self) -> Result<TaskOutcome> {
        // 全局暂停覆盖全部运行态（downloading/pending/retrying）：漏掉 retrying
        // 会让"全部暂停"管不住退避重打的任务，且重试中不产进度、用户无从中止。
        let tasks = download_task::Entity::find()
            .filter(download_task::Column::Status.is_in(DownloadStatus::MONITOR_STATUSES))
            .all(&self.db)
            .await?;

        let mut count = 0usize;
        let mut failed = 0usize;
        for task in &tasks {
            // 与 pause_single_task 同源：retrying 没有存活传输，不去碰它的残值 gid。
            let live_gid = task.gid.as_ref().filter(|_| task.status != "retrying");
            // Native：取消传输；aria2 单任务 gid 也显式 pause（pauseAll 已涵盖，这里只做兜底）
            if let Some(token) = self.native_tasks.lock().await.remove(&task.id) {
                token.cancel();
            } else if let Some(gid) = live_gid {
                // pause 失败降级 forcePause；"cannot be paused now" 表示任务已被
                // 暂停（幂等场景），视为成功照常落库；其余失败跳过该任务，不落库
                // paused（否则 monitor_loop 停止轮询后形成"实际仍在下载"的孤儿任务）
                if let Err(e) = self.pause_gid_with_fallback(gid).await {
                    if is_idempotent_pause_err(&e) {
                        debug!("全局暂停时 aria2 任务 gid={gid} 已处于暂停状态");
                    } else {
                        warn!("全局暂停时暂停 aria2 任务 gid={gid} 失败: {e}");
                        failed += 1;
                        continue;
                    }
                }
            }

            // 落库为 paused（失败则跳过该任务，继续处理其他）
            let transition_result = self
                .state_service
                .transition(
                    task.id,
                    task.generation,
                    DownloadStatus::Paused,
                    DownloadStage::Transferring,
                )
                .await;
            let updated = match transition_result {
                Ok(model) => model,
                Err(e) => {
                    warn!("全局暂停时持久化任务 {} 状态失败: {e}", task.bvid);
                    continue;
                }
            };

            let mut model: download_task::ActiveModel = updated.into();
            model.speed = Set(0);
            if let Err(e) = model.update(&self.db).await {
                warn!("全局暂停时持久化任务 {} 速度归零失败: {e}", task.bvid);
            }

            self.broadcast_progress(
                task,
                "paused",
                task.progress_percent,
                task.downloaded_size,
                task.total_size,
                0,
                None,
            )
            .await;
            count += 1;
        }

        info!("[DownloadManager] 全局暂停完成，共暂停 {count} 个任务，{failed} 个暂停失败");
        let msg = if failed > 0 {
            format!("已暂停 {count} 个下载任务，{failed} 个暂停失败")
        } else {
            format!("已暂停 {count} 个下载任务")
        };
        Ok(TaskOutcome::done(msg))
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::DownloadStatus;
    use std::str::FromStr;

    #[test]
    fn pausable_set_matches_monitor_driven_statuses() {
        // 暂停入口（单任务守卫与全局暂停查询）统一引用 MONITOR_STATUSES，
        // 一旦字面量数组与状态机的 paused 入边脱钩，重试中的任务就会逃出"全部暂停"。
        for status in DownloadStatus::MONITOR_STATUSES {
            let current = DownloadStatus::from_str(status).expect("pausable status is known");
            assert!(
                current.can_transition_to(&DownloadStatus::Paused),
                "{status} 应可暂停"
            );
        }
        for status in DownloadStatus::ACTIVE_STATUSES {
            let current = DownloadStatus::from_str(status).expect("active status is known");
            // paused 由入口的幂等分支先行返回，不占监控集合，但同样"可暂停"。
            let accepted = status == "paused" || DownloadStatus::MONITOR_STATUSES.contains(&status);
            assert_eq!(
                current.can_transition_to(&DownloadStatus::Paused),
                accepted,
                "{status} 的可暂停性与暂停入口引用的集合不一致"
            );
        }
    }
}
