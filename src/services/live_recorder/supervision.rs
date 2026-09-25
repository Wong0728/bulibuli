//! 进程监督与流地址轮换（impl RecordingWorker）：观察 FFmpeg 退出、确认直播间
//! 状态、按退避策略恢复录制，以及健康时段的流地址刷新与分段切换。

use super::segments::{persist_segment, segment_path};
use super::startup::{referer, source_max_qn, user_agent};
use super::state::{unexpected_exit_action, UnexpectedExitAction};
use super::stream_url::select_stream_candidates;
use super::{
    RecordingWorker, MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS, STOP_REASON_OFFLINE_END,
    STOP_REASON_UNRECOVERABLE_EXIT,
};
use crate::services::bili_api::models::live::LiveStreamUrl;
use anyhow::{anyhow, Context, Result};
use std::sync::atomic::Ordering;
use tokio::time::Instant;
use tracing::{info, warn};

/// 异常退出恢复的退避间隔（按已尝试次数）。
fn recovery_backoff(attempts: u32) -> std::time::Duration {
    match attempts {
        1 => std::time::Duration::from_secs(2),
        2 => std::time::Duration::from_secs(5),
        _ => std::time::Duration::from_secs(15),
    }
}

impl RecordingWorker {
    /// 返回 true 表示录制应当收尾；正常运行和恢复中的情况均返回 false。
    pub(super) async fn observe_process(&mut self) -> bool {
        let process_result = self
            .current_ffmpeg
            .as_mut()
            .map(super::ffmpeg_session::FfmpegSession::try_wait);
        let Some(process_result) = process_result else {
            return if self.unexpected_exit_detail.is_some() {
                self.recover_after_unexpected_exit().await
            } else {
                true
            };
        };

        match process_result {
            Ok(None) => false,
            Ok(Some(status)) => {
                let diagnostics = if let Some(session) = self.current_ffmpeg.as_mut() {
                    session.diagnostics().await
                } else {
                    String::new()
                };
                self.current_ffmpeg = None;
                let detail = if diagnostics.is_empty() {
                    format!("FFmpeg 未经停止请求提前退出: status={status}")
                } else {
                    format!("FFmpeg 未经停止请求提前退出: status={status}; {diagnostics}")
                };
                if self.stop_requested {
                    return true;
                }
                let room_is_offline = match self.confirm_room_offline().await {
                    Ok(offline) => Some(offline),
                    Err(error) => {
                        self.unexpected_exit_detail =
                            Some(format!("{detail}; 无法确认直播间是否已下播: {error}"));
                        None
                    }
                };
                match unexpected_exit_action(room_is_offline, self.restart_attempts) {
                    UnexpectedExitAction::CompleteAfterOfflineConfirmation => {
                        self.stop_reason = Some(STOP_REASON_OFFLINE_END);
                        info!(
                            room_id = self.room_id,
                            "FFmpeg 退出且直播间已下播，按正常完成收尾"
                        );
                        true
                    }
                    UnexpectedExitAction::Recover => {
                        if self.unexpected_exit_detail.is_none() {
                            self.unexpected_exit_detail = Some(detail);
                        }
                        self.recover_after_unexpected_exit().await
                    }
                    UnexpectedExitAction::FailRecoverable => {
                        if self.unexpected_exit_detail.is_none() {
                            self.unexpected_exit_detail = Some(detail);
                        }
                        self.stop_reason = Some(STOP_REASON_UNRECOVERABLE_EXIT);
                        self.is_recoverable = true;
                        let detail = self.unexpected_exit_detail.take().expect("exit detail set");
                        self.mark_failure(detail).await;
                        true
                    }
                }
            }
            Err(error) => {
                self.mark_failure(format!("检查 FFmpeg 进程状态失败: {error}"))
                    .await;
                true
            }
        }
    }

    async fn confirm_room_offline(&self) -> Result<bool> {
        let cookies = self.settings_service.cookie_header().await?;
        let init = self
            .bili_api
            .live_room_init(self.room_id, &cookies)
            .await
            .context("FFmpeg 退出后查询直播间状态失败")?;
        Ok(!init.is_live())
    }

    async fn recover_after_unexpected_exit(&mut self) -> bool {
        if let Some(next) = self.next_recovery_at {
            if Instant::now() < next {
                return false;
            }
        }
        if self.restart_attempts < MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS {
            self.restart_attempts += 1;
            warn!(
                room_id = self.room_id,
                attempt = self.restart_attempts,
                "FFmpeg 异常退出，尝试刷新直播流并继续新分段"
            );
            let backoff = recovery_backoff(self.restart_attempts);
            match self.refresh_segment().await {
                Ok(()) => {
                    self.unexpected_exit_detail = None;
                    // 新分段可能同样秒退：退避后再允许下一次恢复。
                    self.next_recovery_at = Some(Instant::now() + backoff);
                    return false;
                }
                Err(error) => {
                    warn!(
                        room_id = self.room_id,
                        attempt = self.restart_attempts,
                        "启动恢复分段失败: {error}"
                    );
                    self.next_recovery_at = Some(Instant::now() + backoff);
                }
            }
        }

        self.stop_reason = Some(STOP_REASON_UNRECOVERABLE_EXIT);
        self.is_recoverable = true;
        let detail = self
            .unexpected_exit_detail
            .take()
            .unwrap_or_else(|| "FFmpeg 异常退出，且无法恢复录制".to_owned());
        self.mark_failure(detail).await;
        true
    }

    pub(super) async fn refresh_segment(&mut self) -> Result<()> {
        let cookies = self.settings_service.cookie_header().await?;
        let max_qn = source_max_qn(&self.db, self.room_id).await;
        let playurl = self
            .bili_api
            .live_playurl(self.room_id, max_qn, &cookies)
            .await
            .context("刷新直播流地址失败")?;
        let candidates = select_stream_candidates(&playurl.durl)?;
        // 分段切换时优先选择与当前录制相同的容器/编码组合：concat 合并无法
        // 混合不同格式的分段。仅在同格式线路都不可用时才跨格式降级。
        let (current_format, current_codec) = {
            let snapshot = self.snapshot.lock().await;
            (
                snapshot.stream_format.clone(),
                snapshot.stream_codec.clone(),
            )
        };
        let same_profile = |stream: &LiveStreamUrl| {
            current_format
                .as_deref()
                .is_none_or(|format| stream.format_name.eq_ignore_ascii_case(format))
                && current_codec
                    .as_deref()
                    .is_none_or(|codec| stream.codec_name.eq_ignore_ascii_case(codec))
        };
        let selected_index = candidates
            .iter()
            .enumerate()
            .find(|(_, stream)| stream.url != self.current_url && same_profile(stream))
            .map(|(index, _)| index)
            .or_else(|| {
                candidates
                    .iter()
                    .enumerate()
                    .find(|(_, stream)| stream.url != self.current_url)
                    .map(|(index, _)| index)
            })
            .unwrap_or_else(|| {
                if candidates.is_empty() {
                    0
                } else {
                    (self.candidate_index + 1) % candidates.len()
                }
            });
        let selected_stream = candidates
            .get(selected_index)
            .cloned()
            .ok_or_else(|| anyhow!("流地址列表为空"))?;
        self.stream_candidates = candidates;
        self.candidate_index = selected_index;
        let new_url = selected_stream.url.clone();
        let new_path = segment_path(&self.live_dir, &self.base_prefix, self.next_segment);

        // 先结束旧分段再创建新进程，避免刷新 URL 时产生重叠录制窗口。
        if let Some(mut old_ffmpeg) = self.current_ffmpeg.take() {
            if let Err(error) = old_ffmpeg.stop_with_timeout(super::STOP_TIMEOUT).await {
                warn!(
                    room_id = self.room_id,
                    "停止旧直播分段失败，已强制回收: {error}"
                );
            }
        }
        let new_ffmpeg = super::ffmpeg_session::FfmpegSession::start(
            &self.ffmpeg_path,
            &new_url,
            new_path.clone(),
            self.room_id,
            user_agent(),
            referer(),
        )?;

        self.current_ffmpeg = Some(new_ffmpeg);
        self.current_url = new_url;
        {
            let mut snapshot = self.snapshot.lock().await;
            snapshot.stream_quality =
                (selected_stream.current_qn > 0).then_some(selected_stream.current_qn);
            snapshot.stream_protocol = (!selected_stream.protocol_name.is_empty())
                .then_some(selected_stream.protocol_name);
            snapshot.stream_format =
                (!selected_stream.format_name.is_empty()).then_some(selected_stream.format_name);
            snapshot.stream_codec =
                (!selected_stream.codec_name.is_empty()).then_some(selected_stream.codec_name);
        }
        let segment_number = self.next_segment;
        self.segments.push(new_path.clone());
        if let Err(error) = persist_segment(
            &self.db,
            self.recording_id,
            segment_number,
            &new_path,
            "open",
        )
        .await
        {
            warn!(
                room_id = self.room_id,
                "persist refreshed recording segment failed: {error}"
            );
        }
        self.segment_index.fetch_add(1, Ordering::Relaxed);
        self.next_segment = self.next_segment.saturating_add(1);
        self.last_url_refresh = Instant::now();
        info!(
            room_id = self.room_id,
            segment = self.next_segment - 1,
            "直播流已切换到新分段"
        );
        Ok(())
    }
}
