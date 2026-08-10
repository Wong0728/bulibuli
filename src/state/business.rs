//! 业务逻辑领域状态：博主服务、历史服务、刷新服务、监控服务、直播监控。

use crate::services::blogger::BloggerService;
use crate::services::history::HistoryService;
use crate::services::live_monitor::LiveMonitor;
use crate::services::live_source::LiveSourceService;
use crate::services::monitor::{MonitorService, MonitorServiceDependencies};
use crate::services::refresh::RefreshService;
use crate::state::bili::BiliState;
use crate::state::infra::InfraState;
use crate::state::media::MediaState;
use std::sync::Arc;

/// 业务逻辑领域：封装面向用户场景的高层服务。
#[derive(Clone)]
pub struct BusinessState {
    pub(crate) blogger_service: Arc<BloggerService>,
    pub(crate) history_service: Arc<HistoryService>,
    pub(crate) refresh_service: Arc<RefreshService>,
    pub(crate) monitor_service: Arc<MonitorService>,
    pub(crate) live_monitor: Arc<LiveMonitor>,
    pub(crate) live_source_service: Arc<LiveSourceService>,
}

impl BusinessState {
    /// 构建业务逻辑领域。
    ///
    /// 依赖前三个领域的全部输出：InfraState / BiliState / MediaState。
    pub(crate) async fn build(
        infra: &InfraState,
        bili: &BiliState,
        media: &MediaState,
    ) -> anyhow::Result<Self> {
        let blogger_service = Arc::new(BloggerService::new(infra.db.clone(), infra.paths.clone()));
        let history_service = Arc::new(HistoryService::new(infra.db.clone(), infra.paths.clone()));

        let refresh_service = Arc::new(RefreshService::new(
            infra.db.clone(),
            bili.bili_api.clone(),
            infra.settings_service.clone(),
            infra.cancellation.child_token(),
        ));

        let monitor_service = Arc::new(
            MonitorService::new(MonitorServiceDependencies {
                config: infra.config.clone(),
                db: infra.db.clone(),
                bili_api: bili.bili_api.clone(),
                download_manager: media.download_manager.clone(),
                danmaku_service: media.danmaku_service.clone(),
                subtitle_service: media.subtitle_service.clone(),
                blogger_service: blogger_service.clone(),
                ws: infra.ws.clone(),
                video_processor: media.video_processor.clone(),
                history_service: history_service.clone(),
                settings_service: infra.settings_service.clone(),
                burn_semaphore: media.burn_semaphore.clone(),
                cancellation: infra.cancellation.child_token(),
            })
            .await,
        );

        let live_source_service = Arc::new(LiveSourceService::new(infra.db.clone()));
        let live_monitor = Arc::new(LiveMonitor::new(
            bili.bili_api.clone(),
            media.live_recorder.clone(),
            live_source_service.clone(),
            infra.settings_service.clone(),
            infra.cancellation.child_token(),
        ));

        Ok(Self {
            blogger_service,
            history_service,
            refresh_service,
            monitor_service,
            live_monitor,
            live_source_service,
        })
    }
}
