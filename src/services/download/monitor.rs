//! 下载监控循环主体：轮询 aria2 状态并驱动任务状态机（generation 守卫防陈旧写入）。
//!
//! 本文件只保留轮询节奏与各分支的编排；单任务状态判定、磁盘策略与
//! aria2 不可用收敛在 [`super::monitor_poll`]，失败终态在 [`super::terminal`]，
//! generation 守卫写库在 [`super::guarded_write`]。

use crate::domain::DownloadStatus;
use crate::models::download_task;
use crate::services::file_safety::ensure_disk_space;
use futures::{stream, StreamExt};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tracing::{debug, error, info, warn};

use super::completion::CompleteOutcome;
use super::monitor_poll::{DiskGuardState, PollAccumulators};
use super::DownloadManager;

impl DownloadManager {
    /// 对账 DB 状态为 paused 但仍有 gid 的任务：若 aria2 报告该 gid 仍在
    /// 传输（active/waiting），说明此前暂停未生效，补一次 forcePause；
    /// 若该 gid 已 complete，则补走完成流程，避免下载记录永久缺失。
    async fn paused_orphan_reconcile(&self) {
        let Ok(paused_tasks) = download_task::Entity::find()
            .filter(download_task::Column::Status.eq("paused"))
            .filter(download_task::Column::Gid.is_not_null())
            .all(&self.db)
            .await
        else {
            return;
        };
        for task in paused_tasks {
            let Some(gid) = task.gid.clone() else {
                continue;
            };
            let Ok(status) = self.aria2.get_download_status(&gid).await else {
                continue;
            };
            match status.status.as_str() {
                "active" | "waiting" => {
                    warn!(
                        "paused 任务 {} 的 aria2 gid={gid} 仍在传输，强制暂停",
                        task.bvid
                    );
                    if let Err(e) = self.aria2.force_pause(&gid).await {
                        warn!("强制暂停 aria2 任务 gid={gid} 失败: {e}");
                    }
                }
                "complete" => {
                    info!(
                        "paused 任务 {} 的 aria2 gid={gid} 已完成，补写下载记录",
                        task.bvid
                    );
                    match self.handle_complete(&task, &status).await {
                        CompleteOutcome::Skip { .. } => {}
                        CompleteOutcome::Finished { uid } => {
                            if let Err(e) = self
                                .on_task_completed(&task.bvid, task.cid, uid.as_deref())
                                .await
                            {
                                error!("处理完成任务失败 {}: {e}", task.bvid);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) async fn monitor_loop(&self) {
        // gid 缺失任务的首见时间：超过宽限期仍无 gid 则判失败（防悬置）
        let mut gidless_since: HashMap<i32, Instant> = HashMap::new();
        // 活跃 2 秒 / 空闲退避 10 秒：空队列时降低查库频率，
        // 新任务入队经 queue_notify 立即唤醒，不牺牲响应速度
        const ACTIVE_TICK: std::time::Duration = std::time::Duration::from_secs(2);
        const IDLE_TICK: std::time::Duration = std::time::Duration::from_secs(10);
        let mut tick = ACTIVE_TICK;
        let mut idle_rounds: u32 = 0;
        // aria2 不可用计数器，需在循环外部声明才能正确累积
        let mut aria2_fail_count: u32 = 0;
        // aria2 本次失联起点：宽限窗口内不批量置 failed（覆盖重启抖动）
        let mut aria2_unavailable_since: Option<Instant> = None;
        let mut status_failures: HashMap<i32, u8> = HashMap::new();
        let mut disk = DiskGuardState::default();
        let mut last_audio_retry = Instant::now() - std::time::Duration::from_secs(30);
        let mut last_paused_check = Instant::now() - std::time::Duration::from_secs(30);
        let mut last_disk_check = Instant::now() - std::time::Duration::from_secs(15);
        let mut disk_space_error: Option<String> = None;

        loop {
            tokio::select! {
                _ = self.cancellation.cancelled() => break,
                _ = tokio::time::sleep(tick) => {}
                _ = self.queue_notify.notified() => {}
            }

            if last_audio_retry.elapsed() >= std::time::Duration::from_secs(30) {
                self.check_audio_retry().await;
                self.check_video_retry().await;
                last_audio_retry = Instant::now();
            }

            let all_tasks = match download_task::Entity::find()
                .filter(download_task::Column::Status.is_in(DownloadStatus::MONITOR_STATUSES))
                .all(&self.db)
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    error!("查询下载任务失败: {e}");
                    continue;
                }
            };

            // 原生任务由后台任务驱动；本循环只处理 aria2 GID 任务。
            let tasks = {
                let native = self.native_tasks.lock().await;
                all_tasks
                    .iter()
                    .filter(|t| !native.contains_key(&t.id))
                    .cloned()
                    .collect::<Vec<_>>()
            };

            debug!(
                operation = "download_queue_poll",
                queue_len = all_tasks.len(),
                aria2_queue_len = tasks.len(),
                idle_rounds,
                "下载队列轮询"
            );

            // 防泄漏：任务被外部删除时同步清掉 gid 缺失计时
            let current_ids: HashSet<i32> = tasks.iter().map(|t| t.id).collect();
            gidless_since.retain(|id, _| current_ids.contains(id));
            status_failures.retain(|id, _| current_ids.contains(id));

            // 磁盘探测与恢复必须先于空队列短路：磁盘满会把全部任务置 paused，
            // 下一轮队列为空，若先 continue，探测/恢复事件/自动续传永远走不到，
            // 用户清出空间后任务会静默停在 paused。
            if last_disk_check.elapsed() >= std::time::Duration::from_secs(15) {
                disk_space_error = ensure_disk_space(&self.paths.download_dir, None)
                    .await
                    .err()
                    .map(|error| error.to_string());
                last_disk_check = Instant::now();
            }
            if self
                .handle_disk_pressure(&all_tasks, disk_space_error.as_deref(), &mut disk)
                .await
            {
                continue;
            }

            if all_tasks.is_empty() {
                // 空队列：连续空闲 3 轮后退避到长间隔，入队通知会立即唤醒
                idle_rounds = idle_rounds.saturating_add(1);
                if idle_rounds >= 3 {
                    tick = IDLE_TICK;
                }
                continue;
            }
            idle_rounds = 0;
            tick = ACTIVE_TICK;

            let aria2_available = self.aria2.is_available().await;
            if !aria2_available {
                aria2_fail_count += 1;
                let unavailable_for = aria2_unavailable_since
                    .get_or_insert_with(Instant::now)
                    .elapsed();
                // 连续 3 次（约 6 秒）且超过宽限窗口（30 秒，覆盖一次典型
                // 重启耗时）aria2 仍不可用，才将所有下载中任务标记为失败。
                const ARIA2_FAILURE_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
                if aria2_fail_count >= 3 && unavailable_for >= ARIA2_FAILURE_GRACE {
                    warn!("Aria2 持续不可用，将 {} 个下载任务标记为失败", tasks.len());
                    self.fail_all_tasks_when_aria2_unavailable(&tasks).await;
                    aria2_fail_count = 0;
                }
                continue;
            }
            aria2_fail_count = 0;
            aria2_unavailable_since = None;

            // paused 任务对账（低频，30s 一次）：暂停 RPC 失败可能留下
            // "DB 状态为 paused 但 aria2 里仍在下载/已完成"的孤儿 gid。
            // monitor 主循环只查 MONITOR_STATUSES，不会轮询这些任务，
            // 这里补查：仍在传输则强制暂停；已 complete 则补走完成流程写历史记录。
            if last_paused_check.elapsed() >= std::time::Duration::from_secs(30) {
                last_paused_check = Instant::now();
                self.paused_orphan_reconcile().await;
            }

            let mut acc = PollAccumulators {
                to_update: Vec::new(),
                gid_lost_failures: Vec::new(),
                deferred_progress: Vec::new(),
                completed_bvids: Vec::new(),
            };
            let gid_tasks = tasks
                .iter()
                .filter_map(|task| task.gid.clone().map(|gid| (task.id, gid)))
                .collect::<Vec<_>>();
            let mut aria_statuses = stream::iter(gid_tasks)
                .map(|(task_id, gid)| async move {
                    (task_id, self.aria2.get_download_status(&gid).await)
                })
                .buffer_unordered(8)
                .collect::<HashMap<_, _>>()
                .await;

            self.poll_gid_tasks(
                &tasks,
                &mut aria_statuses,
                &mut gidless_since,
                &mut status_failures,
                &mut acc,
            )
            .await;

            // 状态/文件名批量写入与进度广播解耦：aria2 任务落库时已是 downloading，
            // 若以“本轮是否更新”为门槛，稳态下进度推送与 DB 落盘会完全停摆
            // （DB 进度从 0 直接跳 100，WS 无下载中事件）。进度提交本身已有
            // generation 守卫与合并去抖，可无条件执行。
            let updated_ids = self.apply_guarded_updates(acc.to_update).await;
            // 仅对写库成功的任务广播失败进度，避免覆盖用户重试后的新状态
            for task in &acc.gid_lost_failures {
                if !updated_ids.contains(&task.id) {
                    continue;
                }
                self.broadcast_progress(
                    task,
                    "failed",
                    task.progress_percent,
                    task.downloaded_size,
                    task.total_size,
                    0,
                    Some("aria2 已丢失该下载任务（GID 失效）"),
                )
                .await;
            }
            for (task, status, progress, downloaded, total, speed) in acc.deferred_progress {
                self.broadcast_progress(&task, &status, progress, downloaded, total, speed, None)
                    .await;
            }

            for (bvid, cid, uid) in acc.completed_bvids {
                if let Err(e) = self.on_task_completed(&bvid, cid, uid.as_deref()).await {
                    error!("处理完成任务失败 {bvid}: {e}");
                }
            }
        }
    }
}
