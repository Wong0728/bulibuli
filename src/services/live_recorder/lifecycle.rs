//! 会话生命周期控制：启动入口（并发与重复启动守门）、停止、后台停止与关停。

use super::merge_jobs::{persist_merge_job, update_merge_job};
use super::{
    active_session_count, LiveRecorder, MergeJobInfo, RecordingInfo, RecordingStatus,
    RecordingTrigger, SessionCommand, SessionEntry, STARTING_STALE_TIMEOUT, START_GENERATION,
};
use crate::services::live_source::CaptureMode;
use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// 启动代际计数器下的快照：网络与进程启动期间以 Starting 状态阻止重复启动。
fn startup_snapshot(
    room_id: i64,
    trigger: RecordingTrigger,
    capture_mode: CaptureMode,
) -> Arc<Mutex<RecordingInfo>> {
    Arc::new(Mutex::new(RecordingInfo {
        room_id,
        recording_id: None,
        title: "正在获取直播信息".to_string(),
        status: RecordingStatus::Starting,
        output_path: String::new(),
        started_at: chrono::Utc::now().to_rfc3339(),
        duration_secs: 0,
        file_size: 0,
        error_msg: None,
        danmu_unavailable: false,
        stream_quality: None,
        stream_protocol: None,
        stream_format: None,
        stream_codec: None,
        trigger: trigger.as_str().to_owned(),
        capture_mode: capture_mode.as_str().to_owned(),
        interaction_capture_status: if capture_mode == CaptureMode::Off {
            "off"
        } else {
            "connecting"
        }
        .to_owned(),
        interaction_error: None,
        event_path: None,
        xml_path: None,
        summary_path: None,
        danmaku_count: 0,
        unique_user_count: 0,
        free_gift_count: 0,
        paid_gift_count: 0,
        sc_count: 0,
        guard_count: 0,
        peak_watched: 0,
        dropped_event_count: 0,
        estimated_paid_value: 0.0,
        last_event_seq: 0,
    }))
}

impl LiveRecorder {
    pub async fn start_with_options(
        &self,
        room_id: i64,
        trigger: RecordingTrigger,
        capture_mode: CaptureMode,
    ) -> Result<RecordingInfo> {
        if room_id <= 0 {
            return Err(anyhow!("直播间号必须为正整数"));
        }
        let max_concurrent = self.inner.settings_service.current().live.max_concurrent;
        {
            // 锁只保护并发计数检查：守卫若活到函数末尾，下方网络调用期间会一直
            // 占锁（status/dashboard 全部被堵），且与后面的二次 lock() 自死锁。
            let sessions = self.inner.sessions.lock().await;
            if active_session_count(&sessions) >= max_concurrent {
                return Err(anyhow!(
                    "已达到直播录制并发上限 ({max_concurrent} 路)，可在系统设置中调整"
                ));
            }
        }
        let cookies = self.inner.settings_service.cookie_header().await?;
        let init = self
            .inner
            .bili_api
            .live_room_init(room_id, &cookies)
            .await
            .context("解析直播间号失败")?;
        if init.room_id <= 0 {
            return Err(anyhow!("直播间 {room_id} 不存在或不可用"));
        }
        if !init.is_live() {
            return Err(anyhow!("直播间 {} 当前未开播", init.room_id));
        }
        let room_id = init.room_id;
        let startup_snapshot = startup_snapshot(room_id, trigger, capture_mode);
        {
            let mut sessions = self.inner.sessions.lock().await;
            if active_session_count(&sessions) >= max_concurrent {
                return Err(anyhow!("recording concurrency limit reached"));
            }
            match sessions.get(&room_id) {
                // 遗留的 Starting 条目（启动任务被异常中止且清理未执行到）：
                // 超时后清除，避免该房间永久报"已在录制中或正在启动"。
                Some(SessionEntry::Starting { created_at, .. })
                    if created_at.elapsed() >= STARTING_STALE_TIMEOUT =>
                {
                    warn!(
                        room_id,
                        "直播启动超过 120 秒未完成，清除遗留 Starting 条目后重试"
                    );
                    sessions.remove(&room_id);
                }
                Some(_) => {
                    return Err(anyhow!("直播间 {room_id} 已在录制中或正在启动"));
                }
                None => {}
            }
            let generation =
                START_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            sessions.insert(
                room_id,
                SessionEntry::Starting {
                    snapshot: startup_snapshot,
                    cancellation: CancellationToken::new(),
                    generation,
                    created_at: Instant::now(),
                },
            );
        }
        let (startup_cancellation, startup_generation) = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Starting {
                    cancellation,
                    generation,
                    ..
                }) => (cancellation.clone(), *generation),
                _ => return Err(anyhow!("直播录制启动已取消")),
            }
        };

        let result = self
            .start_session(
                &init,
                &cookies,
                trigger,
                capture_mode,
                &startup_cancellation,
            )
            .await;
        if result.is_err() {
            // 仅移除本次启动创建的 Starting 条目：若失败前用户已 stop() 并对同房间
            // 发起新录制，map 中是新一代条目，不能误删。
            let mut sessions = self.inner.sessions.lock().await;
            if matches!(
                sessions.get(&room_id),
                Some(SessionEntry::Starting {
                    generation: g, ..
                }) if *g == startup_generation
            ) {
                sessions.remove(&room_id);
            }
        }
        result
    }

    /// 停止录制并等待 worker 完成弹幕收尾和分段合并。
    pub async fn stop(&self, room_id: i64) -> Result<RecordingInfo> {
        let startup = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(_)) => None,
                Some(SessionEntry::Starting {
                    snapshot,
                    cancellation,
                    generation,
                    ..
                }) => {
                    cancellation.cancel();
                    Some((snapshot.clone(), *generation))
                }
                Some(SessionEntry::Stopping { snapshot, .. }) => {
                    let snapshot = snapshot.clone();
                    drop(sessions);
                    return Ok(snapshot.lock().await.clone());
                }
                None => return Err(anyhow!("直播间 {room_id} 未在录制")),
            }
        };

        if let Some((snapshot, generation)) = startup {
            let result = {
                let mut info = snapshot.lock().await;
                info.status = RecordingStatus::Cancelled;
                info.error_msg = Some("启动已由用户取消".to_owned());
                info.clone()
            };
            // 仅移除自己取消的那一代 Starting：跨 await 期间用户可能已发起同房间的新启动。
            let mut sessions = self.inner.sessions.lock().await;
            if matches!(
                sessions.get(&room_id),
                Some(SessionEntry::Starting {
                    generation: g, ..
                }) if *g == generation
            ) {
                sessions.remove(&room_id);
            }
            return Ok(result);
        }
        let handle = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(handle)) => handle.clone(),
                Some(SessionEntry::Stopping { snapshot, .. }) => {
                    let snapshot = snapshot.clone();
                    drop(sessions);
                    return Ok(snapshot.lock().await.clone());
                }
                _ => return Err(anyhow!("直播间 {room_id} 未在录制")),
            }
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle.command_tx.send(SessionCommand::Stop(reply_tx)),
        )
        .await
        .map_err(|_| anyhow!("发送停止命令超时"))?
        .map_err(|_| anyhow!("直播录制 worker 已退出"))?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), reply_rx)
            .await
            .map_err(|_| anyhow!("等待直播录制停止结果超时"))?
            .map_err(|_| anyhow!("直播录制 worker 未返回停止结果"))?;
        self.inner.sessions.lock().await.remove(&room_id);
        result
    }

    /// 请求停止，但不让 HTTP 请求等待互动事件排空和 FFmpeg/ffprobe 合并完成。
    pub async fn request_stop(&self, room_id: i64) -> Result<MergeJobInfo> {
        let (handle, existing_job) = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(handle)) => (Some(handle.clone()), None),
                Some(SessionEntry::Stopping { job_id, .. }) => (None, Some(job_id.clone())),
                Some(SessionEntry::Starting { cancellation, .. }) => {
                    cancellation.cancel();
                    return Err(anyhow!("recording startup cancellation requested"));
                }
                None => return Err(anyhow!("recording session not found")),
            }
        };
        if let Some(job_id) = existing_job {
            return self
                .merge_job(&job_id)
                .await
                .ok_or_else(|| anyhow!("stop operation not found"));
        }
        let handle = handle.ok_or_else(|| anyhow!("recording session is not active"))?;
        let recording_id = handle
            .snapshot
            .lock()
            .await
            .recording_id
            .ok_or_else(|| anyhow!("recording id not available"))?;
        if let Some(job) = self.find_active_merge_job(recording_id).await? {
            return Ok(job);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let job = MergeJobInfo {
            id: uuid::Uuid::new_v4().simple().to_string(),
            recording_id,
            status: "queued".to_owned(),
            progress: 0,
            error: None,
            source_segment_count: 0,
            cancel_requested: false,
            created_at: now.clone(),
            updated_at: now,
        };
        let job_id = job.id.clone();
        let cancellation = CancellationToken::new();
        handle.snapshot.lock().await.status = RecordingStatus::Stopping;
        self.inner
            .merge_jobs
            .lock()
            .await
            .insert(job_id.clone(), job.clone());
        self.inner
            .merge_cancellations
            .lock()
            .await
            .insert(job_id.clone(), cancellation);
        let existing_job = {
            let mut sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(_)) => {
                    sessions.insert(
                        room_id,
                        SessionEntry::Stopping {
                            snapshot: handle.snapshot.clone(),
                            job_id: job_id.clone(),
                        },
                    );
                    None
                }
                Some(SessionEntry::Stopping { job_id, .. }) => Some(job_id.clone()),
                _ => Some(String::new()),
            }
        };
        if let Some(existing_job_id) = existing_job {
            self.inner.merge_jobs.lock().await.remove(&job_id);
            self.inner.merge_cancellations.lock().await.remove(&job_id);
            if existing_job_id.is_empty() {
                return Err(anyhow!("recording session is no longer active"));
            }
            return self
                .merge_job(&existing_job_id)
                .await
                .ok_or_else(|| anyhow!("stop operation not found"));
        }
        if let Err(error) = persist_merge_job(&self.inner.db, &job).await {
            update_merge_job(
                &self.inner,
                &job_id,
                "failed",
                100,
                Some("failed to persist stop operation".to_owned()),
            )
            .await;
            return Err(error);
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        if handle
            .command_tx
            .send(SessionCommand::StopBackground {
                job_id: job_id.clone(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            update_merge_job(
                &self.inner,
                &job_id,
                "failed",
                100,
                Some("recording worker stopped".to_owned()),
            )
            .await;
            return Err(anyhow!("recording worker stopped"));
        }
        // 只等 worker 确认收到停止命令；超时或通道关闭都无碍——
        // 下方直接从 map/DB 读取合并任务状态返回。
        tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx)
            .await
            .ok();
        self.merge_job(&job_id)
            .await
            .ok_or_else(|| anyhow!("stop operation disappeared"))
    }

    /// 优雅停止所有活动会话。单个会话失败时，也不能阻止应用关停期间
    /// 清理其余 FFmpeg 子进程。
    pub async fn stop_all(&self) {
        // 不取消进行中的合并：取消会把任务行写成 cancelled（下次启动不再续跑）
        // 并删掉半成品。收尾任务已登记更大的关机预算，让它跑完才是用户要的结果；
        // 真超过预算被 abort 时，行仍是 running，下次启动由 restore_merge_jobs 接手。
        let room_ids = {
            let sessions = self.inner.sessions.lock().await;
            sessions
                .iter()
                .filter_map(|(room_id, entry)| {
                    matches!(
                        entry,
                        SessionEntry::Active(_)
                            | SessionEntry::Starting { .. }
                            | SessionEntry::Stopping { .. }
                    )
                    .then_some(*room_id)
                })
                .collect::<Vec<_>>()
        };
        // 有界并行关停：逐房间串行时多房间关停总时长线性累加（单房间可达 ~35s）。
        // stop() 不跨 await 持有 sessions 锁且各房间相互独立，可安全并行。
        futures::stream::iter(room_ids)
            .for_each_concurrent(4, |room_id| async move {
                if let Err(error) = self.stop(room_id).await {
                    warn!(room_id, "关闭程序时停止直播录制失败: {error}");
                }
            })
            .await;
    }
}
