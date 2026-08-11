//! 媒体处理领域状态：下载管理、aria2、视频处理、弹幕、字幕、烧录、直播录制。

use crate::models::burn::BurnTask;
use crate::services::aria2::Aria2Manager;
use crate::services::danmaku::DanmakuService;
use crate::services::download::{DownloadManager, DownloadManagerDependencies};
use crate::services::live_recorder::{LiveRecorder, LiveRecorderDeps};
use crate::services::subtitle_fetch::SubtitleFetchService;
use crate::services::video_processor::VideoProcessor;
use crate::state::bili::BiliState;
use crate::state::infra::InfraState;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

pub type BurnTasks = Arc<Mutex<HashMap<String, BurnTask>>>;

/// 媒体处理领域：封装下载、视频处理、弹幕、字幕下载、字幕烧录、直播录制等服务。
#[derive(Clone)]
pub struct MediaState {
    pub(crate) download_manager: Arc<DownloadManager>,
    pub(crate) aria2: Arc<Aria2Manager>,
    pub(crate) video_processor: Arc<VideoProcessor>,
    pub(crate) danmaku_service: Arc<DanmakuService>,
    pub(crate) subtitle_service: Arc<SubtitleFetchService>,
    pub(crate) burn_tasks: BurnTasks,
    pub(crate) burn_semaphore: Arc<Semaphore>,
    pub(crate) live_recorder: Arc<LiveRecorder>,
}

impl MediaState {
    /// 构建媒体处理领域。
    ///
    /// 依赖 `InfraState`（config / paths / db / ws / settings_service / cancellation）
    /// 和 `BiliState`（bili_api / cookie_manager）。
    pub(crate) async fn build(infra: &InfraState, bili: &BiliState) -> anyhow::Result<Self> {
        let video_processor = Arc::new(VideoProcessor::new(infra.paths.clone()));
        let danmaku_service = Arc::new(DanmakuService::new(
            infra.paths.clone(),
            bili.bili_api.clone(),
            bili.cookie_manager.clone(),
        ));
        let subtitle_service = Arc::new(SubtitleFetchService::new(
            infra.paths.clone(),
            bili.bili_api.clone(),
        ));
        let aria2 = Arc::new(Aria2Manager::new(infra.paths.clone(), &infra.config)?);
        let burn_tasks = Arc::new(Mutex::new(HashMap::new()));
        let burn_semaphore = Arc::new(Semaphore::new(2));

        let download_manager = Arc::new(
            DownloadManager::new(DownloadManagerDependencies {
                config: infra.config.clone(),
                paths: infra.paths.clone(),
                db: infra.db.clone(),
                aria2: aria2.clone(),
                bili_api: bili.bili_api.clone(),
                video_processor: video_processor.clone(),
                ws: infra.ws.clone(),
                settings_service: infra.settings_service.clone(),
                cancellation: infra.cancellation.child_token(),
            })
            .await?,
        );

        let live_recorder = Arc::new(LiveRecorder::new(LiveRecorderDeps {
            bili_api: bili.bili_api.clone(),
            video_processor: video_processor.clone(),
            paths: infra.paths.clone(),
            settings_service: infra.settings_service.clone(),
            db: infra.db.clone(),
        }));
        live_recorder.recover_incomplete_records().await?;
        live_recorder.restore_merge_jobs().await?;

        Ok(Self {
            download_manager,
            aria2,
            video_processor,
            danmaku_service,
            subtitle_service,
            burn_tasks,
            burn_semaphore,
            live_recorder,
        })
    }
}
