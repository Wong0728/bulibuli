use crate::error::BiliErrorKind;
use crate::models::{blogger, history};
use crate::services::bili_api::models::video::VideoInfo;
use crate::services::bili_api::BiliApi;
use crate::services::settings::SettingsService;
use anyhow::Result;
use chrono::Local;
use futures::{stream, StreamExt};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// L1 worker 单次拉取的最大视频数。
const L1_BATCH: u64 = 50;
/// 默认 L1 间隔（分钟）。
const DEFAULT_L1_INTERVAL_MINUTES: u64 = 5;
/// L2 间隔（24h）。
const L2_INTERVAL: StdDuration = StdDuration::from_secs(24 * 3600);

#[derive(Clone, Copy, Default)]
struct RefreshReport {
    success: usize,
    risk_limited: bool,
}

#[derive(Clone, Default)]
struct RefreshGate {
    state: Arc<Mutex<RefreshGateState>>,
}

#[derive(Default)]
struct RefreshGateState {
    endpoint_until: HashMap<String, Instant>,
    risk_until: Option<Instant>,
}

impl RefreshGate {
    fn key(endpoint: &str, cookies: &str) -> String {
        format!(
            "{endpoint}:{}",
            crate::services::bili_api::session_fingerprint(cookies)
        )
    }

    async fn is_blocked(&self, endpoint: &str, cookies: &str) -> bool {
        let key = Self::key(endpoint, cookies);
        let now = Instant::now();
        let mut state = self.state.lock().await;
        state.endpoint_until.retain(|_, until| *until > now);
        state.risk_until = state.risk_until.filter(|until| *until > now);
        state.risk_until.is_some_and(|until| until > now)
            || state
                .endpoint_until
                .get(&key)
                .is_some_and(|until| *until > now)
    }

    async fn record(&self, endpoint: &str, cookies: &str, report: RefreshReport) {
        let key = Self::key(endpoint, cookies);
        let now = Instant::now();
        let mut state = self.state.lock().await;
        if report.risk_limited {
            state.risk_until = Some(now + StdDuration::from_secs(60));
            state
                .endpoint_until
                .insert(key, now + StdDuration::from_secs(5 * 60));
        } else if report.success == 0 {
            state
                .endpoint_until
                .insert(key, now + StdDuration::from_secs(30));
        } else {
            state.endpoint_until.remove(&key);
        }
    }
}

async fn run_refresh<F>(
    gate: &RefreshGate,
    endpoint: &str,
    cookies: &str,
    refresh: F,
) -> Result<RefreshReport>
where
    F: Future<Output = Result<RefreshReport>>,
{
    if gate.is_blocked(endpoint, cookies).await {
        return Ok(RefreshReport::default());
    }
    let report = refresh.await?;
    gate.record(endpoint, cookies, report).await;
    Ok(report)
}

/// 拉取频率分层 worker：
/// - L1（5min）：抽 50 条 `state in (completed, pay_blocked, removed)` 最久未刷的视频，
///   调 `get_video_info` 写回 `view` 与 `state`。
/// - L2（24h）：拉所有博主，调 `get_user_info` 写回 `face/sign/level/name`。
///
/// 请求采用最多 4 路有界并发，并继续经过 `BiliApi` 的统一全局限流器。
pub struct RefreshService {
    db: DatabaseConnection,
    bili_api: Arc<BiliApi>,
    settings_service: Arc<SettingsService>,
    l1_handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    l2_handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    cancellation: CancellationToken,
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
        }
    }

    /// 启动 L1 + L2 worker。
    pub async fn start(&self) {
        let gate = RefreshGate::default();
        self.start_l1(gate.clone()).await;
        self.start_l2(gate).await;
    }

    /// 停止 L1 + L2 worker。
    pub async fn stop(&self) {
        self.cancellation.cancel();
        if let Some(h) = self.l1_handle.lock().await.take() {
            await_worker("refresh L1", h).await;
        }
        if let Some(h) = self.l2_handle.lock().await.take() {
            await_worker("refresh L2", h).await;
        }
        info!("[refresh] L1/L2 worker 已停止");
    }

    async fn start_l1(&self, gate: RefreshGate) {
        if self.l1_handle.lock().await.is_some() {
            return;
        }
        info!("[refresh] L1 worker 已启动");
        let db = self.db.clone();
        let bili_api = self.bili_api.clone();
        let settings_service = self.settings_service.clone();
        let cancellation = self.cancellation.child_token();
        let handle = tokio::spawn(async move {
            l1_loop(db, bili_api, settings_service, cancellation, gate).await;
        });
        *self.l1_handle.lock().await = Some(handle);
    }

    async fn start_l2(&self, gate: RefreshGate) {
        if self.l2_handle.lock().await.is_some() {
            return;
        }
        info!("[refresh] L2 worker 已启动");
        let db = self.db.clone();
        let bili_api = self.bili_api.clone();
        let settings_service = self.settings_service.clone();
        let cancellation = self.cancellation.child_token();
        let handle = tokio::spawn(async move {
            l2_loop(db, bili_api, settings_service, cancellation, gate).await;
        });
        *self.l2_handle.lock().await = Some(handle);
    }

    /// 手动触发 L1 刷新（POST /api/refresh?kind=board 用）。
    pub async fn trigger_l1(&self) -> Result<usize> {
        let cookies = read_cookies(&self.settings_service).await;
        Ok(refresh_video_stats(&self.db, &self.bili_api, &cookies)
            .await?
            .success)
    }

    /// 手动触发 L2 刷新（POST /api/refresh?kind=blogger 用）。
    pub async fn trigger_l2(&self) -> Result<usize> {
        let cookies = read_cookies(&self.settings_service).await;
        Ok(
            refresh_all_bloggers_report(&self.db, &self.bili_api, &cookies)
                .await?
                .success,
        )
    }

    /// 手动触发单个视频刷新（POST /api/refresh?kind=video 用）。
    pub async fn trigger_video(&self, bvid: &str) -> crate::error::AppResult<()> {
        let cookies = read_cookies(&self.settings_service).await;
        refresh_single_video(&self.db, &self.bili_api, bvid, &cookies).await
    }
}

/// L1 worker 主循环。
async fn await_worker(name: &str, handle: JoinHandle<()>) {
    match tokio::time::timeout(StdDuration::from_secs(10), handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => error!("[{name}] worker 退出异常: {error}"),
        Err(_) => error!("[{name}] worker 未在 10 秒内退出"),
    }
}

async fn l1_loop(
    db: DatabaseConnection,
    bili_api: Arc<BiliApi>,
    settings_service: Arc<SettingsService>,
    cancellation: CancellationToken,
    gate: RefreshGate,
) {
    loop {
        let interval_minutes = read_l1_interval(&db).await;
        let jitter = Local::now().timestamp_subsec_millis() as u64 % 15;
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
async fn l2_loop(
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
            _ = tokio::time::sleep(L2_INTERVAL + StdDuration::from_secs(Local::now().timestamp_subsec_millis() as u64 % 60)) => {}
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

/// L1：抽 50 条最久未刷的视频，有界并发调 get_video_info 写回 view/state。
/// 返回成功刷新的条数。
async fn refresh_video_stats(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    cookies: &str,
) -> Result<RefreshReport> {
    // 选 state in (completed, pay_blocked, removed) 且按 view_refreshed_at ASC 的 50 条
    let rows = history::Entity::find()
        .filter(history::Column::State.is_in(vec!["completed", "pay_blocked", "removed"]))
        .order_by_asc(history::Column::ViewRefreshedAt)
        .limit(L1_BATCH)
        .all(db)
        .await?;

    if rows.is_empty() {
        return Ok(RefreshReport::default());
    }

    let row_count = rows.len();
    info!("[refresh L1] 开始刷新 {row_count} 条视频的实时数据");
    let risk_limited = Arc::new(AtomicBool::new(false));
    let success = stream::iter(rows)
        .map(|h| {
            let risk_limited = risk_limited.clone();
            async move {
                match bili_api.get_video_info(&h.bvid, cookies).await {
                    Ok(info) => {
                        let model = map_video_info(&h, &info);
                        if let Err(e) = model.update(db).await {
                            warn!("[refresh L1] 更新 {} 失败: {e}", h.bvid);
                            false
                        } else {
                            true
                        }
                    }
                    Err(e) => {
                        warn!("[refresh L1] 获取 {} 出错: {e}", h.bvid);
                        if is_risk_or_permission(&e) {
                            risk_limited.store(true, Ordering::Relaxed);
                        }
                        if !is_risk_or_permission(&e) {
                            touch_view_refreshed_at(db, h.id).await;
                        }
                        false
                    }
                }
            }
        })
        .buffer_unordered(4)
        .filter(|updated| futures::future::ready(*updated))
        .count()
        .await;
    info!("[refresh L1] 完成，成功 {success}/{row_count} 条");
    Ok(RefreshReport {
        success,
        risk_limited: risk_limited.load(Ordering::Relaxed),
    })
}

fn is_risk_or_permission(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<crate::error::BiliApiError>()
            .is_some_and(|bili| {
                matches!(
                    bili.kind,
                    BiliErrorKind::RiskControl
                        | BiliErrorKind::Unauthorized
                        | BiliErrorKind::ChargeRequired
                        | BiliErrorKind::GeoRestricted
                )
            })
    })
}

/// L2：拉所有博主，有界并发调 get_user_info 写回 face/sign/level/name。
/// 返回成功刷新的博主数。
#[allow(dead_code)]
pub async fn refresh_all_bloggers(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    cookies: &str,
) -> Result<usize> {
    Ok(refresh_all_bloggers_report(db, bili_api, cookies)
        .await?
        .success)
}

async fn refresh_all_bloggers_report(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    cookies: &str,
) -> Result<RefreshReport> {
    let bloggers = blogger::Entity::find().all(db).await?;
    if bloggers.is_empty() {
        return Ok(RefreshReport::default());
    }
    let blogger_count = bloggers.len();
    info!("[refresh L2] 开始刷新 {blogger_count} 个博主资料");
    let outcomes = stream::iter(bloggers)
        .map(|blogger| refresh_blogger_profile(db, bili_api, blogger, cookies))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let success = outcomes.iter().filter(|(updated, _)| *updated).count();
    let risk_limited = outcomes.iter().any(|(_, risk)| *risk);
    info!("[refresh L2] 完成，成功 {success}/{blogger_count} 个");
    Ok(RefreshReport {
        success,
        risk_limited,
    })
}

async fn refresh_blogger_profile(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    blogger: blogger::Model,
    cookies: &str,
) -> (bool, bool) {
    let uid: i64 = match blogger.uid.parse() {
        Ok(uid) => uid,
        Err(_) => return (false, false),
    };
    let info = match bili_api.get_user_info(uid, cookies).await {
        Ok(info) => info,
        Err(error) => {
            warn!("[refresh L2] 获取博主 {} 出错: {error}", blogger.uid);
            return (false, is_risk_or_permission(&error));
        }
    };
    let mut model: blogger::ActiveModel = blogger.clone().into();
    let mut changed = false;
    if Some(info.name.as_str()) != blogger.name.as_deref()
        && blogger.last_seen_name.is_none()
        && blogger.name.is_some()
    {
        model.last_seen_name = Set(blogger.name.clone());
        model.last_seen_at = Set(Some(Local::now()));
        changed = true;
    }
    if Some(info.name.as_str()) != blogger.name.as_deref() {
        model.name = Set(Some(info.name.clone()));
        changed = true;
    }
    if Some(info.face.as_str()) != blogger.face.as_deref()
        && blogger.last_seen_face.is_none()
        && blogger.face.is_some()
    {
        model.last_seen_face = Set(blogger.face.clone());
        model.last_seen_at = Set(Some(Local::now()));
        changed = true;
    }
    if Some(info.face.as_str()) != blogger.face.as_deref() {
        model.face = Set(Some(info.face.clone()));
        changed = true;
    }
    if Some(info.sign.as_str()) != blogger.sign.as_deref() {
        model.sign = Set(Some(info.sign));
        changed = true;
    }
    if Some(info.level as i32) != blogger.level {
        model.level = Set(Some(info.level as i32));
        changed = true;
    }
    if !changed {
        return (true, false);
    }
    model.updated_at = Set(Some(Local::now()));
    if let Err(error) = model.update(db).await {
        warn!("[refresh L2] 更新博主 {} 失败: {error}", blogger.uid);
        (false, false)
    } else {
        (true, false)
    }
}

/// 手动刷新单个视频：写回 view/state/view_refreshed_at。
/// history 查不到记录时返回 NotFound（审计 B6：此前静默返回成功，API 假成功）。
pub async fn refresh_single_video(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    bvid: &str,
    cookies: &str,
) -> crate::error::AppResult<()> {
    use crate::error::AppError;
    let h = history::Entity::find()
        .filter(history::Column::Bvid.eq(bvid))
        .one(db)
        .await
        .map_err(|error| AppError::Internal(format!("查询视频记录失败: {error}")))?;
    let Some(h) = h else {
        return Err(AppError::NotFound("未找到该视频的下载记录".to_string()));
    };
    let info = bili_api
        .get_video_info(bvid, cookies)
        .await
        .map_err(|error| AppError::Internal(format!("获取视频信息失败: {error}")))?;
    map_video_info(&h, &info)
        .update(db)
        .await
        .map_err(|error| AppError::Internal(format!("更新视频记录失败: {error}")))?;
    Ok(())
}

fn map_video_info(history: &history::Model, info: &VideoInfo) -> history::ActiveModel {
    let mut model: history::ActiveModel = history.clone().into();
    model.view = Set(Some(info.stat.view));
    let new_state = match info.state {
        -100 => "removed".to_string(),
        -1 | -6 => "pay_blocked".to_string(),
        _ => history
            .state
            .clone()
            .unwrap_or_else(|| "completed".to_string()),
    };
    if history.state.as_deref() != Some(&new_state) {
        model.state = Set(Some(new_state));
    }
    model.view_refreshed_at = Set(Some(Local::now()));
    model.view_source = Set(Some("live".to_string()));
    if history.owner_name.is_none() && !info.owner.name.is_empty() {
        model.owner_name = Set(Some(info.owner.name.clone()));
    }
    if history.owner_face.is_none() && !info.owner.face.is_empty() {
        let face = if info.owner.face.starts_with("http") {
            info.owner.face.clone()
        } else {
            format!("https:{}", info.owner.face)
        };
        model.owner_face = Set(Some(face));
    }
    model
}

/// 失败时也更新 view_refreshed_at，避免该条一直被选中。
async fn touch_view_refreshed_at(db: &DatabaseConnection, id: i32) {
    if let Ok(Some(h)) = history::Entity::find_by_id(id).one(db).await {
        let mut model: history::ActiveModel = h.into();
        model.view_refreshed_at = Set(Some(Local::now()));
        if let Err(error) = model.update(db).await {
            warn!("[refresh] 持久化刷新结果失败: {error}");
        }
    }
}

/// 读取 Cookie（DB key="cookies"）。
async fn read_cookies(settings_service: &SettingsService) -> String {
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
