use crate::models::live_source;
use crate::services::bili_api::BiliApi;
use crate::services::live_recorder::{LiveRecorder, RecordingTrigger};
use crate::services::live_source::{
    next_schedule_start, schedule_is_active, CaptureMode, LiveSourceService,
};
use crate::services::settings::SettingsService;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[derive(Clone, Debug, Serialize)]
pub struct LiveSourceRuntime {
    pub room_id: i64,
    pub live_status: Option<i32>,
    pub last_checked_at: Option<String>,
    pub error: Option<String>,
    pub risk_limited: bool,
    pub schedule_active: bool,
    pub schedule_overrun: bool,
    pub next_retry_at: Option<String>,
    pub next_schedule_at: Option<String>,
    pub stale: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LiveMonitorHealth {
    pub running: bool,
    pub last_heartbeat_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub risk_backoff_until: Option<String>,
}

struct RuntimeEntry {
    public: LiveSourceRuntime,
    next_due: Instant,
}

#[derive(Default)]
struct RiskBackoff {
    level: usize,
    until: Option<Instant>,
    reason: Option<String>,
    successful_batches: u32,
}

#[derive(Default)]
struct MonitorHealthState {
    last_heartbeat_at: Option<String>,
    last_success_at: Option<String>,
    last_error: Option<String>,
}

#[derive(Clone)]
pub struct LiveMonitor {
    inner: Arc<LiveMonitorInner>,
}

struct LiveMonitorInner {
    bili_api: Arc<BiliApi>,
    live_recorder: Arc<LiveRecorder>,
    source_service: Arc<LiveSourceService>,
    settings_service: Arc<SettingsService>,
    runtime: Mutex<HashMap<i64, RuntimeEntry>>,
    risk_backoff: Mutex<RiskBackoff>,
    notify: Notify,
    cancellation: CancellationToken,
    handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    health: Mutex<MonitorHealthState>,
}

impl LiveMonitor {
    pub fn new(
        bili_api: Arc<BiliApi>,
        live_recorder: Arc<LiveRecorder>,
        source_service: Arc<LiveSourceService>,
        settings_service: Arc<SettingsService>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            inner: Arc::new(LiveMonitorInner {
                bili_api,
                live_recorder,
                source_service,
                settings_service,
                runtime: Mutex::new(HashMap::new()),
                risk_backoff: Mutex::new(RiskBackoff::default()),
                notify: Notify::new(),
                cancellation,
                handle: Mutex::new(None),
                health: Mutex::new(MonitorHealthState::default()),
            }),
        }
    }

    pub async fn start(&self) {
        let mut handle = self.inner.handle.lock().await;
        if handle.is_some() {
            return;
        }
        let this = self.clone();
        *handle = Some(tokio::spawn(async move {
            this.run_loop().await;
        }));
        info!("直播自动录制监控已启动，基础检查间隔 30 秒");
    }

    pub async fn stop(&self) {
        self.inner.cancellation.cancel();
        if let Some(handle) = self.inner.handle.lock().await.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
        }
        info!("直播自动录制监控已停止");
    }

    pub async fn wake_room(&self, room_id: i64) {
        let mut runtime = self.inner.runtime.lock().await;
        runtime
            .entry(room_id)
            .and_modify(|entry| entry.next_due = Instant::now())
            .or_insert_with(|| RuntimeEntry {
                public: LiveSourceRuntime {
                    room_id,
                    live_status: None,
                    last_checked_at: None,
                    error: None,
                    risk_limited: false,
                    schedule_active: true,
                    schedule_overrun: false,
                    next_retry_at: None,
                    next_schedule_at: None,
                    stale: false,
                },
                next_due: Instant::now(),
            });
        drop(runtime);
        self.inner.notify.notify_one();
    }

    pub async fn runtime_snapshot(&self) -> HashMap<i64, LiveSourceRuntime> {
        self.inner
            .runtime
            .lock()
            .await
            .iter()
            .map(|(id, value)| (*id, value.public.clone()))
            .collect()
    }

    pub async fn risk_snapshot(&self) -> Option<String> {
        let risk = self.inner.risk_backoff.lock().await;
        risk.until
            .filter(|until| *until > Instant::now())
            .and_then(|_| risk.reason.clone())
    }

    pub async fn health_snapshot(&self) -> LiveMonitorHealth {
        let running = self
            .inner
            .handle
            .lock()
            .await
            .as_ref()
            .is_some_and(|handle| !handle.is_finished());
        let health = self.inner.health.lock().await;
        let risk = self.inner.risk_backoff.lock().await;
        let risk_backoff_until = risk.until.and_then(|until| {
            until
                .checked_duration_since(Instant::now())
                .map(|remaining| {
                    (chrono::Utc::now() + chrono::Duration::from_std(remaining).unwrap_or_default())
                        .to_rfc3339()
                })
        });
        LiveMonitorHealth {
            running,
            last_heartbeat_at: health.last_heartbeat_at.clone(),
            last_success_at: health.last_success_at.clone(),
            last_error: health.last_error.clone(),
            risk_backoff_until,
        }
    }

    async fn run_loop(&self) {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = self.inner.cancellation.cancelled() => return,
                _ = self.inner.notify.notified() => self.check_due().await,
                _ = tick.tick() => self.check_due().await,
            }
        }
    }

    async fn check_due(&self) {
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
                    .or_insert_with(|| RuntimeEntry {
                        public: LiveSourceRuntime {
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
                let limited = is_risk_error(&message);
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
                let _ = self
                    .inner
                    .source_service
                    .set_manual_stop_session(source.room_id, None)
                    .await;
            }
            if recording {
                info!(room_id = source.room_id, "检测到下播，自动停止录制");
                if let Err(error) = self.inner.live_recorder.stop(source.room_id).await {
                    warn!(room_id = source.room_id, "自动停止录制失败: {error}");
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

fn is_risk_error(message: &str) -> bool {
    message.contains("-352")
        || message.contains("429")
        || message.contains("412")
        || message.to_ascii_lowercase().contains("riskcontrol")
}
