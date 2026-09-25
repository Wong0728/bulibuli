//! monitor_loop 的处理分支：单任务 aria2 状态轮询判定、磁盘满/恢复策略与
//! aria2 持续不可用时的批量失败收敛。轮询主体见 [`DownloadManager::monitor_loop`]。

use crate::domain::{DownloadStage, DownloadStatus};
use crate::models::download_task;
use crate::services::aria2::Aria2Status;
use sea_orm::Set;
use serde_json::json;
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, info, warn};

use super::completion::CompleteOutcome;
use super::{task_cache_key, DownloadManager, ProgressCache};

/// 一轮轮询中跨任务累积的待落库/待广播/待完成项，
/// 由 [`DownloadManager::poll_gid_tasks`] 填充、monitor_loop 统一提交。
pub(super) struct PollAccumulators {
    pub to_update: Vec<(String, i32, i64, download_task::ActiveModel)>,
    /// GID 失效被判失败的任务：待 guarded 写库确认后补发一次 failed 进度广播
    pub gid_lost_failures: Vec<download_task::Model>,
    pub deferred_progress: Vec<(download_task::Model, String, i32, i64, i64, i64)>,
    pub completed_bvids: Vec<(String, Option<i64>, Option<String>)>,
}

/// 磁盘满/恢复事件的跨轮询状态（monitor_loop 持有，逐轮传回）。
#[derive(Default)]
pub(super) struct DiskGuardState {
    pub disk_full_notified: bool,
    /// 磁盘满时被自动暂停的任务 ID：恢复后据此自动续传。
    pub disk_paused_ids: Vec<i32>,
}

impl DownloadManager {
    /// 磁盘空间事件处理：写满时逐任务暂停并广播事件；恢复时广播恢复事件并
    /// 自动续传此前因磁盘满被暂停的任务（后台执行，不阻塞监控循环）。
    /// 返回 `true` 表示磁盘仍处于写满状态，monitor_loop 应跳过本轮剩余处理。
    pub(super) async fn handle_disk_pressure(
        &self,
        all_tasks: &[download_task::Model],
        disk_space_error: Option<&str>,
        disk: &mut DiskGuardState,
    ) -> bool {
        if let Some(error) = disk_space_error {
            // 逐任务暂停（不调全局 pauseAll）：与逐任务路径并发会互相误判
            for task in all_tasks {
                if let Some(gid) = &task.gid {
                    if let Err(pause_error) = self.pause_gid_with_fallback(gid).await {
                        warn!("磁盘空间不足时暂停 aria2 任务 gid={gid} 失败: {pause_error}");
                    }
                }
                if self
                    .state_service
                    .transition(
                        task.id,
                        task.generation,
                        DownloadStatus::Paused,
                        DownloadStage::Transferring,
                    )
                    .await
                    .is_ok()
                {
                    disk.disk_paused_ids.push(task.id);
                } else {
                    warn!(task_id = task.id, "磁盘空间不足时持久化暂停状态失败");
                }
            }
            let mut native = self.native_tasks.lock().await;
            for task in all_tasks {
                if let Some(token) = native.remove(&task.id) {
                    token.cancel();
                }
            }
            if !disk.disk_full_notified {
                if let Err(notify_error) = self
                    .ws
                    .broadcast_system(
                        "download:disk-full",
                        json!({ "message": error }),
                        crate::ws::NoticeAudience::CanAct,
                    )
                    .await
                {
                    warn!("推送磁盘空间不足事件失败: {notify_error}");
                }
                disk.disk_full_notified = true;
            }
            return true;
        }
        if disk.disk_full_notified {
            if let Err(error) = self
                .ws
                .broadcast_system(
                    "download:disk-recovered",
                    json!({}),
                    crate::ws::NoticeAudience::CanAct,
                )
                .await
            {
                warn!("推送磁盘恢复事件失败: {error}");
            }
            disk.disk_full_notified = false;
            // 磁盘恢复：自动续传因磁盘满被暂停的任务（后台执行，不阻塞监控循环）。
            let ids = std::mem::take(&mut disk.disk_paused_ids);
            if !ids.is_empty() {
                info!("磁盘空间已恢复，自动续传 {} 个任务", ids.len());
                let manager = self.clone();
                let cancellation = self.cancellation.clone();
                let handle = tokio::spawn(async move {
                    for id in ids {
                        tokio::select! {
                            _ = cancellation.cancelled() => break,
                            result = manager.resume_task(Some(id)) => {
                                if let Err(error) = result {
                                    debug!(task_id = id, "磁盘恢复后自动续传未完成: {error}");
                                }
                            }
                        }
                    }
                });
                *self.disk_resume_handle.lock().await = Some(handle);
            }
        }
        false
    }

    /// aria2 持续不可用（超过宽限窗口）时，将所有 GID 任务标记为失败并收敛终态。
    pub(super) async fn fail_all_tasks_when_aria2_unavailable(
        &self,
        tasks: &[download_task::Model],
    ) {
        for task in tasks {
            let mut model: download_task::ActiveModel = task.clone().into();
            model.status = Set("failed".to_string());
            model.error = Set(Some("Aria2 下载器不可用".to_string()));
            model.speed = Set(0);
            // generation 守卫：避免覆盖用户重试后重置的新状态。
            let updated = self
                .apply_guarded_update(&task.bvid, task.id, task.generation, model)
                .await;
            // 任务终态：清理 DB 节流与进度缓存
            self.progress_cache
                .lock()
                .await
                .remove(&task_cache_key(&task.bvid, task.cid));
            if updated {
                self.broadcast_progress(
                    task,
                    "failed",
                    task.progress_percent,
                    task.downloaded_size,
                    task.total_size,
                    0,
                    Some("Aria2 下载器不可用"),
                )
                .await;
                // 终态收敛：与 queue_terminal_failure 同口径，
                // 否则记录停在 downloading，看板三个 Tab 都不可见。
                self.cleanup_failed_transfer_leftovers(task).await;
                self.mark_history_failed(&task.bvid, task.cid, &task.task_type)
                    .await;
            }
        }
    }

    /// 逐任务轮询 aria2 状态并判定动作：进度落库、状态推进、失败终态与完成收集。
    /// 只累积变更到 `acc`，由 monitor_loop 统一提交（guarded 写库 + 广播 + 完成流程）。
    pub(super) async fn poll_gid_tasks(
        &self,
        tasks: &[download_task::Model],
        aria_statuses: &mut HashMap<i32, anyhow::Result<Aria2Status>>,
        gidless_since: &mut HashMap<i32, Instant>,
        status_failures: &mut HashMap<i32, u8>,
        acc: &mut PollAccumulators,
    ) {
        for task in tasks {
            let Some(gid) = &task.gid else {
                // gid 缺失：add_task_inner 先插行、投递 aria2 成功后才回写 gid，
                // 正常窗口不足 1 秒；若投递失败会留下 gid=NULL 的行，
                // 缺少引擎进度超过 60 秒后判失败，避免任务永久悬空。
                let first_seen = *gidless_since.entry(task.id).or_insert_with(Instant::now);
                if first_seen.elapsed() >= std::time::Duration::from_secs(60) {
                    gidless_since.remove(&task.id);
                    warn!("任务 {} 超过 60 秒无 aria2 GID，标记为失败", task.bvid);
                    self.queue_terminal_failure(
                        task,
                        "任务缺少 aria2 GID，无法监控（投递可能失败）".to_string(),
                        &mut acc.to_update,
                    )
                    .await;
                }
                continue;
            };
            gidless_since.remove(&task.id);
            {
                let Some(status_result) = aria_statuses.remove(&task.id) else {
                    continue;
                };
                match status_result {
                    Ok(status) => {
                        status_failures.remove(&task.id);
                        let mut model: download_task::ActiveModel = task.clone().into();
                        // 更新内存进度缓存，减少数据库查询压力（按分P粒度隔离）
                        {
                            let mut cache = self.progress_cache.lock().await;
                            cache.insert(
                                task_cache_key(&task.bvid, task.cid),
                                ProgressCache {
                                    progress_percent: status.progress_percent,
                                    downloaded_size: status.downloaded_size,
                                    total_size: status.total_size,
                                    speed: status.speed,
                                },
                            );
                        }

                        match status.status.as_str() {
                            "complete" => {
                                match self.handle_complete(task, &status).await {
                                    CompleteOutcome::Skip { clear_throttle } => {
                                        if clear_throttle {
                                            self.progress_cache
                                                .lock()
                                                .await
                                                .remove(&task_cache_key(&task.bvid, task.cid));
                                        }
                                        continue;
                                    }
                                    CompleteOutcome::Finished { uid } => {
                                        // 任务已终态，清理 DB 写入节流缓存，避免长期运行后 HashMap 无限增长
                                        self.progress_cache
                                            .lock()
                                            .await
                                            .remove(&task_cache_key(&task.bvid, task.cid));
                                        // 添加到下载历史并触发音视频合并检查
                                        acc.completed_bvids.push((
                                            task.bvid.clone(),
                                            task.cid,
                                            uid,
                                        ));
                                    }
                                }
                            }
                            "error" => {
                                if task.status != "failed" {
                                    info!(
                                        "[DownloadManager] 下载失败: {} ({}) 错误: {}",
                                        task.bvid,
                                        task.task_type,
                                        status.error_message.as_deref().unwrap_or("未知错误")
                                    );
                                    // 失败计入坏 CDN 熔断（两次即熔断 10 分钟），
                                    // 重试重新解析 URL 时 choose_url 会避开该 host
                                    if let Some(host) = task
                                        .url
                                        .as_deref()
                                        .and_then(|u| url::Url::parse(u).ok())
                                        .and_then(|u| u.host_str().map(str::to_owned))
                                    {
                                        self.bili_api.bad_cdns().record_failure(&host).await;
                                    }
                                    // 手动下载任务不携带 UID，避免混入博主自动监测日志。
                                    let uid = if task.source.as_deref() == Some("manual") {
                                        None
                                    } else {
                                        self.get_blogger_uid_from_history(&task.bvid).await
                                    };
                                    self.log_bvid(
                                        &task.bvid,
                                        uid.as_deref(),
                                        &format!(
                                            "下载失败（{}）：{}",
                                            task.task_type,
                                            status.error_message.as_deref().unwrap_or("未知错误")
                                        ),
                                        "error",
                                    )
                                    .await;
                                }
                                self.queue_terminal_failure(
                                    task,
                                    status
                                        .error_message
                                        .clone()
                                        .unwrap_or_else(|| "未知下载错误".to_string()),
                                    &mut acc.to_update,
                                )
                                .await;
                            }
                            "active" => {
                                let filename_changed = !status.filename.is_empty()
                                    && status.filename != "Unknown"
                                    && task.filename.as_deref() != Some(&status.filename);
                                if filename_changed {
                                    model.filename = Set(Some(status.filename.clone()));
                                }
                                if task.status != "downloading" || filename_changed {
                                    if task.status != "downloading" {
                                        info!(
                                            "[DownloadManager] 开始下载: {} ({})",
                                            task.bvid, task.task_type
                                        );
                                    }
                                    model.status = Set("downloading".to_string());
                                    acc.to_update.push((
                                        task.bvid.clone(),
                                        task.id,
                                        task.generation,
                                        model,
                                    ));
                                }
                                acc.deferred_progress.push((
                                    task.clone(),
                                    "downloading".to_string(),
                                    status.progress_percent,
                                    status.downloaded_size,
                                    status.total_size,
                                    status.speed,
                                ));
                            }
                            "waiting" if task.status != "pending" => {
                                info!(
                                    "[DownloadManager] 任务等待中: {} ({})",
                                    task.bvid, task.task_type
                                );
                                model.status = Set("pending".to_string());
                                acc.to_update.push((
                                    task.bvid.clone(),
                                    task.id,
                                    task.generation,
                                    model,
                                ));
                                acc.deferred_progress.push((
                                    task.clone(),
                                    "pending".to_string(),
                                    task.progress_percent,
                                    task.downloaded_size,
                                    task.total_size,
                                    0,
                                ));
                            }
                            "paused" if task.status != "paused" => {
                                info!(
                                    "[DownloadManager] 任务已暂停: {} ({})",
                                    task.bvid, task.task_type
                                );
                                model.status = Set("paused".to_string());
                                acc.to_update.push((
                                    task.bvid.clone(),
                                    task.id,
                                    task.generation,
                                    model,
                                ));
                                acc.deferred_progress.push((
                                    task.clone(),
                                    "paused".to_string(),
                                    task.progress_percent,
                                    task.downloaded_size,
                                    task.total_size,
                                    0,
                                ));
                            }
                            "waiting" | "paused" => {}
                            "stopped" | "removed" => {
                                // 任务被停止或移除，标记为失败
                                if task.status != "failed" {
                                    info!(
                                        "[DownloadManager] 任务停止: {} ({}) 状态: {}",
                                        task.bvid, task.task_type, status.status
                                    );
                                }
                                self.queue_terminal_failure(
                                    task,
                                    format!("任务已停止 (aria2 状态: {})", status.status),
                                    &mut acc.to_update,
                                )
                                .await;
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        // GID 已失效（下载器重启后 session 丢失/记录被清理）是永久性
                        // 错误：立即判失败并走既有自动重试机制（重新解析 URL 重下），
                        // 不再按瞬时故障累计 3 次，避免每 2 秒刷一遍无效轮询告警。
                        if matches!(
                            e.downcast_ref::<crate::services::aria2::Aria2Error>(),
                            Some(crate::services::aria2::Aria2Error::GidNotFound(_))
                        ) {
                            status_failures.remove(&task.id);
                            info!(
                                "[DownloadManager] aria2 已丢失任务 {} 的 GID={gid}，标记为失败等待自动重试",
                                task.bvid
                            );
                            self.queue_terminal_failure(
                                task,
                                "aria2 已丢失该下载任务（GID 失效，可能因程序重启）".to_string(),
                                &mut acc.to_update,
                            )
                            .await;
                            acc.gid_lost_failures.push(task.clone());
                            continue;
                        }
                        let failures = status_failures.entry(task.id).or_default();
                        *failures = failures.saturating_add(1);
                        warn!("获取 aria2 状态失败 (gid={gid}, attempt={failures}): {e}");
                        if *failures >= 3 {
                            status_failures.remove(&task.id);
                            self.queue_terminal_failure(
                                task,
                                format!("Aria2 状态连续 3 次查询失败: {e}"),
                                &mut acc.to_update,
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }
}
