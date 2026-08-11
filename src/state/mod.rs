//! 应用状态模块：将原 21 字段的 `AppState` 拆分为 4 个领域容器，
//! 通过 `FromRef` 实现类型安全的 Axum 提取。
//!
//! 领域划分：
//! - [`InfraState`]：基础设施（配置、路径、数据库、WebSocket、设置、取消令牌）
//! - [`BiliState`]：B 站 API（BiliApi、资源代理、认证、安全配置、校验）
//! - [`MediaState`]：媒体处理（下载管理、aria2、视频处理、弹幕、烧录）
//! - [`BusinessState`]：业务逻辑（博主、历史、刷新、监控）

pub(crate) mod bili;
pub(crate) mod business;
pub(crate) mod infra;
pub(crate) mod media;

pub(crate) use bili::BiliState;
pub(crate) use business::BusinessState;
pub(crate) use infra::InfraState;
pub(crate) use media::MediaState;

use axum::extract::FromRef;
use std::sync::Arc;

/// 应用顶层状态：持有 4 个领域容器的 Arc 指针。
///
/// `FromRef<AppState>` 为每个 `Arc<XxxState>` 均有实现，
/// Axum handler 可直接提取所需领域而无需访问完整 AppState。
#[derive(Clone)]
pub struct AppState {
    pub infra: Arc<InfraState>,
    pub bili: Arc<BiliState>,
    pub media: Arc<MediaState>,
    pub business: Arc<BusinessState>,
}

pub type SharedState = Arc<AppState>;

// --- FromRef 实现：clone 内部 Arc，无深拷贝开销 ---

impl FromRef<AppState> for Arc<InfraState> {
    fn from_ref(state: &AppState) -> Self {
        state.infra.clone()
    }
}

impl FromRef<AppState> for Arc<BiliState> {
    fn from_ref(state: &AppState) -> Self {
        state.bili.clone()
    }
}

impl FromRef<AppState> for Arc<MediaState> {
    fn from_ref(state: &AppState) -> Self {
        state.media.clone()
    }
}

impl FromRef<AppState> for Arc<BusinessState> {
    fn from_ref(state: &AppState) -> Self {
        state.business.clone()
    }
}

// --- 构造函数：按领域依赖顺序 Infra → Bili → Media → Business ---

impl AppState {
    pub async fn new(
        config: crate::config::AppConfig,
        paths: crate::config::AppPaths,
        db: sea_orm::DatabaseConnection,
        ai_skill_enabled: bool,
    ) -> anyhow::Result<SharedState> {
        // 1. 基础设施
        let (infra, secret_store) = InfraState::build(config, paths, db, ai_skill_enabled).await?;

        // 2. B 站 API
        let bili = BiliState::build(&infra, secret_store).await?;

        // 3. 媒体处理
        let media = MediaState::build(&infra, &bili).await?;

        // 4. 业务逻辑
        let business = BusinessState::build(&infra, &bili, &media).await?;

        Ok(Arc::new(Self {
            infra: Arc::new(infra),
            bili: Arc::new(bili),
            media: Arc::new(media),
            business: Arc::new(business),
        }))
    }
}
