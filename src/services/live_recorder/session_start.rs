//! 录制会话装配：从直播间信息与流地址出发，依次准备弹幕采集、数据库记录与
//! FFmpeg 进程，并把运行中的 worker 接入会话表。FFmpeg 刻意作为启动阶段
//! 的最后一个副作用。

use super::danmu::{abort_danmu_tasks, start_danmu_capture};
use super::interactions::InteractionPaths;
use super::segments::{persist_segment, segment_path};
use super::startup::{
    ensure_startup_active, mark_startup_recording, referer, render_file_template, source_max_qn,
    user_agent,
};
use super::stream_url::select_stream_candidates;
use super::{
    LiveRecorder, RecordingInfo, RecordingSessionHandle, RecordingTrigger, RecordingWorker,
    SessionEntry, FINALIZE_SHUTDOWN_BUDGET, STOP_TIMEOUT,
};
use crate::models::live_recording;
use crate::services::bili_api::models::live::LiveRoomInit;
use crate::services::file_safety::sanitize_filename;
use crate::services::live_source::CaptureMode;
use anyhow::{anyhow, Context, Result};
use sea_orm::{ActiveModelTrait, Set};
use std::collections::VecDeque;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

impl LiveRecorder {
    pub(super) async fn start_session(
        &self,
        init: &LiveRoomInit,
        cookies: &str,
        trigger: RecordingTrigger,
        capture_mode: CaptureMode,
        startup_cancellation: &CancellationToken,
    ) -> Result<RecordingInfo> {
        ensure_startup_active(startup_cancellation)?;
        let room_id = init.room_id;
        let info = self
            .inner
            .bili_api
            .live_get_info(room_id, cookies)
            .await
            .context("获取直播间信息失败")?;
        ensure_startup_active(startup_cancellation)?;
        let max_qn = source_max_qn(&self.inner.db, room_id).await;
        let playurl = self
            .inner
            .bili_api
            .live_playurl(room_id, max_qn, cookies)
            .await
            .context("获取直播流地址失败")?;
        ensure_startup_active(startup_cancellation)?;
        let stream_candidates = select_stream_candidates(&playurl.durl)?;
        let selected_stream = stream_candidates
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("流地址列表为空"))?;
        let stream_url = selected_stream.url.clone();

        let ffmpeg_path = self
            .inner
            .video_processor
            .detect_ffmpeg_for_live()
            .await
            .context("直播录制需要支持网络流的完整版 FFmpeg")?;

        let live_dir = self
            .inner
            .paths
            .download_dir
            .join("live")
            .join(room_id.to_string());
        // spawn_blocking：fs2 是同步系统调用，Windows 网络盘可能阻塞 runtime 线程
        let download_dir = self.inner.paths.download_dir.clone();
        let available = tokio::task::spawn_blocking(move || fs2::available_space(&download_dir))
            .await
            .context("磁盘空间检查任务失败")
            .and_then(|result| result.context("检查直播录制磁盘空间失败"))?;
        let live_cfg = self.inner.settings_service.current().live.clone();
        let min_free_bytes = live_cfg.min_free_space_gib * 1024 * 1024 * 1024;
        if available < min_free_bytes {
            return Err(anyhow!(
                "可用磁盘空间低于 {} GiB 安全阈值，已拒绝启动直播录制",
                live_cfg.min_free_space_gib
            ));
        }
        let now_local = chrono::Local::now();
        let timestamp = now_local.format("%Y%m%d_%H%M%S");
        let rendered = render_file_template(
            &live_cfg.file_name_template,
            room_id,
            &info.title,
            now_local,
        );
        let safe_name = sanitize_filename(&rendered);
        // 保留短时间戳与随机后缀，避免同日多场直播文件名碰撞
        let unique_suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let base_prefix = format!("{safe_name}_{timestamp}_{unique_suffix}");
        let first_segment = segment_path(&live_dir, &base_prefix, 0);
        let started_at = chrono::Utc::now();
        let interaction_paths = InteractionPaths {
            legacy: live_dir.join(format!("{base_prefix}_danmu.json")),
            events: live_dir.join(format!("{base_prefix}_events.jsonl")),
            xml: live_dir.join(format!("{base_prefix}_danmaku.xml")),
            standard_xml: live_dir.join(format!("{base_prefix}_danmaku_bilibili.xml")),
            summary: live_dir.join(format!("{base_prefix}_interaction_summary.json")),
        };
        let initial_info = RecordingInfo {
            room_id,
            recording_id: None,
            title: info.title.clone(),
            status: super::RecordingStatus::Starting,
            output_path: first_segment.to_string_lossy().to_string(),
            started_at: started_at.to_rfc3339(),
            duration_secs: 0,
            file_size: 0,
            error_msg: None,
            danmu_unavailable: false,
            stream_quality: (selected_stream.current_qn > 0).then_some(selected_stream.current_qn),
            stream_protocol: (!selected_stream.protocol_name.is_empty())
                .then_some(selected_stream.protocol_name.clone()),
            stream_format: (!selected_stream.format_name.is_empty())
                .then_some(selected_stream.format_name.clone()),
            stream_codec: (!selected_stream.codec_name.is_empty())
                .then_some(selected_stream.codec_name.clone()),
            trigger: trigger.as_str().to_owned(),
            capture_mode: capture_mode.as_str().to_owned(),
            interaction_capture_status: if capture_mode == CaptureMode::Off {
                "off"
            } else {
                "connecting"
            }
            .to_owned(),
            interaction_error: None,
            event_path: (capture_mode != CaptureMode::Off)
                .then(|| interaction_paths.events.to_string_lossy().to_string()),
            xml_path: (capture_mode != CaptureMode::Off)
                .then(|| interaction_paths.xml.to_string_lossy().to_string()),
            summary_path: (capture_mode != CaptureMode::Off)
                .then(|| interaction_paths.summary.to_string_lossy().to_string()),
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
        };
        let snapshot = Arc::new(Mutex::new(initial_info));
        let recent = Arc::new(Mutex::new(VecDeque::with_capacity(100)));
        let segment_index = Arc::new(AtomicU32::new(0));
        let collector_cancel = CancellationToken::new();
        let mut danmu = start_danmu_capture(
            &self.inner.bili_api,
            room_id,
            cookies,
            capture_mode,
            &info.title,
            started_at,
            &interaction_paths,
            &snapshot,
            &recent,
            &segment_index,
            &collector_cancel,
        )
        .await;
        let initial_info = snapshot.lock().await.clone();
        let now = chrono::Utc::now().to_rfc3339();
        let recording_result = live_recording::ActiveModel {
            room_id: Set(room_id),
            short_id: Set(init.short_id),
            uid: Set(init.uid),
            title: Set(info.title.clone()),
            cover: Set(
                crate::services::bili_url_policy::normalize_syntax(&info.user_cover)
                    .map(|url| url.to_string())
                    .unwrap_or_default(),
            ),
            status: Set(super::RecordingStatus::Starting.to_string()),
            output_path: Set(Some(initial_info.output_path.clone())),
            danmu_path: Set(danmu
                .persisted_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())),
            file_size: Set(0),
            duration: Set(0),
            started_at: Set(initial_info.started_at.clone()),
            ended_at: Set(None),
            error_msg: Set(None),
            trigger: Set(initial_info.trigger.clone()),
            event_path: Set(initial_info.event_path.clone()),
            xml_path: Set(initial_info.xml_path.clone()),
            summary_path: Set(initial_info.summary_path.clone()),
            capture_mode: Set(initial_info.capture_mode.clone()),
            interaction_status: Set(initial_info.interaction_capture_status.clone()),
            interaction_error: Set(initial_info.interaction_error.clone()),
            danmaku_count: Set(0),
            unique_user_count: Set(0),
            free_gift_count: Set(0),
            paid_gift_count: Set(0),
            sc_count: Set(0),
            guard_count: Set(0),
            peak_watched: Set(0),
            dropped_event_count: Set(0),
            estimated_paid_value: Set(0.0),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&self.inner.db)
        .await;
        let recording = match recording_result {
            Ok(recording) => recording,
            Err(error) => {
                abort_danmu_tasks(&collector_cancel, &mut danmu.monitor, &mut danmu.writer);
                return Err(error).context("保存直播录制记录失败");
            }
        };
        snapshot.lock().await.recording_id = Some(recording.id);
        if let Err(error) =
            persist_segment(&self.inner.db, recording.id, 0, &first_segment, "open").await
        {
            abort_danmu_tasks(&collector_cancel, &mut danmu.monitor, &mut danmu.writer);
            mark_startup_recording(
                &self.inner.db,
                recording.id,
                super::RecordingStatus::Failed,
                format!("persist initial recording segment failed: {error}"),
            )
            .await;
            return Err(error).context("persist initial recording segment failed");
        }
        // FFmpeg 刻意作为启动阶段的最后一个副作用：此时元数据、互动绑定和
        // 持久化录制记录都已准备完毕。
        let mut ffmpeg = match super::ffmpeg_session::FfmpegSession::start(
            &ffmpeg_path,
            &stream_url,
            first_segment.clone(),
            room_id,
            user_agent(),
            referer(),
        ) {
            Ok(session) => session,
            Err(error) => {
                abort_danmu_tasks(&collector_cancel, &mut danmu.monitor, &mut danmu.writer);
                let failed = live_recording::ActiveModel {
                    id: Set(recording.id),
                    status: Set(super::RecordingStatus::Failed.to_string()),
                    ended_at: Set(Some(chrono::Utc::now().to_rfc3339())),
                    error_msg: Set(Some(format!("启动 FFmpeg 失败: {error}"))),
                    updated_at: Set(chrono::Utc::now().to_rfc3339()),
                    ..Default::default()
                };
                // 记录已是失败态，此处更新失败只留痕，不掩盖原始启动错误。
                failed
                    .update(&self.inner.db)
                    .await
                    .inspect_err(|update_error| {
                        warn!(
                            recording_id = recording.id,
                            "FFmpeg 启动失败后更新录制记录失败: {update_error}"
                        );
                    })
                    .ok();
                return Err(error);
            }
        };
        if startup_cancellation.is_cancelled() {
            abort_danmu_tasks(&collector_cancel, &mut danmu.monitor, &mut danmu.writer);
            if let Err(error) = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                warn!(
                    room_id,
                    "启动取消后停止 FFmpeg 失败，交由 kill_on_drop 兜底: {error}"
                );
            }
            mark_startup_recording(
                &self.inner.db,
                recording.id,
                super::RecordingStatus::Cancelled,
                "启动已由用户取消".to_owned(),
            )
            .await;
            return Err(anyhow!("直播录制启动已取消"));
        }
        {
            let mut state = snapshot.lock().await;
            state.status = super::RecordingStatus::Recording;
        }
        let initial_info = snapshot.lock().await.clone();
        let started = live_recording::ActiveModel {
            id: Set(recording.id),
            status: Set(super::RecordingStatus::Recording.to_string()),
            interaction_status: Set(initial_info.interaction_capture_status.clone()),
            interaction_error: Set(initial_info.interaction_error.clone()),
            updated_at: Set(chrono::Utc::now().to_rfc3339()),
            ..Default::default()
        };
        if let Err(error) = started.update(&self.inner.db).await {
            if let Err(stop_error) = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                warn!(
                    room_id,
                    "启动失败后停止 FFmpeg 失败，交由 kill_on_drop 兜底: {stop_error}"
                );
            }
            mark_startup_recording(
                &self.inner.db,
                recording.id,
                super::RecordingStatus::Failed,
                format!("update recording startup state failed: {error}"),
            )
            .await;
            return Err(error.into());
        }
        // 最后一次取消检查：上面的 DB 更新等 await 期间，stop() 可能已取消启动令牌
        // 并移除 Starting 条目。此检查之后到 insert/spawn 之间不再有 await，
        // 消除“已向用户返回取消，录制却复活为 Active 会话”的 TOCTOU 竞态。
        if startup_cancellation.is_cancelled() {
            abort_danmu_tasks(&collector_cancel, &mut danmu.monitor, &mut danmu.writer);
            if let Err(error) = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                warn!(
                    room_id,
                    "启动取消后停止 FFmpeg 失败，交由 kill_on_drop 兜底: {error}"
                );
            }
            mark_startup_recording(
                &self.inner.db,
                recording.id,
                super::RecordingStatus::Cancelled,
                "启动已由用户取消".to_owned(),
            )
            .await;
            return Err(anyhow!("直播录制启动已取消"));
        }
        let (command_tx, command_rx) = mpsc::channel(8);
        let handle = RecordingSessionHandle {
            snapshot: snapshot.clone(),
            command_tx,
            recent,
        };

        let worker = RecordingWorker {
            room_id,
            started_at,
            snapshot,
            command_rx,
            reload_rx: danmu.reload_rx,
            bili_api: self.inner.bili_api.clone(),
            settings_service: self.inner.settings_service.clone(),
            db: self.inner.db.clone(),
            recording_id: recording.id,
            ffmpeg_path,
            current_url: stream_url,
            stream_candidates,
            candidate_index: 0,
            current_ffmpeg: Some(ffmpeg),
            live_dir,
            base_prefix,
            next_segment: 1,
            segments: vec![first_segment],
            segment_index,
            collector_cancel,
            danmu_collector_handle: danmu.monitor,
            danmu_collector_rx: danmu.collector_rx,
            danmu_collector_channel_open: danmu.channel_open,
            danmu_write_handle: danmu.writer,
            danmu_path: danmu.persisted_path,
            danmu_failure_rx: danmu.failure_rx,
            danmu_failure_channel_open: danmu.channel_open,
            reload_channel_open: danmu.channel_open,
            failure: None,
            stop_requested: false,
            last_url_refresh: tokio::time::Instant::now(),
            last_checkpoint: tokio::time::Instant::now(),
            restart_attempts: 0,
            next_recovery_at: None,
            stop_reason: None,
            is_recoverable: false,
            unexpected_exit_detail: None,
            sessions: self.inner.sessions.clone(),
            merge_jobs: self.inner.merge_jobs.clone(),
            merge_cancellations: self.inner.merge_cancellations.clone(),
            background_tasks: self.inner.background_tasks.clone(),
        };
        self.inner
            .sessions
            .lock()
            .await
            .insert(room_id, SessionEntry::Active(handle));
        // 带 panic 记录与兜底清理：worker panic 时无人移除 Active 条目，
        // 该房间会永久报"已在录制中"并占用 max_concurrent 额度直至重启。
        // 条目存在期间同房间无法发起新会话，panic 后移除的必是本会话条目。
        let panic_sessions = self.inner.sessions.clone();
        let accepted = self.inner.background_tasks.spawn_with_panic_and_budget(
            "live_recorder_worker",
            async move { worker.run().await },
            move || async move {
                panic_sessions.lock().await.remove(&room_id);
            },
            FINALIZE_SHUTDOWN_BUDGET,
        );
        if !accepted {
            self.inner.sessions.lock().await.remove(&room_id);
            mark_startup_recording(
                &self.inner.db,
                recording.id,
                super::RecordingStatus::Failed,
                "应用正在关闭，直播录制 worker 未启动".to_owned(),
            )
            .await;
            return Err(anyhow!("应用正在关闭，无法启动直播录制"));
        }
        info!(room_id, "直播录制已开始");
        Ok(initial_info)
    }
}
