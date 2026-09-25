//! 录制 worker：单场直播的事件循环。扇入命令、弹幕事件与健康检查tick，
//! 统一驱动失败标记、进程监督与收尾。

use super::danmu::DanmuCollectorEvent;
use super::merge_jobs::persist_merge_job;
use super::segments::total_file_size;
use super::startup::{disk_gate, DiskGate};
use super::stream_url::is_expiring_soon;
use super::{
    RecordingWorker, SessionCommand, CHECKPOINT_INTERVAL, CONSERVATIVE_URL_REFRESH,
    HEALTH_CHECK_INTERVAL, MAX_RECORDING_FILE_SIZE, RECOVERY_ATTEMPTS_RESET_AFTER,
    STOP_REASON_MANUAL, STOP_REASON_OFFLINE_END, URL_REFRESH_MARGIN_SECS,
};
use std::time::Duration;
use tokio::time::{interval, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

impl RecordingWorker {
    pub(super) async fn run(mut self) {
        let mut health_tick = interval(HEALTH_CHECK_INTERVAL);
        let mut refresh_requested = false;
        // 资源保护阈值在会话开始时从设置读取，运行中修改设置只影响新会话
        let live_limits = self.settings_service.current().live.clone();
        let min_free_bytes = live_limits.min_free_space_gib * 1024 * 1024 * 1024;
        let max_duration = chrono::Duration::hours(live_limits.max_duration_hours as i64);
        let mut refresh_failures = 0usize;
        let mut next_refresh_retry = Instant::now();
        // 磁盘可用空间探测失败只提示一次，避免 2 秒一次的健康检查刷日志
        let mut disk_probe_warned = false;

        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    match command {
                        Some(SessionCommand::Stop(reply)) => {
                            self.stop_requested = true;
                            self.stop_reason = Some(STOP_REASON_MANUAL);
                            let result = self.finalize(None).await;
                            // worker 自行清理会话条目：stop() 只等 reply 30 秒，
                            // 而 finalize 含分段合并可达数分钟，超时路径无人清理。
                            // 调用方成功路径的 remove 与此幂等。
                            self.sessions.lock().await.remove(&self.room_id);
                            if reply.send(result).is_err() {
                                debug!(
                                    room_id = self.room_id,
                                    "停止结果回送失败：调用方已放弃等待"
                                );
                            }
                            return;
                        }
                        Some(SessionCommand::StopBackground { job_id, reply }) => {
                            self.stop_requested = true;
                            self.stop_reason = Some(STOP_REASON_MANUAL);
                            let job_snapshot = if let Some(job) = self.merge_jobs.lock().await.get_mut(&job_id) {
                                job.source_segment_count = self.segments.len();
                                job.updated_at = chrono::Utc::now().to_rfc3339();
                                Some(job.clone())
                            } else {
                                None
                            };
                            if let Some(job) = job_snapshot {
                                if let Err(error) = persist_merge_job(&self.db, &job).await {
                                    warn!(job_id = %job_id, "persist merge job failed: {error}");
                                }
                            }
                            let merge_cancellation = self
                                .merge_cancellations
                                .lock()
                                .await
                                .get(&job_id)
                                .cloned()
                                .unwrap_or_else(CancellationToken::new);
                            self.merge_cancellations
                                .lock()
                                .await
                                .insert(job_id.clone(), merge_cancellation.clone());
                            let merge_cancellations = self.merge_cancellations.clone();
                            let sessions = self.sessions.clone();
                            let room_id = self.room_id;
                            let background_job_id = job_id.clone();
                            // 带 panic 兜底：收尾任务 panic 时 Stopping 条目无人清理，
                            // 该房间会永久报"已在录制中"（条目存在期间无法再次录制）。
                            let panic_sessions = sessions.clone();
                            let rejected_merge_cancellations = merge_cancellations.clone();
                            let rejected_sessions = sessions.clone();
                            let rejected_job_id = background_job_id.clone();
                            let background_tasks = self.background_tasks.clone();
                            // 合并任务行的状态由 finalize 统一登记与关闭，
                            // 这里不再重复写终态，避免把失败的合并翻成 completed。
                            let accepted = background_tasks.spawn_with_panic_and_budget(
                                "live_recorder_stop_background",
                                async move {
                                    if let Err(error) =
                                        self.finalize(Some(&merge_cancellation)).await
                                    {
                                        warn!(
                                            room_id,
                                            "后台停止的录制收尾失败: {error}"
                                        );
                                    }
                                    merge_cancellations.lock().await.remove(&background_job_id);
                                    sessions.lock().await.remove(&room_id);
                                },
                                move || async move {
                                    panic_sessions.lock().await.remove(&room_id);
                                },
                                super::FINALIZE_SHUTDOWN_BUDGET,
                            );
                            if !accepted {
                                // 未启动的收尾保持 queued 行：下次启动 restore_merge_jobs
                                // 续跑合并，比在这里判失败并丢弃分段更有价值。
                                warn!(room_id, "应用正在关闭，直播收尾改由下次启动续跑");
                                rejected_merge_cancellations
                                    .lock()
                                    .await
                                    .remove(&rejected_job_id);
                                rejected_sessions.lock().await.remove(&room_id);
                            }
                            if reply.send(job_id).is_err() {
                                debug!(
                                    room_id,
                                    "后台停止回执发送失败：调用方已放弃等待"
                                );
                            }
                            return;
                        }
                        None => {
                            // 命令通道关闭（所有 handle 已释放）：同样清理会话条目。
                            self.sessions.lock().await.remove(&self.room_id);
                            return;
                        }
                    }
                }
                reload = self.reload_rx.recv(), if self.reload_channel_open => {
                    if reload.is_some() {
                        refresh_requested = true;
                    } else {
                        // writer 任务结束后关闭该分支，避免 recv(None) 在 select 中忙循环。
                        self.reload_channel_open = false;
                    }
                }
                failure = self.danmu_failure_rx.recv(), if self.danmu_failure_channel_open => {
                    match failure {
                        Some(error) => self.mark_danmu_unavailable(format!("弹幕文件写入失败: {error}")).await,
                        None => self.danmu_failure_channel_open = false,
                    }
                }
                collector = self.danmu_collector_rx.recv(), if self.danmu_collector_channel_open => {
                    match collector {
                        Some(DanmuCollectorEvent::Failed(error)) => {
                            self.mark_danmu_unavailable(format!("弹幕采集任务异常退出: {error}")).await;
                        }
                        Some(DanmuCollectorEvent::Exited) if !self.collector_cancel.is_cancelled() => {
                            self.mark_danmu_unavailable("弹幕采集任务意外退出".to_string()).await;
                        }
                        Some(DanmuCollectorEvent::Exited) => {}
                        None => self.danmu_collector_channel_open = false,
                    }
                }
                _ = health_tick.tick() => {
                    // spawn_blocking：fs2 是同步系统调用，Windows 网络盘可能阻塞 runtime 线程
                    let live_dir = self.live_dir.clone();
                    let probe = tokio::task::spawn_blocking(move || fs2::available_space(&live_dir))
                        .await
                        .ok()
                        .and_then(|result| result.ok());
                    match disk_gate(probe, min_free_bytes) {
                        DiskGate::Ok => {}
                        DiskGate::ProbeFailed => {
                            // 探测失败不等于空间不足：权限、临时 IO 错误、网络盘抖动都会
                            // 让 available_space 报错，按 0 处理会停掉本来能正常录的场次。
                            // 这里继续录制，真写满时由 FFmpeg 异常退出路径收尾；只提示一次。
                            if !disk_probe_warned {
                                disk_probe_warned = true;
                                warn!(
                                    room_id = self.room_id,
                                    "磁盘可用空间探测失败，本次录制不再以空间阈值停录，写满时按进程异常处理"
                                );
                            }
                        }
                        DiskGate::BelowThreshold => {
                            self.mark_failure(format!(
                                "可用磁盘空间低于 {} GiB 安全阈值，已安全停录",
                                live_limits.min_free_space_gib
                            ))
                            .await;
                            if let Err(error) = self.finalize(None).await {
                                warn!(room_id = self.room_id, "录制收尾失败: {error}");
                            }
                            self.sessions.lock().await.remove(&self.room_id);
                            return;
                        }
                    }
                    if total_file_size(&self.segments).await >= MAX_RECORDING_FILE_SIZE {
                        self.mark_failure("recording file size limit reached".to_owned()).await;
                        if let Err(error) = self.finalize(None).await {
                            warn!(room_id = self.room_id, "录制收尾失败: {error}");
                        }
                        self.sessions.lock().await.remove(&self.room_id);
                        return;
                    }
                    if self.observe_process().await {
                        refresh_requested = false;
                        if self.failure.is_some()
                            || self.stop_reason == Some(STOP_REASON_OFFLINE_END)
                        {
                            if let Err(error) = self.finalize(None).await {
                                warn!(room_id = self.room_id, "录制收尾失败: {error}");
                            }
                            self.sessions.lock().await.remove(&self.room_id);
                            return;
                        }
                    } else if self.current_ffmpeg.is_some()
                        && (is_expiring_soon(&self.current_url, URL_REFRESH_MARGIN_SECS)
                            || self.last_url_refresh.elapsed() >= CONSERVATIVE_URL_REFRESH)
                    {
                        refresh_requested = true;
                    }

                    if refresh_requested
                        && self.current_ffmpeg.is_some()
                        && Instant::now() >= next_refresh_retry
                    {
                        match self.refresh_segment().await {
                            Ok(()) => {
                                refresh_requested = false;
                                refresh_failures = 0;
                                next_refresh_retry = Instant::now();
                            }
                            Err(error) => {
                                // refresh_segment 会先停止旧进程，再启动下一分段。
                                // 将失败保留为异常退出恢复状态，避免下一次健康检查把
                                // 没有 FFmpeg 进程的会话继续视为活动状态。
                                self.unexpected_exit_detail = Some(format!(
                                    "刷新直播流分段失败: {error}"
                                ));
                                refresh_failures = refresh_failures.saturating_add(1);
                                let backoff = match refresh_failures {
                                    1 => Duration::from_secs(5),
                                    2 => Duration::from_secs(15),
                                    _ => Duration::from_secs(30),
                                };
                                next_refresh_retry = Instant::now() + backoff;
                                warn!(
                                    room_id = self.room_id,
                                    ?backoff,
                                    "直播流地址刷新失败，将重试: {error}"
                                );
                            }
                        }
                    }
                    self.update_snapshot().await;
                    // 恢复成功且新分段稳定录制足够久后，重置恢复额度与退避：
                    // 否则长时间直播中累计 3 次瞬时断流就会被永久判死。
                    if self.restart_attempts > 0
                        && self.current_ffmpeg.is_some()
                        && self.last_url_refresh.elapsed() >= RECOVERY_ATTEMPTS_RESET_AFTER
                    {
                        self.restart_attempts = 0;
                        self.next_recovery_at = None;
                    }
                    if self.last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                        let snapshot = self.snapshot.lock().await.clone();
                        self.persist_recording(&snapshot, false).await;
                        self.last_checkpoint = Instant::now();
                    }
                    if chrono::Utc::now().signed_duration_since(self.started_at)
                        > max_duration
                    {
                        self.mark_failure(format!(
                            "录制超过 {} 小时安全上限，已停止",
                            live_limits.max_duration_hours
                        ))
                        .await;
                        if let Err(error) = self.finalize(None).await {
                            warn!(room_id = self.room_id, "录制收尾失败: {error}");
                        }
                        // 与磁盘不足/文件超限/异常退出路径一致：移除会话条目，
                        // 否则房间会占用并发额度且无法再次录制，直到进程重启。
                        self.sessions.lock().await.remove(&self.room_id);
                        return;
                    }
                }
            }
        }
    }
}
