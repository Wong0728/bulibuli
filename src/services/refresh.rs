//! 拉取频率分层 worker：
//! - L1（默认 5min）：抽 50 条 `state in (completed, pay_blocked, removed)` 最久未刷的视频，
//!   调 `get_video_info` 写回 `view` 与 `state`。
//! - L2（24h）：拉所有博主，调 `get_user_info` 写回 `face/sign/level/name`。
//!
//! 请求采用最多 4 路有界并发，并继续经过 `BiliApi` 的统一全局限流器。
//! 子模块分工：`gate` 风控冷却闸门，`workers` L1/L2 主循环，`tasks` 具体刷新任务。

mod gate;
mod tasks;
mod workers;

use crate::services::bili_api::BiliApi;
use crate::services::settings::SettingsService;
use sea_orm::DatabaseConnection;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// L1 worker 单次拉取的最大视频数。
const L1_BATCH: u64 = 50;
/// 默认 L1 间隔（分钟）。
const DEFAULT_L1_INTERVAL_MINUTES: u64 = 5;
/// L2 间隔（24h）。
const L2_INTERVAL: StdDuration = StdDuration::from_secs(24 * 3600);

/// 手动可触发的两类全量扫描。
enum ManualSweep {
    Board,
    Bloggers,
}

/// 手动刷新的结果。`skipped` 说明本轮被主动跳过的原因——跳过不是失败，
/// 但只回一个 0 会让前端显示"已刷新 0 条"，用户不知道发生了什么。
pub struct ManualRefreshOutcome {
    pub refreshed: usize,
    pub skipped: Option<&'static str>,
}

/// 释放手动刷新单飞标记；`?` 提前返回时同样要释放。
struct ManualRefreshGuard(Arc<AtomicBool>);

impl Drop for ManualRefreshGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub struct RefreshService {
    db: DatabaseConnection,
    bili_api: Arc<BiliApi>,
    settings_service: Arc<SettingsService>,
    l1_handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    l2_handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    cancellation: CancellationToken,
    /// 与 L1/L2 worker 共用的风险闸门：手动触发也必须过它。
    gate: gate::RefreshGate,
    /// 手动刷新单飞标记：连点不再叠出多轮全量 sweep。
    manual_busy: Arc<AtomicBool>,
}

impl RefreshService {
    pub fn new(
        db: DatabaseConnection,
        bili_api: Arc<BiliApi>,
        settings_service: Arc<SettingsService>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            db,
            bili_api,
            settings_service,
            l1_handle: Arc::new(tokio::sync::Mutex::new(None)),
            l2_handle: Arc::new(tokio::sync::Mutex::new(None)),
            cancellation,
            gate: gate::RefreshGate::default(),
            manual_busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 启动 L1 + L2 worker。
    pub async fn start(&self) {
        self.start_l1(self.gate.clone()).await;
        self.start_l2(self.gate.clone()).await;
    }

    /// 停止 L1 + L2 worker。
    pub async fn stop(&self) {
        self.cancellation.cancel();
        if let Some(h) = self.l1_handle.lock().await.take() {
            workers::await_worker("refresh L1", h).await;
        }
        if let Some(h) = self.l2_handle.lock().await.take() {
            workers::await_worker("refresh L2", h).await;
        }
        info!("[refresh] L1/L2 worker 已停止");
    }

    async fn start_l1(&self, gate: gate::RefreshGate) {
        if self.l1_handle.lock().await.is_some() {
            return;
        }
        info!("[refresh] L1 worker 已启动");
        let db = self.db.clone();
        let bili_api = self.bili_api.clone();
        let settings_service = self.settings_service.clone();
        let cancellation = self.cancellation.child_token();
        let handle = tokio::spawn(async move {
            workers::l1_loop(db, bili_api, settings_service, cancellation, gate).await;
        });
        *self.l1_handle.lock().await = Some(handle);
    }

    async fn start_l2(&self, gate: gate::RefreshGate) {
        if self.l2_handle.lock().await.is_some() {
            return;
        }
        info!("[refresh] L2 worker 已启动");
        let db = self.db.clone();
        let bili_api = self.bili_api.clone();
        let settings_service = self.settings_service.clone();
        let cancellation = self.cancellation.child_token();
        let handle = tokio::spawn(async move {
            workers::l2_loop(db, bili_api, settings_service, cancellation, gate).await;
        });
        *self.l2_handle.lock().await = Some(handle);
    }

    /// 手动触发 L1 刷新（POST /api/refresh?kind=board 用）。
    pub async fn trigger_l1(&self) -> anyhow::Result<ManualRefreshOutcome> {
        self.run_manual(ManualSweep::Board).await
    }

    /// 手动触发 L2 刷新（POST /api/refresh?kind=blogger 用）。
    pub async fn trigger_l2(&self) -> anyhow::Result<ManualRefreshOutcome> {
        self.run_manual(ManualSweep::Bloggers).await
    }

    /// 手动触发的一轮刷新：与 worker 走同一道风险闸门，并受单飞保护。
    ///
    /// 此前手动路径直接打全量：连点会叠出多轮 sweep（每轮请求量随记录数线性），
    /// 把配额刷满后换来更长的风控，而且完全绕过 worker 已建立的冷却。
    async fn run_manual(&self, sweep: ManualSweep) -> anyhow::Result<ManualRefreshOutcome> {
        let endpoint = match sweep {
            ManualSweep::Board => "video",
            ManualSweep::Bloggers => "user",
        };
        if self.manual_busy.swap(true, Ordering::SeqCst) {
            return Ok(ManualRefreshOutcome {
                refreshed: 0,
                skipped: Some("已有刷新任务在进行中，本次未重复触发"),
            });
        }
        let _guard = ManualRefreshGuard(self.manual_busy.clone());
        let cookies = workers::read_cookies(&self.settings_service).await;
        if self.gate.is_blocked(endpoint, &cookies).await {
            tracing::warn!("[refresh] 手动 {endpoint} 刷新被风控冷却挡下，本轮未发起请求");
            return Ok(ManualRefreshOutcome {
                refreshed: 0,
                skipped: Some("B 站接口处于风控冷却中，本次已跳过"),
            });
        }
        let report = match sweep {
            ManualSweep::Board => {
                tasks::refresh_video_stats(&self.db, &self.bili_api, &cookies).await?
            }
            ManualSweep::Bloggers => {
                tasks::refresh_all_bloggers_report(&self.db, &self.bili_api, &cookies).await?
            }
        };
        self.gate.record(endpoint, &cookies, report).await;
        Ok(ManualRefreshOutcome {
            refreshed: report.success,
            skipped: None,
        })
    }

    /// 手动触发单个视频刷新（POST /api/refresh?kind=video 用）。
    pub async fn trigger_video(&self, bvid: &str) -> crate::error::AppResult<()> {
        let cookies = workers::read_cookies(&self.settings_service).await;
        tasks::refresh_single_video(&self.db, &self.bili_api, bvid, &cookies).await
    }
}
