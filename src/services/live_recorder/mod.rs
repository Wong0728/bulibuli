//! 直播录制服务：管理并发录制、FFmpeg 监督和流地址分段轮换。

pub mod ffmpeg_session;
mod interactions;
pub mod stream_url;

use crate::config::AppPaths;
use crate::models::live_recording;
use crate::services::bili_api::BiliApi;
use crate::services::danmu_collector::DanmuCollector;
use crate::services::live_source::CaptureMode;
use crate::services::settings::SettingsService;
use crate::services::video_processor::VideoProcessor;
use anyhow::{anyhow, Context, Result};
use ffmpeg_session::{merge_segments_to_mp4, FfmpegSession};
pub use interactions::ArchivedLiveEvent;
use interactions::{InteractionPaths, InteractionWriterArgs};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use stream_url::{is_expiring_soon, select_best_stream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{interval, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const DANMU_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const URL_REFRESH_MARGIN_SECS: i64 = 60;

/// 录制状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingStatus {
    Starting,
    Recording,
    Stopping,
    Stopped,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingTrigger {
    Manual,
    Auto,
}

impl RecordingTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}

impl std::fmt::Display for RecordingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Recording => write!(f, "recording"),
            Self::Stopping => write!(f, "stopping"),
            Self::Stopped => write!(f, "stopped"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// 对外暴露的录制信息。
#[derive(Clone, Debug, Serialize)]
pub struct RecordingInfo {
    pub room_id: i64,
    pub title: String,
    pub status: RecordingStatus,
    pub output_path: String,
    pub started_at: String,
    pub duration_secs: i64,
    pub file_size: i64,
    pub error_msg: Option<String>,
    pub danmu_unavailable: bool,
    pub stream_quality: Option<i32>,
    pub stream_protocol: Option<String>,
    pub stream_format: Option<String>,
    pub stream_codec: Option<String>,
    pub trigger: String,
    pub capture_mode: String,
    pub interaction_capture_status: String,
    pub interaction_error: Option<String>,
    pub event_path: Option<String>,
    pub xml_path: Option<String>,
    pub summary_path: Option<String>,
    pub danmaku_count: i64,
    pub unique_user_count: i64,
    pub free_gift_count: i64,
    pub paid_gift_count: i64,
    pub sc_count: i64,
    pub guard_count: i64,
    pub peak_watched: i64,
    pub dropped_event_count: i64,
    pub estimated_paid_value: f64,
    pub last_event_seq: u64,
}

/// 录制服务依赖。
pub struct LiveRecorderDeps {
    pub bili_api: Arc<BiliApi>,
    pub video_processor: Arc<VideoProcessor>,
    pub paths: Arc<AppPaths>,
    pub settings_service: Arc<SettingsService>,
    pub db: DatabaseConnection,
}

/// 直播录制服务：管理多个并发录制会话。
#[derive(Clone)]
pub struct LiveRecorder {
    inner: Arc<LiveRecorderInner>,
}

struct LiveRecorderInner {
    sessions: Mutex<HashMap<i64, SessionEntry>>,
    bili_api: Arc<BiliApi>,
    video_processor: Arc<VideoProcessor>,
    paths: Arc<AppPaths>,
    settings_service: Arc<SettingsService>,
    db: DatabaseConnection,
}

enum SessionEntry {
    Starting(Arc<Mutex<RecordingInfo>>),
    Active(RecordingSessionHandle),
}

#[derive(Clone)]
struct RecordingSessionHandle {
    snapshot: Arc<Mutex<RecordingInfo>>,
    command_tx: mpsc::Sender<SessionCommand>,
    recent: Arc<Mutex<VecDeque<ArchivedLiveEvent>>>,
}

enum SessionCommand {
    Stop(oneshot::Sender<Result<RecordingInfo>>),
}

#[derive(Debug)]
enum DanmuCollectorEvent {
    Exited,
    Failed(String),
}

impl LiveRecorder {
    pub fn new(deps: LiveRecorderDeps) -> Self {
        Self {
            inner: Arc::new(LiveRecorderInner {
                sessions: Mutex::new(HashMap::new()),
                bili_api: deps.bili_api,
                video_processor: deps.video_processor,
                paths: deps.paths,
                settings_service: deps.settings_service,
                db: deps.db,
            }),
        }
    }

    /// 开始录制指定直播间。占位状态在网络和进程启动期间阻止重复启动。
    #[allow(dead_code)]
    pub async fn start(&self, room_id: i64) -> Result<RecordingInfo> {
        self.start_with_options(room_id, RecordingTrigger::Manual, CaptureMode::Standard)
            .await
    }

    pub async fn start_with_options(
        &self,
        room_id: i64,
        trigger: RecordingTrigger,
        capture_mode: CaptureMode,
    ) -> Result<RecordingInfo> {
        if room_id <= 0 {
            return Err(anyhow!("直播间号必须为正整数"));
        }
        let cookies = self.inner.settings_service.cookie_header().await?;
        let init = self
            .inner
            .bili_api
            .live_room_init(room_id, &cookies)
            .await
            .context("解析直播间号失败")?;
        if init.room_id <= 0 {
            return Err(anyhow!("直播间 {room_id} 不存在或不可用"));
        }
        if !init.is_live() {
            return Err(anyhow!("直播间 {} 当前未开播", init.room_id));
        }
        let room_id = init.room_id;
        let startup_snapshot = Arc::new(Mutex::new(RecordingInfo {
            room_id,
            title: "正在获取直播信息".to_string(),
            status: RecordingStatus::Starting,
            output_path: String::new(),
            started_at: chrono::Utc::now().to_rfc3339(),
            duration_secs: 0,
            file_size: 0,
            error_msg: None,
            danmu_unavailable: false,
            stream_quality: None,
            stream_protocol: None,
            stream_format: None,
            stream_codec: None,
            trigger: trigger.as_str().to_owned(),
            capture_mode: capture_mode.as_str().to_owned(),
            interaction_capture_status: if capture_mode == CaptureMode::Off {
                "off"
            } else {
                "connecting"
            }
            .to_owned(),
            interaction_error: None,
            event_path: None,
            xml_path: None,
            summary_path: None,
            danmaku_count: 0,
            unique_user_count: 0,
            free_gift_count: 0,
            paid_gift_count: 0,
            sc_count: 0,
            guard_count: 0,
            peak_watched: 0,
            dropped_event_count: 0,
            estimated_paid_value: 0.0,
            last_event_seq: 0,
        }));
        {
            let mut sessions = self.inner.sessions.lock().await;
            if sessions.contains_key(&room_id) {
                return Err(anyhow!("直播间 {room_id} 已在录制中或正在启动"));
            }
            sessions.insert(room_id, SessionEntry::Starting(startup_snapshot));
        }

        let result = self
            .start_session(&init, &cookies, trigger, capture_mode)
            .await;
        if result.is_err() {
            self.inner.sessions.lock().await.remove(&room_id);
        }
        result
    }

    async fn start_session(
        &self,
        init: &crate::services::bili_api::models::live::LiveRoomInit,
        cookies: &str,
        trigger: RecordingTrigger,
        capture_mode: CaptureMode,
    ) -> Result<RecordingInfo> {
        let room_id = init.room_id;
        let info = self
            .inner
            .bili_api
            .live_get_info(room_id, cookies)
            .await
            .context("获取直播间信息失败")?;
        let playurl = self
            .inner
            .bili_api
            .live_playurl(room_id, None, cookies)
            .await
            .context("获取直播流地址失败")?;
        let selected_stream = select_best_stream(&playurl.durl)?;
        let stream_url = selected_stream.url.clone();

        let (ffmpeg_path, _) = self.inner.video_processor.detect_ffmpeg("auto", None).await;
        let ffmpeg_path = ffmpeg_path.ok_or_else(|| anyhow!("未找到 FFmpeg，无法录制"))?;

        let live_dir = self
            .inner
            .paths
            .download_dir
            .join("live")
            .join(room_id.to_string());
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let safe_title = sanitize_filename(&info.title);
        let base_prefix = format!("{timestamp}_{unique}_{safe_title}");
        let first_segment = segment_path(&live_dir, &base_prefix, 0);
        let mut ffmpeg = FfmpegSession::start(
            &ffmpeg_path,
            &stream_url,
            first_segment.clone(),
            room_id,
            user_agent(),
            referer(),
        )?;

        let started_at = chrono::Utc::now();
        let interaction_paths = InteractionPaths {
            legacy: live_dir.join(format!("{base_prefix}_danmu.json")),
            events: live_dir.join(format!("{base_prefix}_events.jsonl")),
            xml: live_dir.join(format!("{base_prefix}_danmaku.xml")),
            summary: live_dir.join(format!("{base_prefix}_interaction_summary.json")),
        };
        let initial_info = RecordingInfo {
            room_id,
            title: info.title.clone(),
            status: RecordingStatus::Recording,
            output_path: first_segment.to_string_lossy().to_string(),
            started_at: started_at.to_rfc3339(),
            duration_secs: 0,
            file_size: 0,
            error_msg: None,
            danmu_unavailable: false,
            stream_quality: (selected_stream.current_qn > 0).then_some(selected_stream.current_qn),
            stream_protocol: (!selected_stream.protocol_name.is_empty())
                .then_some(selected_stream.protocol_name.clone()),
            stream_format: (!selected_stream.format_name.is_empty())
                .then_some(selected_stream.format_name.clone()),
            stream_codec: (!selected_stream.codec_name.is_empty())
                .then_some(selected_stream.codec_name.clone()),
            trigger: trigger.as_str().to_owned(),
            capture_mode: capture_mode.as_str().to_owned(),
            interaction_capture_status: if capture_mode == CaptureMode::Off {
                "off"
            } else {
                "connecting"
            }
            .to_owned(),
            interaction_error: None,
            event_path: (capture_mode != CaptureMode::Off)
                .then(|| interaction_paths.events.to_string_lossy().to_string()),
            xml_path: (capture_mode != CaptureMode::Off)
                .then(|| interaction_paths.xml.to_string_lossy().to_string()),
            summary_path: (capture_mode != CaptureMode::Off)
                .then(|| interaction_paths.summary.to_string_lossy().to_string()),
            danmaku_count: 0,
            unique_user_count: 0,
            free_gift_count: 0,
            paid_gift_count: 0,
            sc_count: 0,
            guard_count: 0,
            peak_watched: 0,
            dropped_event_count: 0,
            estimated_paid_value: 0.0,
            last_event_seq: 0,
        };
        let snapshot = Arc::new(Mutex::new(initial_info));
        let recent = Arc::new(Mutex::new(VecDeque::with_capacity(100)));
        let segment_index = Arc::new(AtomicU32::new(0));
        let danmu_cancel = CancellationToken::new();
        let (reload_tx, reload_rx) = mpsc::channel(8);
        let (danmu_failure_tx, danmu_failure_rx) = mpsc::channel(1);
        let (danmu_collector_tx, danmu_collector_rx) = mpsc::channel(1);
        let mut danmu_collector_monitor = None;
        let mut danmu_write_handle = None;
        let mut persisted_danmu_path = None;
        let mut danmu_channel_open = false;
        let mut danmu_unavailable = false;

        if capture_mode != CaptureMode::Off {
            match self.inner.bili_api.live_danmu_conf(room_id, cookies).await {
                Ok(danmu_conf) => {
                    let hosts: Vec<(String, i32)> = danmu_conf
                        .host_server_list
                        .iter()
                        .map(|host| (host.host.clone(), host.wss_port))
                        .collect();
                    match DanmuCollector::start(
                        room_id,
                        danmu_conf.token,
                        hosts,
                        self.inner.bili_api.clone(),
                        cookies.to_owned(),
                        danmu_cancel.clone(),
                    )
                    .await
                    {
                        Ok((danmu_collector_handle, mut danmu_rx)) => {
                            danmu_channel_open = true;
                            danmu_collector_monitor = Some(tokio::spawn(async move {
                                match danmu_collector_handle.await {
                                    Ok(()) => {
                                        let _ = danmu_collector_tx
                                            .send(DanmuCollectorEvent::Exited)
                                            .await;
                                    }
                                    Err(error) => {
                                        let _ = danmu_collector_tx
                                            .send(DanmuCollectorEvent::Failed(error.to_string()))
                                            .await;
                                    }
                                }
                            }));
                            let danmu_path = interaction_paths.legacy.clone();
                            persisted_danmu_path = Some(danmu_path.clone());
                            let danmu_write_cancel = danmu_cancel.clone();
                            let writer_args = InteractionWriterArgs {
                                room_id,
                                title: info.title.clone(),
                                mode: capture_mode,
                                started_at,
                                paths: interaction_paths.clone(),
                                snapshot: snapshot.clone(),
                                recent: recent.clone(),
                                cancellation: danmu_write_cancel,
                                reload_tx,
                                segment_index: segment_index.clone(),
                            };
                            danmu_write_handle = Some(tokio::spawn(async move {
                                let result = interactions::run(&mut danmu_rx, writer_args).await;
                                if let Err(error) = &result {
                                    let _ = danmu_failure_tx.send(error.to_string()).await;
                                }
                                result
                            }));
                        }
                        Err(error) => {
                            danmu_unavailable = true;
                            let mut state = snapshot.lock().await;
                            state.danmu_unavailable = true;
                            state.interaction_capture_status = "unavailable".to_owned();
                            state.interaction_error = Some(error.to_string());
                            warn!(room_id, "弹幕采集启动失败，降级为仅录制视频: {error}");
                        }
                    }
                }
                Err(error) => {
                    danmu_unavailable = true;
                    let mut state = snapshot.lock().await;
                    state.danmu_unavailable = true;
                    state.interaction_capture_status = "unavailable".to_owned();
                    state.interaction_error = Some(error.to_string());
                    warn!(room_id, "获取弹幕配置失败，降级为仅录制视频: {error}");
                }
            }
        }
        if capture_mode != CaptureMode::Off && !danmu_unavailable {
            snapshot.lock().await.interaction_capture_status = "capturing".to_owned();
        }
        let initial_info = snapshot.lock().await.clone();
        let now = chrono::Utc::now().to_rfc3339();
        let recording_result = live_recording::ActiveModel {
            room_id: Set(room_id),
            short_id: Set(init.short_id),
            uid: Set(init.uid),
            title: Set(info.title.clone()),
            cover: Set(
                crate::services::bili_url_policy::normalize_syntax(&info.user_cover)
                    .map(|url| url.to_string())
                    .unwrap_or_default(),
            ),
            status: Set(RecordingStatus::Recording.to_string()),
            output_path: Set(Some(initial_info.output_path.clone())),
            danmu_path: Set(persisted_danmu_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())),
            file_size: Set(0),
            duration: Set(0),
            started_at: Set(initial_info.started_at.clone()),
            ended_at: Set(None),
            error_msg: Set(None),
            trigger: Set(initial_info.trigger.clone()),
            event_path: Set(initial_info.event_path.clone()),
            xml_path: Set(initial_info.xml_path.clone()),
            summary_path: Set(initial_info.summary_path.clone()),
            capture_mode: Set(initial_info.capture_mode.clone()),
            interaction_status: Set(initial_info.interaction_capture_status.clone()),
            interaction_error: Set(initial_info.interaction_error.clone()),
            danmaku_count: Set(0),
            unique_user_count: Set(0),
            free_gift_count: Set(0),
            paid_gift_count: Set(0),
            sc_count: Set(0),
            guard_count: Set(0),
            peak_watched: Set(0),
            dropped_event_count: Set(0),
            estimated_paid_value: Set(0.0),
            created_at: Set(now.clone()),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&self.inner.db)
        .await;
        let recording = match recording_result {
            Ok(recording) => recording,
            Err(error) => {
                danmu_cancel.cancel();
                if let Some(handle) = danmu_collector_monitor.take() {
                    handle.abort();
                }
                if let Some(handle) = danmu_write_handle.take() {
                    handle.abort();
                }
                if let Err(stop_error) = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                    warn!(room_id, "保存录制记录失败后停止 FFmpeg 失败: {stop_error}");
                }
                return Err(error).context("保存直播录制记录失败");
            }
        };
        let (command_tx, command_rx) = mpsc::channel(8);
        let handle = RecordingSessionHandle {
            snapshot: snapshot.clone(),
            command_tx,
            recent,
        };

        let worker = RecordingWorker {
            room_id,
            started_at,
            snapshot,
            command_rx,
            reload_rx,
            bili_api: self.inner.bili_api.clone(),
            settings_service: self.inner.settings_service.clone(),
            db: self.inner.db.clone(),
            recording_id: recording.id,
            ffmpeg_path,
            current_url: stream_url,
            current_ffmpeg: Some(ffmpeg),
            live_dir,
            base_prefix,
            next_segment: 1,
            segments: vec![first_segment],
            segment_index,
            danmu_cancel,
            danmu_collector_handle: danmu_collector_monitor,
            danmu_collector_rx,
            danmu_collector_channel_open: danmu_channel_open,
            danmu_write_handle,
            danmu_path: persisted_danmu_path,
            danmu_failure_rx,
            danmu_failure_channel_open: danmu_channel_open,
            reload_channel_open: danmu_channel_open,
            failure: None,
            stop_requested: false,
        };
        tokio::spawn(async move { worker.run().await });

        self.inner
            .sessions
            .lock()
            .await
            .insert(room_id, SessionEntry::Active(handle));
        info!(room_id, "直播录制已开始");
        Ok(initial_info)
    }

    /// 停止录制并等待 worker 完成弹幕收尾和分段合并。
    pub async fn stop(&self, room_id: i64) -> Result<RecordingInfo> {
        let handle = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(handle)) => handle.clone(),
                Some(SessionEntry::Starting(_)) => {
                    return Err(anyhow!("直播间 {room_id} 正在启动，请稍后再试"));
                }
                None => return Err(anyhow!("直播间 {room_id} 未在录制")),
            }
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .command_tx
            .send(SessionCommand::Stop(reply_tx))
            .await
            .map_err(|_| anyhow!("直播录制 worker 已退出"))?;
        let result = reply_rx
            .await
            .map_err(|_| anyhow!("直播录制 worker 未返回停止结果"))?;
        self.inner.sessions.lock().await.remove(&room_id);
        result
    }

    /// 获取所有活跃录制会话的快照，不持有 sessions 锁执行文件或网络 I/O。
    pub async fn status_all(&self) -> Vec<RecordingInfo> {
        let handles = {
            let sessions = self.inner.sessions.lock().await;
            sessions
                .values()
                .map(|entry| match entry {
                    SessionEntry::Active(handle) => handle.snapshot.clone(),
                    SessionEntry::Starting(snapshot) => snapshot.clone(),
                })
                .collect::<Vec<_>>()
        };

        let mut result = Vec::with_capacity(handles.len());
        for snapshot in handles {
            result.push(snapshot.lock().await.clone());
        }
        result
    }

    pub async fn status(&self, room_id: i64) -> Option<RecordingInfo> {
        let snapshot = {
            let sessions = self.inner.sessions.lock().await;
            sessions.get(&room_id).map(|entry| match entry {
                SessionEntry::Active(handle) => handle.snapshot.clone(),
                SessionEntry::Starting(snapshot) => snapshot.clone(),
            })
        }?;
        let info = snapshot.lock().await.clone();
        Some(info)
    }

    pub async fn events(
        &self,
        room_id: i64,
        after_seq: u64,
        limit: usize,
    ) -> Vec<ArchivedLiveEvent> {
        let recent = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(handle)) => Some(handle.recent.clone()),
                _ => None,
            }
        };
        let Some(recent) = recent else {
            return Vec::new();
        };
        let events = recent
            .lock()
            .await
            .iter()
            .filter(|event| event.seq > after_seq)
            .take(limit.min(100))
            .cloned()
            .collect();
        events
    }

    /// Mark sessions left in a running state by a previous crash as failed and
    /// report residual FLV segments for manual recovery instead of silently
    /// presenting them as active recordings.
    pub async fn recover_incomplete_records(&self) -> Result<()> {
        let rows = live_recording::Entity::find()
            .filter(live_recording::Column::Status.is_in(["starting", "recording", "stopping"]))
            .all(&self.inner.db)
            .await?;
        let now = chrono::Utc::now().to_rfc3339();
        for row in rows {
            live_recording::ActiveModel {
                id: Set(row.id),
                status: Set(RecordingStatus::Failed.to_string()),
                ended_at: Set(Some(now.clone())),
                error_msg: Set(Some("程序上次运行时异常中断".to_string())),
                updated_at: Set(now.clone()),
                ..Default::default()
            }
            .update(&self.inner.db)
            .await?;
        }

        let live_root = self.inner.paths.download_dir.join("live");
        let residual_count = count_residual_segments(&live_root);
        if residual_count > 0 {
            warn!(
                residual_count,
                root = %live_root.display(),
                "发现未合并的直播 FLV 分段，已保留文件供恢复"
            );
        }
        Ok(())
    }

    /// Gracefully stop every active session. One failed session must not keep
    /// the remaining FFmpeg children alive during application shutdown.
    pub async fn stop_all(&self) {
        let room_ids = {
            let sessions = self.inner.sessions.lock().await;
            sessions
                .iter()
                .filter_map(|(room_id, entry)| {
                    matches!(entry, SessionEntry::Active(_)).then_some(*room_id)
                })
                .collect::<Vec<_>>()
        };
        for room_id in room_ids {
            if let Err(error) = self.stop(room_id).await {
                warn!(room_id, "关闭程序时停止直播录制失败: {error}");
            }
        }
    }
}

struct RecordingWorker {
    room_id: i64,
    started_at: chrono::DateTime<chrono::Utc>,
    snapshot: Arc<Mutex<RecordingInfo>>,
    command_rx: mpsc::Receiver<SessionCommand>,
    reload_rx: mpsc::Receiver<()>,
    bili_api: Arc<BiliApi>,
    settings_service: Arc<SettingsService>,
    db: DatabaseConnection,
    recording_id: i32,
    ffmpeg_path: PathBuf,
    current_url: String,
    current_ffmpeg: Option<FfmpegSession>,
    live_dir: PathBuf,
    base_prefix: String,
    next_segment: u32,
    segments: Vec<PathBuf>,
    segment_index: Arc<AtomicU32>,
    danmu_cancel: CancellationToken,
    danmu_collector_handle: Option<tokio::task::JoinHandle<()>>,
    danmu_collector_rx: mpsc::Receiver<DanmuCollectorEvent>,
    danmu_collector_channel_open: bool,
    danmu_write_handle: Option<tokio::task::JoinHandle<Result<()>>>,
    danmu_path: Option<PathBuf>,
    danmu_failure_rx: mpsc::Receiver<String>,
    danmu_failure_channel_open: bool,
    reload_channel_open: bool,
    failure: Option<String>,
    stop_requested: bool,
}

impl RecordingWorker {
    async fn run(mut self) {
        let mut health_tick = interval(HEALTH_CHECK_INTERVAL);
        let mut refresh_requested = false;
        let mut refresh_failures = 0usize;
        let mut next_refresh_retry = Instant::now();

        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    match command {
                        Some(SessionCommand::Stop(reply)) => {
                            self.stop_requested = true;
                            let result = self.finalize().await;
                            let _ = reply.send(result);
                            return;
                        }
                        None => return,
                    }
                }
                reload = self.reload_rx.recv(), if self.reload_channel_open => {
                    if reload.is_some() {
                        refresh_requested = true;
                    } else {
                        // writer 任务结束后关闭该分支，避免 recv(None) 在 select 中忙循环。
                        self.reload_channel_open = false;
                    }
                }
                failure = self.danmu_failure_rx.recv(), if self.danmu_failure_channel_open => {
                    match failure {
                        Some(error) => self.mark_danmu_unavailable(format!("弹幕文件写入失败: {error}")).await,
                        None => self.danmu_failure_channel_open = false,
                    }
                }
                collector = self.danmu_collector_rx.recv(), if self.danmu_collector_channel_open => {
                    match collector {
                        Some(DanmuCollectorEvent::Failed(error)) => {
                            self.mark_danmu_unavailable(format!("弹幕采集任务异常退出: {error}")).await;
                        }
                        Some(DanmuCollectorEvent::Exited) if !self.danmu_cancel.is_cancelled() => {
                            self.mark_danmu_unavailable("弹幕采集任务意外退出".to_string()).await;
                        }
                        Some(DanmuCollectorEvent::Exited) => {}
                        None => self.danmu_collector_channel_open = false,
                    }
                }
                _ = health_tick.tick() => {
                    if self.observe_process().await {
                        // 进程异常退出后仍保留 worker，让 API 可以看到 failed，
                        // 用户随后调用 stop 时仍会完成弹幕和分段收尾。
                        refresh_requested = false;
                    } else if self.current_ffmpeg.is_some()
                        && is_expiring_soon(&self.current_url, URL_REFRESH_MARGIN_SECS)
                    {
                        refresh_requested = true;
                    }

                    if refresh_requested
                        && self.current_ffmpeg.is_some()
                        && Instant::now() >= next_refresh_retry
                    {
                        match self.refresh_segment().await {
                            Ok(()) => {
                                refresh_requested = false;
                                refresh_failures = 0;
                                next_refresh_retry = Instant::now();
                            }
                            Err(error) => {
                                refresh_failures = refresh_failures.saturating_add(1);
                                let backoff = match refresh_failures {
                                    1 => Duration::from_secs(5),
                                    2 => Duration::from_secs(15),
                                    _ => Duration::from_secs(30),
                                };
                                next_refresh_retry = Instant::now() + backoff;
                                warn!(
                                    room_id = self.room_id,
                                    ?backoff,
                                    "直播流地址刷新失败，将重试: {error}"
                                );
                            }
                        }
                    }
                    self.update_snapshot().await;
                }
            }
        }
    }

    /// 返回 true 表示进程已退出或状态检查失败。
    async fn observe_process(&mut self) -> bool {
        let process_result = self.current_ffmpeg.as_mut().map(FfmpegSession::try_wait);
        let Some(process_result) = process_result else {
            return true;
        };

        match process_result {
            Ok(None) => false,
            Ok(Some(status)) => {
                let diagnostics = if let Some(session) = self.current_ffmpeg.as_mut() {
                    session.diagnostics().await
                } else {
                    String::new()
                };
                self.current_ffmpeg = None;
                let detail = if diagnostics.is_empty() {
                    format!("FFmpeg 未经停止请求提前退出: status={status}")
                } else {
                    format!("FFmpeg 未经停止请求提前退出: status={status}; {diagnostics}")
                };
                self.mark_failure(detail).await;
                true
            }
            Err(error) => {
                self.mark_failure(format!("检查 FFmpeg 进程状态失败: {error}"))
                    .await;
                true
            }
        }
    }

    async fn refresh_segment(&mut self) -> Result<()> {
        let cookies = self.settings_service.cookie_header().await?;
        let playurl = self
            .bili_api
            .live_playurl(self.room_id, None, &cookies)
            .await
            .context("刷新直播流地址失败")?;
        let selected_stream = select_best_stream(&playurl.durl)?;
        let new_url = selected_stream.url.clone();
        let new_path = segment_path(&self.live_dir, &self.base_prefix, self.next_segment);

        // 新进程先启动；如果启动失败，旧分段仍继续录制，不丢当前会话。
        let new_ffmpeg = FfmpegSession::start(
            &self.ffmpeg_path,
            &new_url,
            new_path.clone(),
            self.room_id,
            user_agent(),
            referer(),
        )?;

        if let Some(mut old_ffmpeg) = self.current_ffmpeg.take() {
            if let Err(error) = old_ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                warn!(
                    room_id = self.room_id,
                    "停止旧直播分段失败，已强制回收: {error}"
                );
            }
        }

        self.current_ffmpeg = Some(new_ffmpeg);
        self.current_url = new_url;
        {
            let mut snapshot = self.snapshot.lock().await;
            snapshot.stream_quality =
                (selected_stream.current_qn > 0).then_some(selected_stream.current_qn);
            snapshot.stream_protocol = (!selected_stream.protocol_name.is_empty())
                .then_some(selected_stream.protocol_name);
            snapshot.stream_format =
                (!selected_stream.format_name.is_empty()).then_some(selected_stream.format_name);
            snapshot.stream_codec =
                (!selected_stream.codec_name.is_empty()).then_some(selected_stream.codec_name);
        }
        self.segments.push(new_path);
        self.segment_index.fetch_add(1, Ordering::Relaxed);
        self.next_segment = self.next_segment.saturating_add(1);
        info!(
            room_id = self.room_id,
            segment = self.next_segment - 1,
            "直播流已切换到新分段"
        );
        Ok(())
    }

    async fn finalize(&mut self) -> Result<RecordingInfo> {
        if self.failure.is_none() {
            self.set_status(RecordingStatus::Stopping).await;
        }

        if let Some(mut ffmpeg) = self.current_ffmpeg.take() {
            if let Err(error) = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                self.mark_failure(format!("停止 FFmpeg 失败: {error}"))
                    .await;
            }
        }
        self.danmu_cancel.cancel();

        if let Some(handle) = self.danmu_collector_handle.take() {
            match tokio::time::timeout(DANMU_STOP_TIMEOUT, handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.mark_danmu_unavailable(format!("弹幕采集任务异常退出: {error}"))
                        .await
                }
                Err(_) => {
                    self.mark_danmu_unavailable("等待弹幕采集任务超时".to_string())
                        .await
                }
            }
        }
        if let Some(handle) = self.danmu_write_handle.take() {
            match tokio::time::timeout(DANMU_STOP_TIMEOUT, handle).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    self.mark_danmu_unavailable(format!("弹幕文件写入失败: {error}"))
                        .await
                }
                Ok(Err(error)) => {
                    self.mark_danmu_unavailable(format!("弹幕写入任务异常退出: {error}"))
                        .await
                }
                Err(_) => {
                    self.mark_danmu_unavailable("等待弹幕写入任务超时".to_string())
                        .await
                }
            }
        }

        let (final_path, merge_error) =
            match merge_segments_to_mp4(&self.ffmpeg_path, &self.segments).await {
                Ok(path) => {
                    for segment in &self.segments {
                        if let Err(error) = tokio::fs::remove_file(segment).await {
                            debug!(path = %segment.display(), "删除已合并的直播分段失败: {error}");
                        }
                    }
                    (path, None)
                }
                Err(error) => (
                    self.segments
                        .first()
                        .cloned()
                        .unwrap_or_else(|| self.live_dir.join(&self.base_prefix)),
                    Some(error.to_string()),
                ),
            };
        if let Some(error) = merge_error {
            self.mark_failure(format!("直播分段合并失败: {error}"))
                .await;
        }

        let final_status = if self.failure.is_some() {
            RecordingStatus::Failed
        } else if self.stop_requested {
            RecordingStatus::Stopped
        } else {
            RecordingStatus::Completed
        };
        let file_size = tokio::fs::metadata(&final_path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let duration = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .num_seconds();
        let mut info = self.snapshot.lock().await.clone();
        info.status = final_status.clone();
        info.output_path = final_path.to_string_lossy().to_string();
        info.duration_secs = duration;
        info.file_size = file_size as i64;
        info.error_msg = self.failure.clone();
        if info.capture_mode != "off" && info.interaction_capture_status == "capturing" {
            info.interaction_capture_status = "completed".to_owned();
        }
        self.persist_recording(&info, true).await;
        *self.snapshot.lock().await = info.clone();
        info!(
            room_id = self.room_id,
            ?final_status,
            duration_secs = duration,
            "直播录制已完成收尾"
        );
        Ok(info)
    }

    async fn mark_failure(&mut self, error: String) {
        if self.failure.is_none() {
            warn!(room_id = self.room_id, "直播录制进入失败状态: {error}");
            self.failure = Some(error);
        }
        self.danmu_cancel.cancel();
        {
            let mut snapshot = self.snapshot.lock().await;
            snapshot.status = RecordingStatus::Failed;
            snapshot.error_msg = self.failure.clone();
        }
        let snapshot = self.snapshot.lock().await.clone();
        self.persist_recording(&snapshot, false).await;
    }

    async fn mark_danmu_unavailable(&mut self, error: String) {
        warn!(
            room_id = self.room_id,
            "弹幕录制不可用，视频录制继续: {error}"
        );
        self.danmu_cancel.cancel();
        self.snapshot.lock().await.danmu_unavailable = true;
        let mut snapshot = self.snapshot.lock().await;
        snapshot.interaction_capture_status = "unavailable".to_owned();
        snapshot.interaction_error = Some(error);
    }

    async fn set_status(&self, status: RecordingStatus) {
        self.snapshot.lock().await.status = status;
    }

    async fn update_snapshot(&self) {
        let size = total_file_size(&self.segments).await;
        let output_path = self
            .current_ffmpeg
            .as_ref()
            .map(|session| session.output_path().to_string_lossy().to_string())
            .or_else(|| {
                self.segments
                    .last()
                    .map(|path| path.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| {
                self.live_dir
                    .join(&self.base_prefix)
                    .to_string_lossy()
                    .to_string()
            });
        let duration = chrono::Utc::now()
            .signed_duration_since(self.started_at)
            .num_seconds();
        let mut snapshot = self.snapshot.lock().await;
        snapshot.output_path = output_path;
        snapshot.duration_secs = duration;
        snapshot.file_size = size as i64;
        snapshot.error_msg = self.failure.clone();
        if self.failure.is_some() {
            snapshot.status = RecordingStatus::Failed;
        }
    }

    async fn persist_recording(&self, info: &RecordingInfo, ended: bool) {
        let update = live_recording::ActiveModel {
            id: Set(self.recording_id),
            status: Set(info.status.to_string()),
            output_path: Set((!info.output_path.is_empty()).then_some(info.output_path.clone())),
            danmu_path: Set(self
                .danmu_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string())),
            file_size: Set(info.file_size),
            duration: Set(info.duration_secs),
            ended_at: if ended {
                Set(Some(chrono::Utc::now().to_rfc3339()))
            } else {
                sea_orm::ActiveValue::NotSet
            },
            error_msg: Set(info.error_msg.clone()),
            event_path: Set(info.event_path.clone()),
            xml_path: Set(info.xml_path.clone()),
            summary_path: Set(info.summary_path.clone()),
            capture_mode: Set(info.capture_mode.clone()),
            interaction_status: Set(info.interaction_capture_status.clone()),
            interaction_error: Set(info.interaction_error.clone()),
            danmaku_count: Set(info.danmaku_count),
            unique_user_count: Set(info.unique_user_count),
            free_gift_count: Set(info.free_gift_count),
            paid_gift_count: Set(info.paid_gift_count),
            sc_count: Set(info.sc_count),
            guard_count: Set(info.guard_count),
            peak_watched: Set(info.peak_watched),
            dropped_event_count: Set(info.dropped_event_count),
            estimated_paid_value: Set(info.estimated_paid_value),
            updated_at: Set(chrono::Utc::now().to_rfc3339()),
            ..Default::default()
        };
        if let Err(error) = update.update(&self.db).await {
            warn!(room_id = self.room_id, "更新直播录制记录失败: {error}");
        }
    }
}

fn segment_path(live_dir: &Path, base_prefix: &str, index: u32) -> PathBuf {
    live_dir.join(format!("{base_prefix}_segment_{index:04}.flv"))
}

fn user_agent() -> &'static str {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
}

fn referer() -> &'static str {
    "https://live.bilibili.com/"
}

async fn total_file_size(paths: &[PathBuf]) -> u64 {
    let mut total = 0;
    for path in paths {
        total += tokio::fs::metadata(path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
    }
    total
}

fn count_residual_segments(root: &Path) -> usize {
    if !root.is_dir() {
        return 0;
    }
    let mut count = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("_segment_") && name.ends_with(".flv"))
            {
                count += 1;
            }
        }
    }
    count
}

fn sanitize_filename(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect::<String>()
        .trim()
        .to_string();
    if sanitized.is_empty() {
        "直播录制".to_string()
    } else {
        sanitized
    }
}
