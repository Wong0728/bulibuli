//! L1/L2 worker 主循环：按各自间隔（带毫秒抖动）醒来，读 Cookie 后经风控闸门
//! 执行一轮刷新；取消令牌触发即退出，由 `stop` 统一 join。

use super::gate::{run_refresh, RefreshGate};
use super::tasks::{refresh_all_bloggers_report, refresh_video_stats};
use super::{DEFAULT_L1_INTERVAL_MINUTES, L2_INTERVAL};
use crate::services::bili_api::BiliApi;
use crate::services::settings::SettingsService;
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

/// L1 worker 主循环。
pub(super) async fn await_worker(name: &'static str, handle: tokio::task::JoinHandle<()>) {
    // 这些调用点的名称是固定字面量；统一 helper 在超时后 abort 并 await，
    // 不再把仍访问数据库的 worker 通过丢弃 JoinHandle 带入关库阶段。
    crate::services::spawn_util::wait_join_handle(name, handle, StdDuration::from_secs(10)).await;
}

pub(super) async fn l1_loop(
    db: DatabaseConnection,
    bili_api: Arc<BiliApi>,
    settings_service: Arc<SettingsService>,
    cancellation: CancellationToken,
    gate: RefreshGate,
) {
    loop {
        let interval_minutes = read_l1_interval(&db).await;
        let jitter = chrono::Local::now().timestamp_subsec_millis() as u64 % 15;
        let interval = StdDuration::from_secs(interval_minutes * 60 + jitter);
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = tokio::time::sleep(interval) => {}
        }
        let cookies = read_cookies(&settings_service).await;
        if let Err(e) = run_refresh(
            &gate,
            "video",
            &cookies,
            refresh_video_stats(&db, &bili_api, &cookies),
        )
        .await
        {
            error!("[refresh L1] 出错: {e}");
        }
    }
}

/// L2 worker 主循环。
pub(super) async fn l2_loop(
    db: DatabaseConnection,
    bili_api: Arc<BiliApi>,
    settings_service: Arc<SettingsService>,
    cancellation: CancellationToken,
    gate: RefreshGate,
) {
    // 启动后立即补齐缺失的博主资料。
    {
        let cookies = read_cookies(&settings_service).await;
        if let Err(e) = run_refresh(
            &gate,
            "user",
            &cookies,
            refresh_all_bloggers_report(&db, &bili_api, &cookies),
        )
        .await
        {
            error!("[refresh L2] 启动刷新出错: {e}");
        }
    }
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = tokio::time::sleep(L2_INTERVAL + StdDuration::from_secs(chrono::Local::now().timestamp_subsec_millis() as u64 % 60)) => {}
        }
        let cookies = read_cookies(&settings_service).await;
        if let Err(e) = run_refresh(
            &gate,
            "user",
            &cookies,
            refresh_all_bloggers_report(&db, &bili_api, &cookies),
        )
        .await
        {
            error!("[refresh L2] 出错: {e}");
        }
    }
}

/// 读取 Cookie（DB key="cookies"）。
pub(super) async fn read_cookies(settings_service: &SettingsService) -> String {
    match settings_service.cookie_header().await {
        Ok(cookies) => cookies,
        Err(error) => {
            warn!(%error, "读取 Cookie 失败");
            String::new()
        }
    }
}

/// 读取 L1 间隔（分钟），默认 5。
async fn read_l1_interval(db: &DatabaseConnection) -> u64 {
    match crate::services::settings::all_settings(db).await {
        Ok(s) => s
            .get("refresh")
            .and_then(|r| r.get("l1_interval_minutes"))
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_L1_INTERVAL_MINUTES),
        Err(_) => DEFAULT_L1_INTERVAL_MINUTES,
    }
}
