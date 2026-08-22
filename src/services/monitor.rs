//! 博主监控服务：定时检查博主新视频并驱动自动下载管线。
//!
//! 子模块职责：
//! - `active_window`：博主活跃检查时段（闹钟式窗口）解析与调度计算
//! - `blogger_check`：单个博主的视频扫描、下架/重投检测与资料变更检测
//! - `video_window`：视频窗口截取与标题相似度（重投检测）工具函数
//! - `video_queue`：新视频入队与弹幕/评论自动下载
//! - `paywall`：充电/付费前置校验与 pay_blocked 记录落库
//! - `scheduled_tasks`：计划弹幕下载、自动烧录与下次检查调度
//! - `logging`：监控日志写入、查询与清理

mod active_window;
mod blogger_check;
mod logging;
mod paywall;
mod scheduled_helpers;
mod scheduled_tasks;
mod video_queue;
mod video_window;

pub(crate) use active_window::{
    is_active as is_within_active_window, next_window_start, normalize_windows, parse_windows,
    schedule_snapshot,
};
pub(crate) use paywall::pay_reason_to_state;

use crate::config::AppConfig;
use crate::models::blogger;
use crate::services::{
    bili_api::BiliApi, blogger::BloggerService, danmaku::DanmakuService, download::DownloadManager,
    history::HistoryService, settings::SettingsService, spawn_util::spawn_logged,
    subtitle_fetch::SubtitleFetchService, video_processor::VideoProcessor,
};
use anyhow::Result;
use chrono::Local;
use futures::{stream, StreamExt};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::{RwLock, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// settings 缓存有效期（10 秒），避免单次博客检查周期内 5+ 次全表查询
const SETTINGS_CACHE_TTL: StdDuration = StdDuration::from_secs(10);
/// 每写入多少条日志触发一次日志清理
const LOG_CLEANUP_INTERVAL: u64 = 100;

#[derive(Clone)]
pub struct MonitorService {
    config: Arc<AppConfig>,
    db: DatabaseConnection,
    bili_api: Arc<BiliApi>,
    download_manager: Arc<DownloadManager>,
    danmaku_service: Arc<DanmakuService>,
    subtitle_service: Arc<SubtitleFetchService>,
    blogger_service: Arc<BloggerService>,
    video_processor: Arc<VideoProcessor>,
    history_service: Arc<HistoryService>,
    settings_service: Arc<SettingsService>,
    burn_semaphore: Arc<Semaphore>,
    handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    cancellation: CancellationToken,
    settings_cache: Arc<RwLock<Option<(Value, Instant)>>>,
    log_counter: Arc<AtomicU64>,
    auto_burn_in_progress: Arc<tokio::sync::Mutex<HashSet<i32>>>,
    scheduled_sidecar_in_progress: Arc<tokio::sync::Mutex<HashSet<i32>>>,
    sidecar_semaphore: Arc<Semaphore>,
}

/// MonitorService 构造所需的依赖集合，与 DownloadManagerDependencies 模式一致。
pub struct MonitorServiceDependencies {
    pub config: Arc<AppConfig>,
    pub db: DatabaseConnection,
    pub bili_api: Arc<BiliApi>,
    pub download_manager: Arc<DownloadManager>,
    pub danmaku_service: Arc<DanmakuService>,
    pub subtitle_service: Arc<SubtitleFetchService>,
    pub blogger_service: Arc<BloggerService>,
    pub video_processor: Arc<VideoProcessor>,
    pub history_service: Arc<HistoryService>,
    pub settings_service: Arc<SettingsService>,
    pub burn_semaphore: Arc<Semaphore>,
    pub cancellation: CancellationToken,
}

impl MonitorService {
    pub async fn new(deps: MonitorServiceDependencies) -> Self {
        Self {
            config: deps.config,
            db: deps.db,
            bili_api: deps.bili_api,
            download_manager: deps.download_manager,
            danmaku_service: deps.danmaku_service,
            subtitle_service: deps.subtitle_service,
            blogger_service: deps.blogger_service,
            video_processor: deps.video_processor,
            history_service: deps.history_service,
            settings_service: deps.settings_service,
            burn_semaphore: deps.burn_semaphore,
            handle: Arc::new(tokio::sync::Mutex::new(None)),
            cancellation: deps.cancellation,
            settings_cache: Arc::new(RwLock::new(None)),
            log_counter: Arc::new(AtomicU64::new(0)),
            auto_burn_in_progress: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            scheduled_sidecar_in_progress: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            sidecar_semaphore: Arc::new(Semaphore::new(2)),
        }
    }

    pub async fn start(&self) {
        if self.handle.lock().await.is_some() {
            return;
        }
        info!("监控服务已启动");
        let s = self.clone();
        let handle = tokio::spawn(async move {
            s.run().await;
        });
        *self.handle.lock().await = Some(handle);

        // 后台补齐 fans 为空的博主资料。
        let s = self.clone();
        spawn_logged("monitor_backfill_fans", async move {
            s.backfill_missing_fans().await;
        });
    }

    pub async fn stop(&self) {
        self.cancellation.cancel();
        if let Some(h) = self.handle.lock().await.take() {
            match tokio::time::timeout(StdDuration::from_secs(10), h).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => error!("监控任务退出异常: {error}"),
                Err(_) => error!("监控任务未在 10 秒内退出"),
            }
        }
        info!("监控服务已停止");
    }

    async fn run(&self) {
        loop {
            if let Err(e) = self.check_and_execute().await {
                error!("监控循环出错: {e}");
            }
            tokio::select! {
                _ = self.cancellation.cancelled() => break,
                _ = tokio::time::sleep(StdDuration::from_secs(10)) => {}
            }
        }
    }

    async fn check_and_execute(&self) -> Result<()> {
        let now = Local::now();
        let bloggers = blogger::Entity::find()
            .filter(blogger::Column::IsRunning.eq(true))
            .filter(blogger::Column::NextCheck.lte(now))
            .all(&self.db)
            .await?;

        if !bloggers.is_empty() {
            info!("发现 {} 个博主需要检查", bloggers.len());
        }

        stream::iter(bloggers)
            .for_each_concurrent(4, |blogger| async move {
                let claimed = blogger::Entity::update_many()
                    .col_expr(
                        blogger::Column::NextCheck,
                        Expr::value(Local::now() + chrono::Duration::minutes(5)),
                    )
                    .filter(blogger::Column::Id.eq(blogger.id))
                    .filter(blogger::Column::IsRunning.eq(true))
                    .filter(blogger::Column::NextCheck.lte(Local::now()))
                    .exec(&self.db)
                    .await
                    .map(|result| result.rows_affected == 1)
                    .unwrap_or(false);
                if !claimed {
                    return;
                }
                // 活跃时段守卫：静默期不发任何 B 站请求，直接把 next_check
                // 顺延到下一窗口开始（防止用户中途改时段后旧 next_check 反复触发）
                let windows = blogger
                    .active_windows
                    .as_deref()
                    .map(active_window::parse_windows)
                    .unwrap_or_default();
                if !active_window::is_active(Local::now(), &windows) {
                    if let Err(e) = self.defer_to_next_window(&blogger, &windows).await {
                        error!("顺延博主 {} 检查时间失败: {e}", blogger.uid);
                    }
                    return;
                }
                if let Err(e) = self.check_blogger(&blogger).await {
                    error!("检查博主 {} 失败: {e}", blogger.uid);
                    self.add_log(Some(&blogger.uid), None, &format!("检查失败: {e}"), "error")
                        .await;
                }
            })
            .await;

        self.check_scheduled_danmaku().await?;
        self.check_auto_burn().await?;
        Ok(())
    }

    /// 补齐 fans 为 NULL 的博主；逐个拉取并间隔 2 秒。
    async fn backfill_missing_fans(&self) {
        use sea_orm::{ActiveModelTrait, Set};
        let bloggers = match blogger::Entity::find()
            .filter(blogger::Column::Fans.is_null())
            .all(&self.db)
            .await
        {
            Ok(list) => list,
            Err(e) => {
                error!("查询待补齐粉丝数的博主失败: {e}");
                return;
            }
        };
        for b in bloggers {
            if self.cancellation.is_cancelled() {
                return;
            }
            let Ok(uid_i64) = b.uid.parse::<i64>() else {
                continue;
            };
            let cookies = self.get_cookies_for_blogger(&b.uid).await;
            match self.bili_api.get_user_info(uid_i64, &cookies).await {
                Ok(info) if info.fans > 0 => {
                    let uid = b.uid.clone();
                    let mut model: blogger::ActiveModel = b.into();
                    model.fans = Set(Some(info.fans));
                    model.updated_at = Set(Some(Local::now()));
                    if let Err(e) = model.update(&self.db).await {
                        error!("补齐博主 {uid} 粉丝数失败: {e}");
                    } else {
                        info!("已补齐博主 {uid} 粉丝数: {}", info.fans);
                    }
                }
                Ok(_) => {}
                Err(e) => error!("补齐粉丝数时拉取博主 {} 资料失败: {e}", b.uid),
            }
            tokio::time::sleep(StdDuration::from_secs(2)).await;
        }
    }

    async fn get_cookies_for_blogger(&self, _uid: &str) -> String {
        self.settings_service
            .cookie_header()
            .await
            .unwrap_or_default()
    }

    async fn settings_cached(&self) -> Result<Value> {
        // 快速路径：读锁检查缓存命中
        {
            let cache = self.settings_cache.read().await;
            if let Some((settings, fetched_at)) = cache.as_ref() {
                if fetched_at.elapsed() < SETTINGS_CACHE_TTL {
                    return Ok(settings.clone());
                }
            }
        }
        // 缓存 miss：升级为写锁
        let mut cache = self.settings_cache.write().await;
        // 双检锁：在等写锁期间，可能有其他任务已刷新缓存，避免重复 DB 查询
        if let Some((settings, fetched_at)) = cache.as_ref() {
            if fetched_at.elapsed() < SETTINGS_CACHE_TTL {
                return Ok(settings.clone());
            }
        }
        // 真正发起 DB 查询（仅在缓存仍过期时）
        let settings = crate::services::settings::all_settings(&self.db).await?;
        *cache = Some((settings.clone(), Instant::now()));
        Ok(settings)
    }

    /// 从缓存设置中提取字幕设置，反序列化失败时返回默认值。
    async fn subtitle_settings(&self) -> crate::services::settings::SubtitleSettings {
        match self.settings_cached().await {
            Ok(settings) => {
                serde_json::from_value(settings.get("subtitle").cloned().unwrap_or_default())
                    .unwrap_or_default()
            }
            Err(_) => crate::services::settings::SubtitleSettings::default(),
        }
    }

    /// 清除设置缓存，使下次读取时重新从数据库加载。
    pub async fn invalidate_settings_cache(&self) {
        let mut cache = self.settings_cache.write().await;
        *cache = None;
    }
}
