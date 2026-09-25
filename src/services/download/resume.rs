//! 恢复控制：单任务/全局恢复、aria2 gid 状态复核与派发失败时的 paused 回滚。

use crate::domain::{DownloadStage, DownloadStatus};
use crate::models::download_task;
use anyhow::{anyhow, Result};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use tracing::{error, info, warn};

use super::engine::TransferEngine;
use super::{DownloadManager, TaskOutcome};

/// 同 pause 侧：`unpause` 对"已非 paused"（active/waiting/complete）报
/// cannot be unpaused now，属于重复恢复而非真故障。
fn is_idempotent_unpause_err(e: &anyhow::Error) -> bool {
    e.to_string().contains("cannot be unpaused now")
}

impl DownloadManager {
    /// 恢复下载任务。
    /// - `task_id = None`：全局恢复（逐任务重新调度所有 paused 任务）。
    /// - `task_id = Some(id)`：仅恢复指定任务。仅 paused 可恢复。
    ///
    /// 恢复时重新解析 URL（B 站 CDN URL 带 deadline 签名，过期会 403），
    /// 然后重新 dispatch_transfer 进入引擎调度。aria2 路径天然支持断点续传；
    /// 原生路径当前为整段重下（不依赖 .part 续传）。
    pub async fn resume_task(&self, task_id: Option<i32>) -> Result<TaskOutcome> {
        match task_id {
            Some(id) => self.resume_single_task(id).await,
            None => self.resume_all_tasks().await,
        }
    }

    async fn resume_single_task(&self, task_id: i32) -> Result<TaskOutcome> {
        let task = download_task::Entity::find_by_id(task_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| anyhow!("任务 {task_id} 不存在"))?;

        if task.status != "paused" {
            return Ok(TaskOutcome::rejected(format!(
                "当前状态为 {}，仅已暂停可恢复",
                task.status
            )));
        }

        let download_id = task.id;
        match self.resume_task_inner(&task).await? {
            true => Ok(TaskOutcome::accepted("已恢复下载任务", download_id)),
            false => Ok(TaskOutcome::rejected(
                "恢复失败：无法解析下载链接或投递引擎失败",
            )),
        }
    }

    async fn resume_all_tasks(&self) -> Result<TaskOutcome> {
        // 不调全局 unpauseAll：与逐任务 resume 并发会产生
        // "cannot be unpaused now" 误判，统一走逐任务路径（DB 为事实源）
        let tasks = download_task::Entity::find()
            .filter(download_task::Column::Status.eq("paused"))
            .all(&self.db)
            .await?;

        let mut count = 0usize;
        let mut failed = 0usize;
        for task in &tasks {
            match self.resume_task_inner(task).await {
                Ok(true) => count += 1,
                Ok(false) => failed += 1,
                Err(e) => {
                    warn!("恢复任务 {} 失败: {e}", task.bvid);
                    failed += 1;
                }
            }
        }

        info!("[DownloadManager] 全局恢复完成，恢复 {count} 个，失败 {failed} 个");
        Ok(TaskOutcome::done(format!(
            "已恢复 {count} 个暂停任务，{failed} 个失败"
        )))
    }

    /// 恢复单个 paused 任务的内部实现。
    /// - aria2 路径：先 tellStatus 校验 gid 当前状态，再决定动作：
    ///   paused → unpause（保留断点续传控制文件 .aria2）；active/waiting/complete →
    ///   无需 unpause，直接落库 downloading（complete 由 monitor 轮询补走完成流程）；
    ///   gid 已失效（aria2 重启后 session 丢失）→ 降级为重新解析 URL + 重新 dispatch。
    /// - native 路径：始终重新解析 URL + 重新 dispatch（native 不支持 .part 续传）。
    ///
    /// 返回 `true` 表示已成功投递至引擎；`false` 表示 URL 解析或投递失败。
    async fn resume_task_inner(&self, task: &download_task::Model) -> Result<bool> {
        let cookies = self
            .settings_service
            .cookie_header()
            .await
            .unwrap_or_default();

        // 路径一：原 aria2 任务，gid 仍可能在 aria2 session 中
        if let Some(gid) = &task.gid {
            match self.aria2.get_download_status(gid).await {
                Ok(status) => match status.status.as_str() {
                    // 已在传输或已完成：无需 unpause，直接落库 downloading；
                    // waiting 由 monitor 归一为 pending，complete 由 monitor 走完成流程
                    "active" | "waiting" | "complete" => {
                        let updated = self
                            .state_service
                            .transition(
                                task.id,
                                task.generation,
                                DownloadStatus::Downloading,
                                DownloadStage::Transferring,
                            )
                            .await
                            .map_err(|e| anyhow!(e.to_string()))?;
                        let mut model: download_task::ActiveModel = updated.into();
                        model.speed = Set(0);
                        model.error = Set(None);
                        model.next_retry_at = Set(None);
                        if let Err(e) = model.update(&self.db).await {
                            error!("恢复任务 {} 时持久化状态失败: {e}", task.bvid);
                            return Ok(false);
                        }
                        self.queue_notify.notify_one();
                        info!(
                            "[DownloadManager] aria2 任务 {} ({}) 已处于 {} 状态，直接落库恢复",
                            task.bvid, task.task_type, status.status
                        );
                        return Ok(true);
                    }
                    // 仍是 paused：unpause 恢复传输
                    "paused" => {
                        let updated = self
                            .state_service
                            .transition(
                                task.id,
                                task.generation,
                                DownloadStatus::Downloading,
                                DownloadStage::Transferring,
                            )
                            .await
                            .map_err(|e| anyhow!(e.to_string()))?;
                        if let Err(e) = self.aria2.unpause(gid).await {
                            // "cannot be unpaused now"：状态已变（幂等场景），复核后按实际状态处理
                            if !is_idempotent_unpause_err(&e) {
                                // 复核：若实际已在传输/完成仍算恢复成功，避免 DB 回滚与 aria2 分叉
                                match self.aria2.get_download_status(gid).await {
                                    Ok(recheck)
                                        if matches!(
                                            recheck.status.as_str(),
                                            "active" | "waiting" | "complete"
                                        ) => {}
                                    _ => {
                                        warn!("恢复 aria2 任务 gid={gid} 失败: {e}");
                                        if let Err(rollback_error) = self
                                            .state_service
                                            .transition(
                                                task.id,
                                                updated.generation,
                                                DownloadStatus::Paused,
                                                DownloadStage::Transferring,
                                            )
                                            .await
                                        {
                                            // 回滚失败会让 DB 停在 downloading 而 aria2 已暂停，
                                            // 留痕便于排查状态分叉
                                            warn!(
                                                "恢复失败后回滚任务 {} 为 paused 失败: {rollback_error}",
                                                task.bvid
                                            );
                                        }
                                        return Ok(false);
                                    }
                                }
                            }
                        }
                        let mut model: download_task::ActiveModel = updated.into();
                        model.speed = Set(0);
                        model.error = Set(None);
                        model.next_retry_at = Set(None);
                        if let Err(e) = model.update(&self.db).await {
                            error!("恢复任务 {} 时持久化状态失败: {e}", task.bvid);
                            if let Err(pause_error) = self.aria2.pause(gid).await {
                                warn!("持久化失败后暂停 aria2 任务 gid={gid} 失败: {pause_error}");
                            }
                            return Ok(false);
                        }
                        self.queue_notify.notify_one();
                        info!(
                            "[DownloadManager] 已恢复 aria2 任务: {} ({})",
                            task.bvid, task.task_type
                        );
                        return Ok(true);
                    }
                    // error 等异常态：走下面的重建路径
                    _ => {
                        warn!(
                            "aria2 gid={} 状态异常（{}），将重新解析 URL 并重建任务",
                            gid, status.status
                        );
                    }
                },
                // gid 已失效：走下面的重建路径
                Err(_) => {
                    warn!("aria2 gid={} 已失效，将重新解析 URL 并重建任务", gid);
                }
            }
        }

        // 路径二：重建（native 必走；aria2 gid 失效时降级走）
        // B 站 m4s/m4a 的 CDN URL 带 deadline 签名（约 2h），暂停时间过长会过期，必须重新解析。
        let url = match self.resolve_resume_url(task).await {
            Some(u) if !u.is_empty() => u,
            _ => {
                warn!("恢复任务 {}：无法解析下载链接，保持暂停状态", task.bvid);
                return Ok(false);
            }
        };

        let engine = self.select_engine().await;
        let dir = self.task_download_dir(task).await;
        let filename = task
            .filename
            .clone()
            .unwrap_or_else(|| format!("{}.{}", task.bvid, task.task_type));

        let updated = self
            .state_service
            .transition(
                task.id,
                task.generation,
                DownloadStatus::Downloading,
                DownloadStage::Transferring,
            )
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        // 先持久化状态，再派发外部引擎；派发失败时恢复 paused，避免 DB 与引擎分叉。
        let (gid, permit) = match self
            .dispatch_transfer(engine, task.id, &url, &task.bvid, &cookies, &dir, &filename)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if let Err(rollback_error) = self
                    .state_service
                    .transition(
                        task.id,
                        updated.generation,
                        DownloadStatus::Paused,
                        DownloadStage::Transferring,
                    )
                    .await
                {
                    warn!(
                        "恢复任务 {} 派发失败后回滚为 paused 失败: {rollback_error}",
                        task.bvid
                    );
                }
                return Err(error);
            }
        };
        let runtime_gid = gid.clone();

        // 更新 gid / 清除 error / next_retry_at
        let updated_generation = updated.generation;
        let mut model: download_task::ActiveModel = updated.into();
        if let Some(g) = gid {
            model.gid = Set(Some(g));
        }
        model.url = Set(Some(url.clone()));
        model.speed = Set(0);
        model.error = Set(None);
        model.next_retry_at = Set(None);
        if let Err(e) = model.update(&self.db).await {
            error!("恢复任务 {} 时持久化状态失败: {e}", task.bvid);
            if let Some(gid) = runtime_gid.as_deref() {
                if let Err(remove_error) = self.aria2.remove(gid).await {
                    warn!("回滚 Aria2 任务失败 gid={gid}: {remove_error}");
                }
            }
            if let Err(rollback_error) = self
                .state_service
                .transition(
                    task.id,
                    updated_generation,
                    DownloadStatus::Paused,
                    DownloadStage::Transferring,
                )
                .await
            {
                warn!(
                    "恢复任务 {} 回滚为 paused 失败: {rollback_error}",
                    task.bvid
                );
            }
            return Ok(false);
        }

        // Native 路径需重新 spawn 传输（aria2 路径已通过 dispatch 进入调度）
        if engine == TransferEngine::Native {
            let uid = if task.source.as_deref() == Some("manual") {
                None
            } else {
                self.get_blogger_uid_from_history(&task.bvid).await
            };
            if let Err(error) = self
                .spawn_native_transfer(task.id, &url, &cookies, uid.as_deref(), permit)
                .await
            {
                if let Err(rollback_error) = self
                    .state_service
                    .transition(
                        task.id,
                        updated_generation,
                        DownloadStatus::Paused,
                        DownloadStage::Transferring,
                    )
                    .await
                {
                    warn!(
                        "恢复任务 {} 启动原生传输失败后回滚为 paused 失败: {rollback_error}",
                        task.bvid
                    );
                }
                return Err(error);
            }
        }
        // aria2 路径：permit 在此处 Drop 释放（aria2 自行管理并发上限）

        // 唤醒 monitor_loop 空闲退避，立即开始监控恢复后的任务
        self.queue_notify.notify_one();

        info!(
            "[DownloadManager] 已恢复任务（重建路径）: {} ({})",
            task.bvid, task.task_type
        );
        Ok(true)
    }
}
