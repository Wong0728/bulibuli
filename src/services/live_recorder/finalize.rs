//! 录制收尾（impl RecordingWorker）：停止 FFmpeg、排空弹幕任务、登记并执行
//! 分段合并、落库最终状态，以及失败/快照维护。

use super::ffmpeg_merge::{merge_segments_to_mp4, merge_segments_to_mp4_cancelable};
use super::merge_jobs::{finish_finalize_merge_job, register_finalize_merge_job};
use super::segments::{
    cleanup_recording_segments, merge_fallback_output_path, persist_closed_segments,
    remove_merged_segments,
};
use super::startup::recovery_state;
use super::{
    RecordingInfo, RecordingStatus, RecordingWorker, DANMU_STOP_TIMEOUT, STOP_REASON_COMPLETED,
    STOP_REASON_FAILED, STOP_REASON_MANUAL, STOP_TIMEOUT,
};
use crate::models::live_recording;
use anyhow::Result;
use sea_orm::{ActiveModelTrait, Set};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

impl RecordingWorker {
    pub(super) async fn finalize(
        &mut self,
        merge_cancellation: Option<&CancellationToken>,
    ) -> Result<RecordingInfo> {
        if self.stop_requested && self.stop_reason.is_none() {
            self.stop_reason = Some(STOP_REASON_MANUAL);
        }
        if self.failure.is_none() {
            self.set_status(RecordingStatus::Stopping).await;
        }

        if let Some(mut ffmpeg) = self.current_ffmpeg.take() {
            if let Err(error) = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                self.mark_failure(format!("停止 FFmpeg 失败: {error}"))
                    .await;
            }
        }
        self.collector_cancel.cancel();

        if let Some(mut handle) = self.danmu_collector_handle.take() {
            match tokio::time::timeout(DANMU_STOP_TIMEOUT, &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.mark_danmu_unavailable(format!("弹幕采集任务异常退出: {error}"))
                        .await
                }
                Err(_) => {
                    handle.abort();
                    self.mark_danmu_unavailable("等待弹幕采集任务超时".to_string())
                        .await
                }
            }
        }
        if let Some(mut handle) = self.danmu_write_handle.take() {
            match tokio::time::timeout(DANMU_STOP_TIMEOUT, &mut handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    self.mark_danmu_unavailable(format!("弹幕文件写入失败: {error}"))
                        .await
                }
                Ok(Err(error)) => {
                    self.mark_danmu_unavailable(format!("弹幕写入任务异常退出: {error}"))
                        .await
                }
                Err(_) => {
                    handle.abort();
                    self.mark_danmu_unavailable("等待弹幕写入任务超时".to_string())
                        .await
                }
            }
        }

        self.set_status(RecordingStatus::Finalizing).await;
        persist_closed_segments(&self.db, self.recording_id, &self.segments).await;
        // 崩溃/秒退可能留下不存在或 0 字节的分段，concat 清单引用它们必然失败；
        // 只合并真实有效的分段。
        let mut valid_segments = Vec::with_capacity(self.segments.len());
        for segment in &self.segments {
            match tokio::fs::metadata(segment).await {
                Ok(metadata) if metadata.len() > 0 => valid_segments.push(segment.clone()),
                _ => {
                    debug!(
                        path = %segment.display(),
                        "跳过无效直播分段（不存在或为空）"
                    );
                }
            }
        }
        // 同步停止（stop()/关机）与后台停止口径一致：合并之前先登记 live_merge_jobs 行，
        // 进程在合并中被杀时下次启动的 restore_merge_jobs 才有活可接。
        let merge_job_id = self.begin_finalize_merge_job(valid_segments.len()).await;
        let merge_result = if valid_segments.is_empty() {
            Err(anyhow::anyhow!("没有有效的直播分段可供合并"))
        } else {
            match merge_cancellation {
                Some(cancellation) => {
                    merge_segments_to_mp4_cancelable(
                        &self.ffmpeg_path,
                        &valid_segments,
                        cancellation,
                    )
                    .await
                }
                None => merge_segments_to_mp4(&self.ffmpeg_path, &valid_segments).await,
            }
        };
        // 关闭合并任务必须在删除源分段之前：万一此后进程被杀，行里已是终态，
        // 下次启动不会去重跑一次无分段的合并。
        self.close_finalize_merge_job(merge_job_id.as_deref(), &merge_result, merge_cancellation)
            .await;
        let (final_path, merge_error) = match merge_result {
            Ok(path) => (path, None),
            Err(error) => (
                merge_fallback_output_path(&self.segments, &self.live_dir, &self.base_prefix),
                Some(error.to_string()),
            ),
        };
        if let Some(error) = &merge_error {
            self.mark_failure(format!("直播分段合并失败: {error}"))
                .await;
        }

        let final_status = if self.failure.is_some() {
            RecordingStatus::Failed
        } else if self.stop_requested {
            RecordingStatus::Stopped
        } else {
            if self.stop_reason.is_none() {
                self.stop_reason = Some(STOP_REASON_COMPLETED);
            }
            RecordingStatus::Completed
        };
        let file_size = tokio::fs::metadata(&final_path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let duration = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .num_seconds();
        let mut info = self.snapshot.lock().await.clone();
        info.status = final_status.clone();
        info.output_path = final_path.to_string_lossy().to_string();
        info.duration_secs = duration;
        info.file_size = file_size as i64;
        info.error_msg = self.failure.clone();
        if info.capture_mode != "off" && info.interaction_capture_status == "capturing" {
            info.interaction_capture_status = "completed".to_owned();
        }
        // 数据库落盘失败时保留最终文件并立即重试一次，避免出现“磁盘有文件、
        // 数据库无记录”的单点瞬时不一致；后续启动恢复仍会扫描可恢复记录。
        let persisted =
            self.persist_recording(&info, true).await || self.persist_recording(&info, true).await;
        // 分段只能在记录认领成品之后删除：先删后落库一旦失败，成品就成了孤儿，
        // 而重试合并因分段已删恒报 no segments，不可恢复。
        if persisted && merge_error.is_none() {
            remove_merged_segments(&self.segments).await;
            cleanup_recording_segments(&self.db, self.recording_id).await;
        }
        *self.snapshot.lock().await = info.clone();
        info!(
            operation = "live_recording_finalize",
            room_id = self.room_id,
            ?final_status,
            duration_secs = duration,
            bytes = file_size,
            segment_count = self.segments.len(),
            restart_attempts = self.restart_attempts,
            recovery_state = recovery_state(&final_status.to_string(), true, self.segments.len()),
            "直播录制已完成收尾"
        );
        Ok(info)
    }

    /// 合并前登记收尾任务，使同步停止与后台停止在“是否登记合并任务”上口径一致。
    async fn begin_finalize_merge_job(&self, segment_count: usize) -> Option<String> {
        register_finalize_merge_job(&self.db, &self.merge_jobs, self.recording_id, segment_count)
            .await
    }

    async fn close_finalize_merge_job(
        &self,
        job_id: Option<&str>,
        merge_result: &Result<PathBuf>,
        cancellation: Option<&CancellationToken>,
    ) {
        finish_finalize_merge_job(
            &self.db,
            &self.merge_jobs,
            &self.merge_cancellations,
            job_id,
            merge_result,
            cancellation,
        )
        .await;
    }

    pub(super) async fn mark_failure(&mut self, error: String) {
        if self.stop_reason.is_none() {
            self.stop_reason = Some(STOP_REASON_FAILED);
        }
        if self.failure.is_none() {
            warn!(room_id = self.room_id, "直播录制进入失败状态: {error}");
            self.failure = Some(error);
        }
        self.collector_cancel.cancel();
        {
            let mut snapshot = self.snapshot.lock().await;
            snapshot.status = RecordingStatus::Failed;
            snapshot.error_msg = self.failure.clone();
        }
        let snapshot = self.snapshot.lock().await.clone();
        self.persist_recording(&snapshot, false).await;
    }

    pub(super) async fn mark_danmu_unavailable(&mut self, error: String) {
        warn!(
            room_id = self.room_id,
            "弹幕录制不可用，视频录制继续: {error}"
        );
        self.collector_cancel.cancel();
        let snapshot = {
            let mut snapshot = self.snapshot.lock().await;
            snapshot.danmu_unavailable = true;
            snapshot.interaction_capture_status = "unavailable".to_owned();
            snapshot.interaction_error = Some(error);
            snapshot.clone()
        };
        self.persist_recording(&snapshot, false).await;
    }

    async fn set_status(&self, status: RecordingStatus) {
        self.snapshot.lock().await.status = status;
    }

    pub(super) async fn update_snapshot(&self) {
        let size = super::segments::total_file_size(&self.segments).await;
        let output_path = self
            .current_ffmpeg
            .as_ref()
            .map(|session| session.output_path().to_string_lossy().to_string())
            .or_else(|| {
                self.segments
                    .last()
                    .map(|path| path.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| {
                self.live_dir
                    .join(&self.base_prefix)
                    .to_string_lossy()
                    .to_string()
            });
        let duration = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .num_seconds();
        let mut snapshot = self.snapshot.lock().await;
        snapshot.output_path = output_path;
        snapshot.duration_secs = duration;
        snapshot.file_size = size as i64;
        snapshot.error_msg = self.failure.clone();
        if self.failure.is_some() {
            snapshot.status = RecordingStatus::Failed;
        }
    }

    pub(super) async fn persist_recording(&self, info: &RecordingInfo, ended: bool) -> bool {
        let update = live_recording::ActiveModel {
            id: Set(self.recording_id),
            status: Set(info.status.to_string()),
            output_path: Set((!info.output_path.is_empty()).then_some(info.output_path.clone())),
            danmu_path: Set(self
                .danmu_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())),
            file_size: Set(info.file_size),
            duration: Set(info.duration_secs),
            ended_at: if ended {
                Set(Some(chrono::Utc::now().to_rfc3339()))
            } else {
                sea_orm::ActiveValue::NotSet
            },
            error_msg: Set(info.error_msg.clone()),
            event_path: Set(info.event_path.clone()),
            xml_path: Set(info.xml_path.clone()),
            summary_path: Set(info.summary_path.clone()),
            capture_mode: Set(info.capture_mode.clone()),
            interaction_status: Set(info.interaction_capture_status.clone()),
            interaction_error: Set(info.interaction_error.clone()),
            danmaku_count: Set(info.danmaku_count),
            unique_user_count: Set(info.unique_user_count),
            free_gift_count: Set(info.free_gift_count),
            paid_gift_count: Set(info.paid_gift_count),
            sc_count: Set(info.sc_count),
            guard_count: Set(info.guard_count),
            peak_watched: Set(info.peak_watched),
            dropped_event_count: Set(info.dropped_event_count),
            estimated_paid_value: Set(info.estimated_paid_value),
            stop_reason: Set(self.stop_reason.map(str::to_owned)),
            segment_index: Set(self.segment_index.load(Ordering::Relaxed) as i32),
            restart_attempts: Set(self.restart_attempts as i32),
            checkpointed_at: Set(Some(chrono::Utc::now().to_rfc3339())),
            is_recoverable: Set(self.is_recoverable),
            updated_at: Set(chrono::Utc::now().to_rfc3339()),
            ..Default::default()
        };
        if let Err(error) = update.update(&self.db).await {
            warn!(room_id = self.room_id, "更新直播录制记录失败: {error}");
            return false;
        }
        true
    }
}
