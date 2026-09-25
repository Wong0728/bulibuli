//! 直播源到点检查与结果处理：批量拉取房间状态、下播自动停止 / 开播自动启动录制、
//! 错误标记与全局风控退避（含退避衰减）。

use crate::error::{BiliApiError, BiliErrorKind};
use crate::models::live_source;
use crate::services::live_recorder::RecordingTrigger;
use crate::services::live_source::{next_schedule_start, schedule_is_active, CaptureMode};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::LiveMonitor;

impl LiveMonitor {
    pub(super) async fn check_due(&self) {
        self.inner.health.lock().await.last_heartbeat_at = Some(chrono::Utc::now().to_rfc3339());
        {
            let risk = self.inner.risk_backoff.lock().await;
            if risk.until.is_some_and(|until| until > Instant::now()) {
                drop(risk);
                self.mark_all_stale("B站状态检查处于风控退避".to_owned())
                    .await;
                return;
            }
        }
        let sources = match self.inner.source_service.list().await {
            Ok(value) => value,
            Err(error) => {
                warn!("读取直播源失败: {error}");
                self.inner.health.lock().await.last_error = Some(error.to_string());
                return;
            }
        };
        let ids = sources
            .iter()
            .map(|source| source.room_id)
            .collect::<std::collections::HashSet<_>>();
        let due = {
            let mut runtime = self.inner.runtime.lock().await;
            runtime.retain(|id, _| ids.contains(id));
            let now = Instant::now();
            for source in &sources {
                runtime
                    .entry(source.room_id)
                    .or_insert_with(|| super::RuntimeEntry {
                        public: super::LiveSourceRuntime {
                            room_id: source.room_id,
                            live_status: None,
                            last_checked_at: None,
                            error: None,
                            risk_limited: false,
                            schedule_active: schedule_is_active(
                                source.weekly_schedule.as_deref(),
                                chrono::Local::now(),
                            ),
                            schedule_overrun: false,
                            next_retry_at: None,
                            next_schedule_at: None,
                            stale: false,
                        },
                        next_due: now + Duration::from_secs(source.room_id.unsigned_abs() % 30),
                    });
            }
            sources
                .into_iter()
                .filter(|source| {
                    runtime
                        .get(&source.room_id)
                        .is_some_and(|entry| entry.next_due <= now)
                })
                .collect::<Vec<_>>()
        };
        if due.is_empty() {
            return;
        }
        let cookies = match self.inner.settings_service.cookie_header().await {
            Ok(value) => value,
            Err(error) => {
                self.inner.health.lock().await.last_error = Some(error.to_string());
                self.mark_batch_error(&due, error.to_string(), false).await;
                return;
            }
        };
        let batch = self
            .inner
            .bili_api
            .live_status_by_uids(
                &due.iter().map(|source| source.uid).collect::<Vec<_>>(),
                &cookies,
            )
            .await;
        match batch {
            Ok(statuses) => {
                let mut health = self.inner.health.lock().await;
                health.last_success_at = Some(chrono::Utc::now().to_rfc3339());
                health.last_error = None;
                drop(health);
                self.note_successful_batch().await;
                for source in due {
                    if let Some(status) = statuses.get(&source.uid) {
                        self.handle_success(source, status.live_status, status.live_time)
                            .await;
                    } else {
                        self.mark_error(
                            source.room_id,
                            "批量状态响应未包含该直播源".to_owned(),
                            false,
                        )
                        .await;
                    }
                }
            }
            Err(error) => {
                let message = error.to_string();
                self.inner.health.lock().await.last_error = Some(message.clone());
                let limited = is_risk_error(&error);
                self.mark_batch_error(&due, message.clone(), limited).await;
                if limited {
                    self.activate_risk_backoff(message).await;
                }
            }
        }
    }

    async fn handle_success(&self, source: live_source::Model, live_status: i32, live_time: i64) {
        let active = schedule_is_active(source.weekly_schedule.as_deref(), chrono::Local::now());
        let recording = self
            .inner
            .live_recorder
            .status(source.room_id)
            .await
            .is_some();
        {
            let mut runtime = self.inner.runtime.lock().await;
            if let Some(entry) = runtime.get_mut(&source.room_id) {
                let jitter = source.room_id.unsigned_abs().wrapping_mul(2_654_435_761) % 11 + 25;
                entry.next_due = Instant::now() + Duration::from_secs(jitter);
                entry.public.live_status = Some(live_status);
                entry.public.last_checked_at = Some(chrono::Utc::now().to_rfc3339());
                entry.public.error = None;
                entry.public.risk_limited = false;
                entry.public.stale = false;
                entry.public.next_retry_at = None;
                entry.public.next_schedule_at =
                    next_schedule_start(source.weekly_schedule.as_deref(), chrono::Local::now())
                        .map(|value| value.to_rfc3339());
                entry.public.schedule_active = active;
                entry.public.schedule_overrun = live_status == 1 && recording && !active;
            }
        }
        if live_status != 1 {
            if source.manual_stop_latched || source.manual_stop_session_key.is_some() {
                // 清除人工停播闩锁是 best-effort：失败时下个周期会再次尝试，不影响录制状态机。
                if let Err(error) = self
                    .inner
                    .source_service
                    .set_manual_stop_session(source.room_id, None)
                    .await
                {
                    debug!(room_id = source.room_id, "清除人工停播闩锁失败: {error}");
                }
            }
            if recording {
                info!(room_id = source.room_id, "检测到下播，自动停止录制");
                // 用异步收尾路径：合并等耗时操作在后台进行，不阻塞其它房间的开播检测。
                if let Err(error) = self.inner.live_recorder.request_stop(source.room_id).await {
                    let message = format!("{error:#}");
                    // 会话刚被人工停止 / 已在收尾 / 启动期取消，都不算自动停止失败。
                    if message.contains("not found")
                        || message.contains("no longer active")
                        || message.contains("cancellation requested")
                    {
                        debug!(room_id = source.room_id, "自动停止录制：会话已不在录制中");
                    } else {
                        warn!(room_id = source.room_id, "自动停止录制失败: {error}");
                    }
                }
            }
            return;
        }
        let session_key = (live_time > 0).then(|| format!("{}:{live_time}", source.room_id));
        let manually_stopped = (source.manual_stop_session_key.is_some()
            && source.manual_stop_session_key == session_key)
            || (source.manual_stop_latched && source.manual_stop_session_key.is_none());
        if !recording && source.auto_record_enabled && active && !manually_stopped {
            let mode = CaptureMode::parse(&source.capture_mode).unwrap_or_default();
            info!(room_id = source.room_id, "检测到开播，自动开始录制");
            if let Err(error) = self
                .inner
                .live_recorder
                .start_with_options(source.room_id, RecordingTrigger::Auto, mode)
                .await
            {
                warn!(room_id = source.room_id, "自动录制启动失败: {error}");
            }
        }
    }

    async fn mark_error(&self, room_id: i64, message: String, limited: bool) {
        let mut runtime = self.inner.runtime.lock().await;
        if let Some(entry) = runtime.get_mut(&room_id) {
            entry.next_due = Instant::now() + Duration::from_secs(30);
            entry.public.last_checked_at = Some(chrono::Utc::now().to_rfc3339());
            entry.public.error = Some(message);
            entry.public.risk_limited = limited;
            entry.public.stale = true;
            entry.public.next_retry_at =
                Some((chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339());
        }
    }

    async fn mark_batch_error(
        &self,
        sources: &[live_source::Model],
        message: String,
        limited: bool,
    ) {
        for source in sources {
            self.mark_error(source.room_id, message.clone(), limited)
                .await;
        }
    }

    async fn activate_risk_backoff(&self, message: String) {
        let mut risk = self.inner.risk_backoff.lock().await;
        let delays = [60_u64, 120, 300];
        let delay = delays[risk.level.min(delays.len() - 1)];
        risk.level = (risk.level + 1).min(delays.len() - 1);
        risk.until = Some(Instant::now() + Duration::from_secs(delay));
        risk.reason = Some(format!("B站状态检查受限，{delay} 秒后重试：{message}"));
        risk.successful_batches = 0;
        warn!(delay, "B站直播状态检查受限，启用全局退避");
    }

    /// 退避衰减：连续 2 批成功检查后降低一级退避等级；等级归零后彻底清除退避。
    /// 注意衰减过程不能反过来设置新的退避窗口，否则成功检查之后会把所有
    /// 直播源误标为“风控退避中”。
    async fn note_successful_batch(&self) {
        let mut risk = self.inner.risk_backoff.lock().await;
        risk.successful_batches = risk.successful_batches.saturating_add(1);
        if risk.level == 0 || risk.successful_batches < 2 {
            return;
        }
        risk.successful_batches = 0;
        risk.level = risk.level.saturating_sub(1);
        if risk.level == 0 {
            risk.until = None;
            risk.reason = None;
        }
    }

    async fn mark_all_stale(&self, message: String) {
        self.inner.health.lock().await.last_error = Some(message.clone());
        let mut runtime = self.inner.runtime.lock().await;
        for entry in runtime.values_mut() {
            entry.public.stale = true;
            entry.public.risk_limited = true;
            entry.public.error = Some(message.clone());
            entry.next_due = Instant::now() + Duration::from_secs(60);
            entry.public.next_retry_at =
                Some((chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339());
        }
    }
}

fn is_risk_error(error: &anyhow::Error) -> bool {
    // 风控码集合以 `BiliApiError::classify` 的 code→kind 映射为唯一来源，不在此复制；
    // -101 属登录失效（Unauthorized），全局退避无法修复，因此不参与退避。
    if let Some(bili) = error.downcast_ref::<BiliApiError>() {
        return matches!(
            bili.kind,
            BiliErrorKind::RiskControl | BiliErrorKind::RateLimited
        );
    }
    // 非结构化错误（网络层 context 包装、CookieManager 文案）退回文案匹配。
    let message = error.to_string();
    message.contains("-352")
        || message.contains("-403")
        || message.contains("-799")
        || message.contains("412")
        || message.contains("429")
        || message.to_ascii_lowercase().contains("riskcontrol")
}

#[cfg(test)]
mod tests {
    use super::is_risk_error;
    use crate::error::BiliApiError;

    fn typed(code: i64) -> anyhow::Error {
        anyhow::Error::new(BiliApiError::classify(code, "upstream rejected"))
    }

    #[test]
    fn risk_control_codes_activate_global_backoff() {
        for code in [-352, -403, -412, -799] {
            assert!(is_risk_error(&typed(code)), "code {code} 应触发退避");
        }
    }

    #[test]
    fn rate_limited_response_activates_global_backoff() {
        assert!(is_risk_error(&typed(429)));
    }

    #[test]
    fn auth_and_business_errors_do_not_activate_backoff() {
        assert!(!is_risk_error(&typed(-101)));
        assert!(!is_risk_error(&typed(-404)));
        assert!(!is_risk_error(&typed(503)));
        assert!(!is_risk_error(&anyhow::anyhow!(
            "批量状态响应未包含该直播源"
        )));
    }

    #[test]
    fn untyped_errors_fall_back_to_message_matching() {
        assert!(is_risk_error(&anyhow::anyhow!("B站API返回HTTP 412")));
        assert!(is_risk_error(&anyhow::anyhow!("触发 RiskControl 校验")));
        assert!(!is_risk_error(&anyhow::anyhow!("发送 B站 API 请求失败")));
    }
}
