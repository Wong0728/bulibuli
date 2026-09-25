//! 直播自动录制监控：轮询直播源房间状态，联动录制器的启动/停止。
//!
//! 模块组织：
//! - 本文件：公开类型（运行时快照 / 健康状态）、生命周期与唤醒入口
//! - `checks`：到点检查、录制联动、错误标记与全局风控退避

mod checks;

use crate::services::bili_api::BiliApi;
use crate::services::live_recorder::LiveRecorder;
use crate::services::live_source::LiveSourceService;
use crate::services::settings::SettingsService;
use crate::services::spawn_util::wait_join_handle;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::info;

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
            wait_join_handle("live_monitor", handle, Duration::from_secs(5)).await;
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
}
