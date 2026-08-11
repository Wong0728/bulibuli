//! 直播录制服务：管理并发录制、FFmpeg 监督和流地址分段轮换。
pub mod ffmpeg_session;
mod interactions;
pub mod stream_url;

use crate::config::AppPaths;
use crate::models::live_recording;
use crate::models::live_source;
use crate::services::bili_api::models::live::LiveStreamUrl;
use crate::services::bili_api::BiliApi;
use crate::services::danmu_collector::DanmuCollector;
use crate::services::file_safety::sanitize_filename;
use crate::services::live_source::CaptureMode;
use crate::services::settings::SettingsService;
use crate::services::video_processor::VideoProcessor;
use anyhow::{anyhow, Context, Result};
use ffmpeg_session::{
    merge_segments_to_mp4, merge_segments_to_mp4_cancelable, redact_diagnostics, FfmpegSession,
};
pub use interactions::ArchivedLiveEvent;
use interactions::{InteractionPaths, InteractionWriterArgs};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QueryResult, QuerySelect, Set, Statement,
};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use stream_url::{is_expiring_soon, select_stream_candidates};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{interval, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const DANMU_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const URL_REFRESH_MARGIN_SECS: i64 = 60;
const CONSERVATIVE_URL_REFRESH: Duration = Duration::from_secs(15 * 60);
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_RECORDING_FILE_SIZE: u64 = 200 * 1024 * 1024 * 1024;
const MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS: u32 = 3;
const STOP_REASON_MANUAL: &str = "manual_stop";
const STOP_REASON_OFFLINE_END: &str = "stream_ended_after_offline_confirmation";
const STOP_REASON_UNRECOVERABLE_EXIT: &str = "ffmpeg_exit_while_live_or_unconfirmed";
const STOP_REASON_FAILED: &str = "recording_failed";
const STOP_REASON_COMPLETED: &str = "recording_completed";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnexpectedExitAction {
    CompleteAfterOfflineConfirmation,
    Recover,
    FailRecoverable,
}

fn unexpected_exit_action(
    room_is_offline: Option<bool>,
    restart_attempts: u32,
) -> UnexpectedExitAction {
    match room_is_offline {
        Some(true) => UnexpectedExitAction::CompleteAfterOfflineConfirmation,
        Some(false) | None if restart_attempts < MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS => {
            UnexpectedExitAction::Recover
        }
        Some(false) | None => UnexpectedExitAction::FailRecoverable,
    }
}

/// 录制状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingStatus {
    Starting,
    Recording,
    Stopping,
    Finalizing,
    Stopped,
    Completed,
    Failed,
    Cancelled,
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
            Self::Finalizing => write!(f, "finalizing"),
            Self::Stopped => write!(f, "stopped"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// 对外暴露的录制信息。
#[derive(Clone, Debug, Serialize)]
pub struct RecordingInfo {
    pub room_id: i64,
    pub recording_id: Option<i32>,
    pub title: String,
    pub status: RecordingStatus,
    /// Internal filesystem path; deliberately omitted from API serialization.
    #[serde(skip_serializing)]
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
    #[serde(skip_serializing)]
    pub event_path: Option<String>,
    #[serde(skip_serializing)]
    pub xml_path: Option<String>,
    #[serde(skip_serializing)]
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

#[derive(Clone, Debug, Serialize)]
pub struct MergeJobInfo {
    pub id: String,
    pub recording_id: i32,
    pub status: String,
    pub progress: u8,
    pub error: Option<String>,
    pub source_segment_count: usize,
    pub cancel_requested: bool,
    pub created_at: String,
    pub updated_at: String,
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
    sessions: Arc<Mutex<HashMap<i64, SessionEntry>>>,
    merge_jobs: Arc<Mutex<HashMap<String, MergeJobInfo>>>,
    merge_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    bili_api: Arc<BiliApi>,
    video_processor: Arc<VideoProcessor>,
    paths: Arc<AppPaths>,
    settings_service: Arc<SettingsService>,
    db: DatabaseConnection,
}

enum SessionEntry {
    Starting {
        snapshot: Arc<Mutex<RecordingInfo>>,
        cancellation: CancellationToken,
    },
    Active(RecordingSessionHandle),
    Stopping {
        snapshot: Arc<Mutex<RecordingInfo>>,
        job_id: String,
    },
}

#[derive(Clone)]
struct RecordingSessionHandle {
    snapshot: Arc<Mutex<RecordingInfo>>,
    command_tx: mpsc::Sender<SessionCommand>,
    recent: Arc<Mutex<VecDeque<ArchivedLiveEvent>>>,
}

enum SessionCommand {
    Stop(oneshot::Sender<Result<RecordingInfo>>),
    StopBackground {
        job_id: String,
        reply: oneshot::Sender<String>,
    },
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
                sessions: Arc::new(Mutex::new(HashMap::new())),
                merge_jobs: Arc::new(Mutex::new(HashMap::new())),
                merge_cancellations: Arc::new(Mutex::new(HashMap::new())),
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
        let max_concurrent = self.inner.settings_service.current().live.max_concurrent;
        if self.inner.sessions.lock().await.len() >= max_concurrent {
            return Err(anyhow!(
                "已达到直播录制并发上限 ({max_concurrent} 路)，可在系统设置中调整"
            ));
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
            recording_id: None,
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
            if sessions.len() >= max_concurrent {
                return Err(anyhow!("recording concurrency limit reached"));
            }
            if sessions.contains_key(&room_id) {
                return Err(anyhow!("直播间 {room_id} 已在录制中或正在启动"));
            }
            sessions.insert(
                room_id,
                SessionEntry::Starting {
                    snapshot: startup_snapshot,
                    cancellation: CancellationToken::new(),
                },
            );
        }
        let startup_cancellation = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Starting { cancellation, .. }) => cancellation.clone(),
                _ => return Err(anyhow!("直播录制启动已取消")),
            }
        };

        let result = self
            .start_session(
                &init,
                &cookies,
                trigger,
                capture_mode,
                &startup_cancellation,
            )
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
        startup_cancellation: &CancellationToken,
    ) -> Result<RecordingInfo> {
        ensure_startup_active(startup_cancellation)?;
        let room_id = init.room_id;
        let info = self
            .inner
            .bili_api
            .live_get_info(room_id, cookies)
            .await
            .context("获取直播间信息失败")?;
        ensure_startup_active(startup_cancellation)?;
        let max_qn = source_max_qn(&self.inner.db, room_id).await;
        let playurl = self
            .inner
            .bili_api
            .live_playurl(room_id, max_qn, cookies)
            .await
            .context("获取直播流地址失败")?;
        ensure_startup_active(startup_cancellation)?;
        let stream_candidates = select_stream_candidates(&playurl.durl)?;
        let selected_stream = stream_candidates
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("流地址列表为空"))?;
        let stream_url = selected_stream.url.clone();

        let (ffmpeg_path, _) = self.inner.video_processor.detect_ffmpeg("auto", None).await;
        let ffmpeg_path = ffmpeg_path.ok_or_else(|| anyhow!("未找到 FFmpeg，无法录制"))?;

        let live_dir = self
            .inner
            .paths
            .download_dir
            .join("live")
            .join(room_id.to_string());
        let available = fs2::available_space(&self.inner.paths.download_dir)
            .context("检查直播录制磁盘空间失败")?;
        let live_cfg = self.inner.settings_service.current().live.clone();
        let min_free_bytes = live_cfg.min_free_space_gib * 1024 * 1024 * 1024;
        if available < min_free_bytes {
            return Err(anyhow!(
                "可用磁盘空间低于 {} GiB 安全阈值，已拒绝启动直播录制",
                live_cfg.min_free_space_gib
            ));
        }
        let now_local = chrono::Local::now();
        let timestamp = now_local.format("%Y%m%d_%H%M%S");
        let rendered = render_file_template(
            &live_cfg.file_name_template,
            room_id,
            &info.title,
            now_local,
        );
        let safe_name = sanitize_filename(&rendered);
        // 保留短时间戳与随机后缀，避免同日多场直播文件名碰撞
        let unique_suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let base_prefix = format!("{safe_name}_{timestamp}_{unique_suffix}");
        let first_segment = segment_path(&live_dir, &base_prefix, 0);
        let started_at = chrono::Utc::now();
        let interaction_paths = InteractionPaths {
            legacy: live_dir.join(format!("{base_prefix}_danmu.json")),
            events: live_dir.join(format!("{base_prefix}_events.jsonl")),
            xml: live_dir.join(format!("{base_prefix}_danmaku.xml")),
            standard_xml: live_dir.join(format!("{base_prefix}_danmaku_bilibili.xml")),
            summary: live_dir.join(format!("{base_prefix}_interaction_summary.json")),
        };
        let initial_info = RecordingInfo {
            room_id,
            recording_id: None,
            title: info.title.clone(),
            status: RecordingStatus::Starting,
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
        // The collector owns the sender.  Do not share this cancellation token
        // with the writer: cancellation must first close the sender and let the
        // writer drain every queued event before it finalizes the archive.
        let collector_cancel = CancellationToken::new();
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
                        collector_cancel.clone(),
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
                            let writer_args = InteractionWriterArgs {
                                room_id,
                                title: info.title.clone(),
                                mode: capture_mode,
                                started_at,
                                paths: interaction_paths.clone(),
                                snapshot: snapshot.clone(),
                                recent: recent.clone(),
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
            status: Set(RecordingStatus::Starting.to_string()),
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
                collector_cancel.cancel();
                if let Some(handle) = danmu_collector_monitor.take() {
                    handle.abort();
                }
                if let Some(handle) = danmu_write_handle.take() {
                    handle.abort();
                }
                return Err(error).context("保存直播录制记录失败");
            }
        };
        snapshot.lock().await.recording_id = Some(recording.id);
        if let Err(error) =
            persist_segment(&self.inner.db, recording.id, 0, &first_segment, "open").await
        {
            collector_cancel.cancel();
            if let Some(handle) = danmu_collector_monitor.take() {
                handle.abort();
            }
            if let Some(handle) = danmu_write_handle.take() {
                handle.abort();
            }
            mark_startup_recording(
                &self.inner.db,
                recording.id,
                RecordingStatus::Failed,
                format!("persist initial recording segment failed: {error}"),
            )
            .await;
            return Err(error).context("persist initial recording segment failed");
        }
        // FFmpeg is intentionally the final startup side effect: all metadata,
        // interaction wiring and the durable recording row already exist.
        let mut ffmpeg = match FfmpegSession::start(
            &ffmpeg_path,
            &stream_url,
            first_segment.clone(),
            room_id,
            user_agent(),
            referer(),
        ) {
            Ok(session) => session,
            Err(error) => {
                collector_cancel.cancel();
                if let Some(handle) = danmu_collector_monitor.take() {
                    handle.abort();
                }
                if let Some(handle) = danmu_write_handle.take() {
                    handle.abort();
                }
                let failed = live_recording::ActiveModel {
                    id: Set(recording.id),
                    status: Set(RecordingStatus::Failed.to_string()),
                    ended_at: Set(Some(chrono::Utc::now().to_rfc3339())),
                    error_msg: Set(Some(format!("启动 FFmpeg 失败: {error}"))),
                    updated_at: Set(chrono::Utc::now().to_rfc3339()),
                    ..Default::default()
                };
                let _ = failed.update(&self.inner.db).await;
                return Err(error);
            }
        };
        if startup_cancellation.is_cancelled() {
            collector_cancel.cancel();
            if let Some(handle) = danmu_collector_monitor.take() {
                handle.abort();
            }
            if let Some(handle) = danmu_write_handle.take() {
                handle.abort();
            }
            let _ = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await;
            mark_startup_recording(
                &self.inner.db,
                recording.id,
                RecordingStatus::Cancelled,
                "启动已由用户取消".to_owned(),
            )
            .await;
            return Err(anyhow!("直播录制启动已取消"));
        }
        {
            let mut state = snapshot.lock().await;
            state.status = RecordingStatus::Recording;
        }
        let initial_info = snapshot.lock().await.clone();
        let started = live_recording::ActiveModel {
            id: Set(recording.id),
            status: Set(RecordingStatus::Recording.to_string()),
            interaction_status: Set(initial_info.interaction_capture_status.clone()),
            interaction_error: Set(initial_info.interaction_error.clone()),
            updated_at: Set(chrono::Utc::now().to_rfc3339()),
            ..Default::default()
        };
        if let Err(error) = started.update(&self.inner.db).await {
            let _ = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await;
            mark_startup_recording(
                &self.inner.db,
                recording.id,
                RecordingStatus::Failed,
                format!("update recording startup state failed: {error}"),
            )
            .await;
            return Err(error.into());
        }
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
            stream_candidates,
            candidate_index: 0,
            current_ffmpeg: Some(ffmpeg),
            live_dir,
            base_prefix,
            next_segment: 1,
            segments: vec![first_segment],
            segment_index,
            collector_cancel,
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
            last_url_refresh: Instant::now(),
            last_checkpoint: Instant::now(),
            restart_attempts: 0,
            stop_reason: None,
            is_recoverable: false,
            unexpected_exit_detail: None,
            sessions: self.inner.sessions.clone(),
            merge_jobs: self.inner.merge_jobs.clone(),
            merge_cancellations: self.inner.merge_cancellations.clone(),
        };
        self.inner
            .sessions
            .lock()
            .await
            .insert(room_id, SessionEntry::Active(handle));
        tokio::spawn(async move { worker.run().await });
        info!(room_id, "直播录制已开始");
        Ok(initial_info)
    }

    /// 停止录制并等待 worker 完成弹幕收尾和分段合并。
    pub async fn stop(&self, room_id: i64) -> Result<RecordingInfo> {
        let startup = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(_)) => None,
                Some(SessionEntry::Starting {
                    snapshot,
                    cancellation,
                }) => {
                    cancellation.cancel();
                    Some(snapshot.clone())
                }
                Some(SessionEntry::Stopping { snapshot, .. }) => {
                    let snapshot = snapshot.clone();
                    drop(sessions);
                    return Ok(snapshot.lock().await.clone());
                }
                None => return Err(anyhow!("直播间 {room_id} 未在录制")),
            }
        };

        if let Some(snapshot) = startup {
            let result = {
                let mut info = snapshot.lock().await;
                info.status = RecordingStatus::Cancelled;
                info.error_msg = Some("启动已由用户取消".to_owned());
                info.clone()
            };
            self.inner.sessions.lock().await.remove(&room_id);
            return Ok(result);
        }
        let handle = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(handle)) => handle.clone(),
                Some(SessionEntry::Stopping { snapshot, .. }) => {
                    let snapshot = snapshot.clone();
                    drop(sessions);
                    return Ok(snapshot.lock().await.clone());
                }
                _ => return Err(anyhow!("直播间 {room_id} 未在录制")),
            }
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        tokio::time::timeout(
            Duration::from_secs(5),
            handle.command_tx.send(SessionCommand::Stop(reply_tx)),
        )
        .await
        .map_err(|_| anyhow!("发送停止命令超时"))?
        .map_err(|_| anyhow!("直播录制 worker 已退出"))?;
        let result = tokio::time::timeout(Duration::from_secs(30), reply_rx)
            .await
            .map_err(|_| anyhow!("等待直播录制停止结果超时"))?
            .map_err(|_| anyhow!("直播录制 worker 未返回停止结果"))?;
        self.inner.sessions.lock().await.remove(&room_id);
        result
    }

    /// Request stop without keeping the HTTP request open while interaction
    /// drain and FFmpeg/ffprobe merge complete.
    pub async fn request_stop(&self, room_id: i64) -> Result<MergeJobInfo> {
        let (handle, existing_job) = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(handle)) => (Some(handle.clone()), None),
                Some(SessionEntry::Stopping { job_id, .. }) => (None, Some(job_id.clone())),
                Some(SessionEntry::Starting { cancellation, .. }) => {
                    cancellation.cancel();
                    return Err(anyhow!("recording startup cancellation requested"));
                }
                None => return Err(anyhow!("recording session not found")),
            }
        };
        if let Some(job_id) = existing_job {
            return self
                .merge_job(&job_id)
                .await
                .ok_or_else(|| anyhow!("stop operation not found"));
        }
        let handle = handle.ok_or_else(|| anyhow!("recording session is not active"))?;
        let recording_id = handle
            .snapshot
            .lock()
            .await
            .recording_id
            .ok_or_else(|| anyhow!("recording id not available"))?;
        if let Some(job) = self.find_active_merge_job(recording_id).await? {
            return Ok(job);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let job = MergeJobInfo {
            id: uuid::Uuid::new_v4().simple().to_string(),
            recording_id,
            status: "queued".to_owned(),
            progress: 0,
            error: None,
            source_segment_count: 0,
            cancel_requested: false,
            created_at: now.clone(),
            updated_at: now,
        };
        let job_id = job.id.clone();
        let cancellation = CancellationToken::new();
        handle.snapshot.lock().await.status = RecordingStatus::Stopping;
        self.inner
            .merge_jobs
            .lock()
            .await
            .insert(job_id.clone(), job.clone());
        self.inner
            .merge_cancellations
            .lock()
            .await
            .insert(job_id.clone(), cancellation);
        let existing_job = {
            let mut sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(SessionEntry::Active(_)) => {
                    sessions.insert(
                        room_id,
                        SessionEntry::Stopping {
                            snapshot: handle.snapshot.clone(),
                            job_id: job_id.clone(),
                        },
                    );
                    None
                }
                Some(SessionEntry::Stopping { job_id, .. }) => Some(job_id.clone()),
                _ => Some(String::new()),
            }
        };
        if let Some(existing_job_id) = existing_job {
            self.inner.merge_jobs.lock().await.remove(&job_id);
            self.inner.merge_cancellations.lock().await.remove(&job_id);
            if existing_job_id.is_empty() {
                return Err(anyhow!("recording session is no longer active"));
            }
            return self
                .merge_job(&existing_job_id)
                .await
                .ok_or_else(|| anyhow!("stop operation not found"));
        }
        if let Err(error) = persist_merge_job(&self.inner.db, &job).await {
            update_merge_job(
                &self.inner,
                &job_id,
                "failed",
                100,
                Some("failed to persist stop operation".to_owned()),
            )
            .await;
            return Err(error);
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        if handle
            .command_tx
            .send(SessionCommand::StopBackground {
                job_id: job_id.clone(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            update_merge_job(
                &self.inner,
                &job_id,
                "failed",
                100,
                Some("recording worker stopped".to_owned()),
            )
            .await;
            return Err(anyhow!("recording worker stopped"));
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), reply_rx).await;
        self.merge_job(&job_id)
            .await
            .ok_or_else(|| anyhow!("stop operation disappeared"))
    }

    /// 获取所有活跃录制会话的快照，不持有 sessions 锁执行文件或网络 I/O。
    pub async fn status_all(&self) -> Vec<RecordingInfo> {
        let handles = {
            let sessions = self.inner.sessions.lock().await;
            sessions
                .values()
                .map(|entry| match entry {
                    SessionEntry::Active(handle) => handle.snapshot.clone(),
                    SessionEntry::Starting { snapshot, .. } => snapshot.clone(),
                    SessionEntry::Stopping { snapshot, .. } => snapshot.clone(),
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
                SessionEntry::Starting { snapshot, .. } => snapshot.clone(),
                SessionEntry::Stopping { snapshot, .. } => snapshot.clone(),
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

    /// Completed and failed sessions are retained as first-class product data;
    /// callers receive the database record rather than an in-memory session.
    pub async fn history(&self, limit: usize) -> Result<Vec<live_recording::Model>> {
        Ok(live_recording::Entity::find()
            .order_by_desc(live_recording::Column::StartedAt)
            .limit(limit.clamp(1, 100) as u64)
            .all(&self.inner.db)
            .await?)
    }

    pub async fn history_item(&self, recording_id: i32) -> Result<Option<live_recording::Model>> {
        Ok(live_recording::Entity::find_by_id(recording_id)
            .one(&self.inner.db)
            .await?)
    }

    pub async fn merge_jobs(&self) -> Vec<MergeJobInfo> {
        match self
            .inner
            .db
            .query_all_raw(Statement::from_string(
                self.inner.db.get_database_backend(),
                "SELECT id, recording_id, status, progress, error_msg, source_segment_count,
                        cancel_requested, created_at, updated_at
                   FROM live_merge_jobs
                  ORDER BY updated_at DESC
                  LIMIT 200"
                    .to_owned(),
            ))
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| match merge_job_from_row(&row) {
                    Ok(job) => Some(job),
                    Err(error) => {
                        warn!("read persisted merge job failed: {error}");
                        None
                    }
                })
                .collect(),
            Err(error) => {
                warn!("read persisted merge jobs failed: {error}");
                Vec::new()
            }
        }
    }

    pub async fn merge_job(&self, job_id: &str) -> Option<MergeJobInfo> {
        if let Some(job) = self.inner.merge_jobs.lock().await.get(job_id).cloned() {
            return Some(job);
        }
        let row = self
            .inner
            .db
            .query_one_raw(Statement::from_sql_and_values(
                self.inner.db.get_database_backend(),
                "SELECT id, recording_id, status, progress, error_msg, source_segment_count,
                        cancel_requested, created_at, updated_at
                   FROM live_merge_jobs WHERE id = ?"
                    .to_owned(),
                [job_id.to_owned().into()],
            ))
            .await
            .ok()??;
        merge_job_from_row(&row).ok()
    }

    /// Rebuild a failed/recoverable recording in the background.  The source
    /// FLV segments are never removed until FFmpeg and ffprobe both succeed.
    pub async fn retry_merge(&self, recording_id: i32) -> Result<MergeJobInfo> {
        let row = live_recording::Entity::find_by_id(recording_id)
            .one(&self.inner.db)
            .await?
            .ok_or_else(|| anyhow!("recording not found"))?;
        let segment_dir = self
            .inner
            .paths
            .download_dir
            .join("live")
            .join(row.room_id.to_string());
        let segments = find_recording_segments(&segment_dir, row.output_path.as_deref()).await;
        if segments.is_empty() {
            return Err(anyhow!("no recoverable recording segments found"));
        }
        let (ffmpeg_path, _) = self.inner.video_processor.detect_ffmpeg("auto", None).await;
        let ffmpeg_path = ffmpeg_path.ok_or_else(|| anyhow!("FFmpeg not found"))?;
        if let Some(job) = self.find_active_merge_job(recording_id).await? {
            return Ok(job);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let job = MergeJobInfo {
            id: uuid::Uuid::new_v4().simple().to_string(),
            recording_id,
            status: "queued".to_owned(),
            progress: 0,
            error: None,
            source_segment_count: segments.len(),
            cancel_requested: false,
            created_at: now.clone(),
            updated_at: now,
        };
        self.start_merge_job(job, ffmpeg_path, segments).await
    }

    async fn find_active_merge_job(&self, recording_id: i32) -> Result<Option<MergeJobInfo>> {
        let row = self
            .inner
            .db
            .query_one_raw(Statement::from_sql_and_values(
                self.inner.db.get_database_backend(),
                "SELECT id, recording_id, status, progress, error_msg, source_segment_count,
                        cancel_requested, created_at, updated_at
                   FROM live_merge_jobs
                  WHERE recording_id = ? AND status IN ('queued', 'running', 'cancelling')
                  ORDER BY updated_at DESC LIMIT 1"
                    .to_owned(),
                [recording_id.into()],
            ))
            .await?;
        row.as_ref().map(merge_job_from_row).transpose()
    }

    async fn start_merge_job(
        &self,
        job: MergeJobInfo,
        ffmpeg_path: PathBuf,
        segments: Vec<PathBuf>,
    ) -> Result<MergeJobInfo> {
        let cancellation = CancellationToken::new();
        self.inner
            .merge_jobs
            .lock()
            .await
            .insert(job.id.clone(), job.clone());
        if let Err(error) = persist_merge_job(&self.inner.db, &job).await {
            self.inner.merge_jobs.lock().await.remove(&job.id);
            if let Some(existing) = self.find_active_merge_job(job.recording_id).await? {
                return Ok(existing);
            }
            return Err(error);
        }
        self.inner
            .merge_cancellations
            .lock()
            .await
            .insert(job.id.clone(), cancellation.clone());
        let inner = self.inner.clone();
        tokio::spawn(run_merge_job(
            inner,
            job.id.clone(),
            job.recording_id,
            ffmpeg_path,
            segments,
            cancellation,
        ));
        Ok(job)
    }

    pub async fn cancel_merge(&self, job_id: &str) -> Result<MergeJobInfo> {
        if !self.inner.merge_jobs.lock().await.contains_key(job_id) {
            let job = self
                .merge_job(job_id)
                .await
                .ok_or_else(|| anyhow!("merge job not found"))?;
            self.inner
                .merge_jobs
                .lock()
                .await
                .insert(job_id.to_owned(), job);
        }
        let mut jobs = self.inner.merge_jobs.lock().await;
        let job = jobs
            .get_mut(job_id)
            .ok_or_else(|| anyhow!("merge job not found"))?;
        if matches!(job.status.as_str(), "completed" | "failed" | "cancelled") {
            return Ok(job.clone());
        }
        job.cancel_requested = true;
        job.status = "cancelling".to_owned();
        job.updated_at = chrono::Utc::now().to_rfc3339();
        let result = job.clone();
        drop(jobs);
        if let Err(error) = persist_merge_job(&self.inner.db, &result).await {
            warn!(job_id, "persist merge cancellation failed: {error}");
        }
        if let Some(cancellation) = self.inner.merge_cancellations.lock().await.get(job_id) {
            cancellation.cancel();
        }
        Ok(result)
    }

    pub async fn recovery_items(&self) -> Result<Vec<serde_json::Value>> {
        let rows = live_recording::Entity::find()
            .order_by_desc(live_recording::Column::StartedAt)
            .limit(100)
            .all(&self.inner.db)
            .await?;
        let mut result = Vec::new();
        for row in rows {
            let dir = self
                .inner
                .paths
                .download_dir
                .join("live")
                .join(row.room_id.to_string());
            let count = find_recording_segments(&dir, row.output_path.as_deref())
                .await
                .len();
            if row.is_recoverable || count > 0 {
                result.push(serde_json::json!({
                    "recording_id": row.id, "room_id": row.room_id, "title": row.title,
                    "status": row.status, "segment_count": count,
                    "has_output": row.output_path.as_deref().is_some_and(|path| Path::new(path).exists()),
                    "is_recoverable": row.is_recoverable,
                    "error_msg": row.error_msg.as_deref().map(redact_diagnostics),
                }));
            }
        }
        Ok(result)
    }

    pub async fn restore_merge_jobs(&self) -> Result<()> {
        prune_persisted_merge_jobs(&self.inner.db).await?;
        let rows = self
            .inner
            .db
            .query_all_raw(Statement::from_string(
                self.inner.db.get_database_backend(),
                "SELECT id, recording_id, status, progress, error_msg, source_segment_count,
                        cancel_requested, created_at, updated_at
                   FROM live_merge_jobs
                  WHERE status IN ('queued', 'running', 'cancelling')
                  ORDER BY updated_at ASC"
                    .to_owned(),
            ))
            .await?;
        for row in rows {
            let mut job = merge_job_from_row(&row)?;
            let Some(recording) = live_recording::Entity::find_by_id(job.recording_id)
                .one(&self.inner.db)
                .await?
            else {
                job.status = "failed".to_owned();
                job.progress = 100;
                job.error = Some("recording not found after restart".to_owned());
                job.updated_at = chrono::Utc::now().to_rfc3339();
                persist_merge_job(&self.inner.db, &job).await?;
                continue;
            };
            let directory = self
                .inner
                .paths
                .download_dir
                .join("live")
                .join(recording.room_id.to_string());
            let segments =
                find_recording_segments(&directory, recording.output_path.as_deref()).await;
            let (ffmpeg_path, _) = self.inner.video_processor.detect_ffmpeg("auto", None).await;
            let Some(ffmpeg_path) = ffmpeg_path else {
                job.status = "failed".to_owned();
                job.progress = 100;
                job.error = Some("FFmpeg not found after restart".to_owned());
                job.updated_at = chrono::Utc::now().to_rfc3339();
                persist_merge_job(&self.inner.db, &job).await?;
                continue;
            };
            if segments.is_empty() {
                job.status = "failed".to_owned();
                job.progress = 100;
                job.error = Some("no recoverable recording segments found".to_owned());
                job.updated_at = chrono::Utc::now().to_rfc3339();
                persist_merge_job(&self.inner.db, &job).await?;
                continue;
            }
            self.inner
                .merge_jobs
                .lock()
                .await
                .insert(job.id.clone(), job.clone());
            let cancellation = CancellationToken::new();
            if job.status == "cancelling" || job.cancel_requested {
                cancellation.cancel();
            }
            self.inner
                .merge_cancellations
                .lock()
                .await
                .insert(job.id.clone(), cancellation.clone());
            tokio::spawn(run_merge_job(
                self.inner.clone(),
                job.id,
                recording.id,
                ffmpeg_path,
                segments,
                cancellation,
            ));
        }
        Ok(())
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
                error_msg: Set(Some(
                    "previous process ended before recording finalized".to_owned(),
                )),
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
                    matches!(
                        entry,
                        SessionEntry::Active(_) | SessionEntry::Starting { .. }
                    )
                    .then_some(*room_id)
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
    stream_candidates: Vec<LiveStreamUrl>,
    candidate_index: usize,
    current_ffmpeg: Option<FfmpegSession>,
    live_dir: PathBuf,
    base_prefix: String,
    next_segment: u32,
    segments: Vec<PathBuf>,
    segment_index: Arc<AtomicU32>,
    collector_cancel: CancellationToken,
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
    last_url_refresh: Instant,
    last_checkpoint: Instant,
    restart_attempts: u32,
    stop_reason: Option<&'static str>,
    is_recoverable: bool,
    unexpected_exit_detail: Option<String>,
    sessions: Arc<Mutex<HashMap<i64, SessionEntry>>>,
    merge_jobs: Arc<Mutex<HashMap<String, MergeJobInfo>>>,
    merge_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl RecordingWorker {
    async fn run(mut self) {
        let mut health_tick = interval(HEALTH_CHECK_INTERVAL);
        let mut refresh_requested = false;
        // 资源保护阈值在会话开始时从设置读取，运行中修改设置只影响新会话
        let live_limits = self.settings_service.current().live.clone();
        let min_free_bytes = live_limits.min_free_space_gib * 1024 * 1024 * 1024;
        let max_duration = chrono::Duration::hours(live_limits.max_duration_hours as i64);
        let mut refresh_failures = 0usize;
        let mut next_refresh_retry = Instant::now();

        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    match command {
                        Some(SessionCommand::Stop(reply)) => {
                            self.stop_requested = true;
                            self.stop_reason = Some(STOP_REASON_MANUAL);
                            let result = self.finalize(None).await;
                            let _ = reply.send(result);
                            return;
                        }
                        Some(SessionCommand::StopBackground { job_id, reply }) => {
                            self.stop_requested = true;
                            self.stop_reason = Some(STOP_REASON_MANUAL);
                            let job_snapshot = if let Some(job) = self.merge_jobs.lock().await.get_mut(&job_id) {
                                job.source_segment_count = self.segments.len();
                                job.updated_at = chrono::Utc::now().to_rfc3339();
                                Some(job.clone())
                            } else {
                                None
                            };
                            if let Some(job) = job_snapshot {
                                if let Err(error) = persist_merge_job(&self.db, &job).await {
                                    warn!(job_id = %job_id, "persist merge job failed: {error}");
                                }
                            }
                            let merge_cancellation = self
                                .merge_cancellations
                                .lock()
                                .await
                                .get(&job_id)
                                .cloned()
                                .unwrap_or_else(CancellationToken::new);
                            self.merge_cancellations
                                .lock()
                                .await
                                .insert(job_id.clone(), merge_cancellation.clone());
                            let merge_jobs = self.merge_jobs.clone();
                            let merge_cancellations = self.merge_cancellations.clone();
                            let sessions = self.sessions.clone();
                            let room_id = self.room_id;
                            let background_job_id = job_id.clone();
                            tokio::spawn(async move {
                                if let Some(job) = merge_jobs.lock().await.get_mut(&background_job_id) {
                                    job.status = "running".to_owned();
                                    job.progress = 10;
                                }
                                let result = self.finalize(Some(&merge_cancellation)).await;
                                let job_snapshot = if let Some(job) = merge_jobs.lock().await.get_mut(&background_job_id) {
                                    job.status = if result.is_ok() {
                                        "completed"
                                    } else if merge_cancellation.is_cancelled() {
                                        "cancelled"
                                    } else {
                                        "failed"
                                    }
                                    .to_owned();
                                    job.progress = 100;
                                    job.error = result
                                        .as_ref()
                                        .err()
                                        .map(ToString::to_string)
                                        .map(|error| redact_diagnostics(&error));
                                    job.updated_at = chrono::Utc::now().to_rfc3339();
                                    Some(job.clone())
                                } else {
                                    None
                                };
                                if let Some(job) = job_snapshot {
                                    if let Err(error) = persist_merge_job(&self.db, &job).await {
                                        warn!(job_id = %background_job_id, "persist merge job failed: {error}");
                                    }
                                }
                                merge_cancellations.lock().await.remove(&background_job_id);
                                sessions.lock().await.remove(&room_id);
                            });
                            let _ = reply.send(job_id);
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
                        Some(DanmuCollectorEvent::Exited) if !self.collector_cancel.is_cancelled() => {
                            self.mark_danmu_unavailable("弹幕采集任务意外退出".to_string()).await;
                        }
                        Some(DanmuCollectorEvent::Exited) => {}
                        None => self.danmu_collector_channel_open = false,
                    }
                }
                _ = health_tick.tick() => {
                    if fs2::available_space(&self.live_dir).unwrap_or(0) < min_free_bytes {
                        self.mark_failure(format!(
                            "可用磁盘空间低于 {} GiB 安全阈值，已安全停录",
                            live_limits.min_free_space_gib
                        ))
                        .await;
                        let _ = self.finalize(None).await;
                        self.sessions.lock().await.remove(&self.room_id);
                        return;
                    }
                    if total_file_size(&self.segments).await >= MAX_RECORDING_FILE_SIZE {
                        self.mark_failure("recording file size limit reached".to_owned()).await;
                        let _ = self.finalize(None).await;
                        self.sessions.lock().await.remove(&self.room_id);
                        return;
                    }
                    if self.observe_process().await {
                        refresh_requested = false;
                        if self.failure.is_some()
                            || self.stop_reason == Some(STOP_REASON_OFFLINE_END)
                        {
                            let _ = self.finalize(None).await;
                            self.sessions.lock().await.remove(&self.room_id);
                            return;
                        }
                    } else if self.current_ffmpeg.is_some()
                        && (is_expiring_soon(&self.current_url, URL_REFRESH_MARGIN_SECS)
                            || self.last_url_refresh.elapsed() >= CONSERVATIVE_URL_REFRESH)
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
                                // refresh_segment stops the old process before starting the next
                                // segment. Preserve the failure as an unexpected-exit recovery so
                                // the next health tick cannot leave a session active without FFmpeg.
                                self.unexpected_exit_detail = Some(format!(
                                    "刷新直播流分段失败: {error}"
                                ));
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
                    if self.last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                        let snapshot = self.snapshot.lock().await.clone();
                        self.persist_recording(&snapshot, false).await;
                        self.last_checkpoint = Instant::now();
                    }
                    if chrono::Utc::now().signed_duration_since(self.started_at)
                        > max_duration
                    {
                        self.mark_failure(format!(
                            "录制超过 {} 小时安全上限，已停止",
                            live_limits.max_duration_hours
                        ))
                        .await;
                        let _ = self.finalize(None).await;
                        return;
                    }
                }
            }
        }
    }

    /// 返回 true 表示录制应当收尾；正常运行和恢复中的情况均返回 false。
    async fn observe_process(&mut self) -> bool {
        let process_result = self.current_ffmpeg.as_mut().map(FfmpegSession::try_wait);
        let Some(process_result) = process_result else {
            return if self.unexpected_exit_detail.is_some() {
                self.recover_after_unexpected_exit().await
            } else {
                true
            };
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
                if self.stop_requested {
                    return true;
                }
                let room_is_offline = match self.confirm_room_offline().await {
                    Ok(offline) => Some(offline),
                    Err(error) => {
                        self.unexpected_exit_detail =
                            Some(format!("{detail}; 无法确认直播间是否已下播: {error}"));
                        None
                    }
                };
                match unexpected_exit_action(room_is_offline, self.restart_attempts) {
                    UnexpectedExitAction::CompleteAfterOfflineConfirmation => {
                        self.stop_reason = Some(STOP_REASON_OFFLINE_END);
                        info!(
                            room_id = self.room_id,
                            "FFmpeg 退出且直播间已下播，按正常完成收尾"
                        );
                        true
                    }
                    UnexpectedExitAction::Recover => {
                        if self.unexpected_exit_detail.is_none() {
                            self.unexpected_exit_detail = Some(detail);
                        }
                        self.recover_after_unexpected_exit().await
                    }
                    UnexpectedExitAction::FailRecoverable => {
                        if self.unexpected_exit_detail.is_none() {
                            self.unexpected_exit_detail = Some(detail);
                        }
                        self.stop_reason = Some(STOP_REASON_UNRECOVERABLE_EXIT);
                        self.is_recoverable = true;
                        let detail = self.unexpected_exit_detail.take().expect("exit detail set");
                        self.mark_failure(detail).await;
                        true
                    }
                }
            }
            Err(error) => {
                self.mark_failure(format!("检查 FFmpeg 进程状态失败: {error}"))
                    .await;
                true
            }
        }
    }

    async fn confirm_room_offline(&self) -> Result<bool> {
        let cookies = self.settings_service.cookie_header().await?;
        let init = self
            .bili_api
            .live_room_init(self.room_id, &cookies)
            .await
            .context("FFmpeg 退出后查询直播间状态失败")?;
        Ok(!init.is_live())
    }

    async fn recover_after_unexpected_exit(&mut self) -> bool {
        if self.restart_attempts < MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS {
            self.restart_attempts += 1;
            warn!(
                room_id = self.room_id,
                attempt = self.restart_attempts,
                "FFmpeg 异常退出，尝试刷新直播流并继续新分段"
            );
            match self.refresh_segment().await {
                Ok(()) => {
                    self.unexpected_exit_detail = None;
                    return false;
                }
                Err(error) => warn!(
                    room_id = self.room_id,
                    attempt = self.restart_attempts,
                    "启动恢复分段失败: {error}"
                ),
            }
        }

        self.stop_reason = Some(STOP_REASON_UNRECOVERABLE_EXIT);
        self.is_recoverable = true;
        let detail = self
            .unexpected_exit_detail
            .take()
            .unwrap_or_else(|| "FFmpeg 异常退出，且无法恢复录制".to_owned());
        self.mark_failure(detail).await;
        true
    }

    async fn refresh_segment(&mut self) -> Result<()> {
        let cookies = self.settings_service.cookie_header().await?;
        let max_qn = source_max_qn(&self.db, self.room_id).await;
        let playurl = self
            .bili_api
            .live_playurl(self.room_id, max_qn, &cookies)
            .await
            .context("刷新直播流地址失败")?;
        let candidates = select_stream_candidates(&playurl.durl)?;
        // 分段切换时优先选择与当前录制相同的容器/编码组合：concat 合并无法
        // 混合不同格式的分段。仅在同格式线路都不可用时才跨格式降级。
        let (current_format, current_codec) = {
            let snapshot = self.snapshot.lock().await;
            (
                snapshot.stream_format.clone(),
                snapshot.stream_codec.clone(),
            )
        };
        let same_profile = |stream: &LiveStreamUrl| {
            current_format
                .as_deref()
                .is_none_or(|format| stream.format_name.eq_ignore_ascii_case(format))
                && current_codec
                    .as_deref()
                    .is_none_or(|codec| stream.codec_name.eq_ignore_ascii_case(codec))
        };
        let selected_index = candidates
            .iter()
            .enumerate()
            .find(|(_, stream)| stream.url != self.current_url && same_profile(stream))
            .map(|(index, _)| index)
            .or_else(|| {
                candidates
                    .iter()
                    .enumerate()
                    .find(|(_, stream)| stream.url != self.current_url)
                    .map(|(index, _)| index)
            })
            .unwrap_or_else(|| {
                if candidates.is_empty() {
                    0
                } else {
                    (self.candidate_index + 1) % candidates.len()
                }
            });
        let selected_stream = candidates
            .get(selected_index)
            .cloned()
            .ok_or_else(|| anyhow!("流地址列表为空"))?;
        self.stream_candidates = candidates;
        self.candidate_index = selected_index;
        let new_url = selected_stream.url.clone();
        let new_path = segment_path(&self.live_dir, &self.base_prefix, self.next_segment);

        // 先结束旧分段再创建新进程，避免刷新 URL 时产生重叠录制窗口。
        if let Some(mut old_ffmpeg) = self.current_ffmpeg.take() {
            if let Err(error) = old_ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                warn!(
                    room_id = self.room_id,
                    "停止旧直播分段失败，已强制回收: {error}"
                );
            }
        }
        let new_ffmpeg = FfmpegSession::start(
            &self.ffmpeg_path,
            &new_url,
            new_path.clone(),
            self.room_id,
            user_agent(),
            referer(),
        )?;

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
        let segment_number = self.next_segment;
        self.segments.push(new_path.clone());
        if let Err(error) = persist_segment(
            &self.db,
            self.recording_id,
            segment_number,
            &new_path,
            "open",
        )
        .await
        {
            warn!(
                room_id = self.room_id,
                "persist refreshed recording segment failed: {error}"
            );
        }
        self.segment_index.fetch_add(1, Ordering::Relaxed);
        self.next_segment = self.next_segment.saturating_add(1);
        self.last_url_refresh = Instant::now();
        info!(
            room_id = self.room_id,
            segment = self.next_segment - 1,
            "直播流已切换到新分段"
        );
        Ok(())
    }

    async fn finalize(
        &mut self,
        merge_cancellation: Option<&CancellationToken>,
    ) -> Result<RecordingInfo> {
        if self.stop_requested && self.stop_reason.is_none() {
            self.stop_reason = Some(STOP_REASON_MANUAL);
        }
        if self.failure.is_none() {
            self.set_status(RecordingStatus::Stopping).await;
        }

        if let Some(mut ffmpeg) = self.current_ffmpeg.take() {
            if let Err(error) = ffmpeg.stop_with_timeout(STOP_TIMEOUT).await {
                self.mark_failure(format!("停止 FFmpeg 失败: {error}"))
                    .await;
            }
        }
        self.collector_cancel.cancel();

        if let Some(mut handle) = self.danmu_collector_handle.take() {
            match tokio::time::timeout(DANMU_STOP_TIMEOUT, &mut handle).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.mark_danmu_unavailable(format!("弹幕采集任务异常退出: {error}"))
                        .await
                }
                Err(_) => {
                    handle.abort();
                    self.mark_danmu_unavailable("等待弹幕采集任务超时".to_string())
                        .await
                }
            }
        }
        if let Some(mut handle) = self.danmu_write_handle.take() {
            match tokio::time::timeout(DANMU_STOP_TIMEOUT, &mut handle).await {
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
                    handle.abort();
                    self.mark_danmu_unavailable("等待弹幕写入任务超时".to_string())
                        .await
                }
            }
        }

        self.set_status(RecordingStatus::Finalizing).await;
        persist_closed_segments(&self.db, self.recording_id, &self.segments).await;
        let merge_result = match merge_cancellation {
            Some(cancellation) => {
                merge_segments_to_mp4_cancelable(&self.ffmpeg_path, &self.segments, cancellation)
                    .await
            }
            None => merge_segments_to_mp4(&self.ffmpeg_path, &self.segments).await,
        };
        let (final_path, merge_error) = match merge_result {
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
            if self.stop_reason.is_none() {
                self.stop_reason = Some(STOP_REASON_COMPLETED);
            }
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
        if self.stop_reason.is_none() {
            self.stop_reason = Some(STOP_REASON_FAILED);
        }
        if self.failure.is_none() {
            warn!(room_id = self.room_id, "直播录制进入失败状态: {error}");
            self.failure = Some(error);
        }
        self.collector_cancel.cancel();
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
        self.collector_cancel.cancel();
        let snapshot = {
            let mut snapshot = self.snapshot.lock().await;
            snapshot.danmu_unavailable = true;
            snapshot.interaction_capture_status = "unavailable".to_owned();
            snapshot.interaction_error = Some(error);
            snapshot.clone()
        };
        self.persist_recording(&snapshot, false).await;
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
            stop_reason: Set(self.stop_reason.map(str::to_owned)),
            segment_index: Set(self.segment_index.load(Ordering::Relaxed) as i32),
            restart_attempts: Set(self.restart_attempts as i32),
            checkpointed_at: Set(Some(chrono::Utc::now().to_rfc3339())),
            is_recoverable: Set(self.is_recoverable),
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

/// 读取每个直播源的清晰度上限；未配置或非法时返回 None，交给 B 站默认原画。
async fn source_max_qn(db: &DatabaseConnection, room_id: i64) -> Option<i32> {
    live_source::Entity::find()
        .filter(live_source::Column::RoomId.eq(room_id))
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|source| source.max_qn)
        .filter(|qn| *qn > 0)
}

/// 渲染录制文件名模板，支持 {room_id} {title} {date} {time} 占位符；
/// 结果未经清洗，调用方需自行 sanitize。
fn render_file_template(
    template: &str,
    room_id: i64,
    title: &str,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    let rendered = template
        .replace("{room_id}", &room_id.to_string())
        .replace("{title}", title)
        .replace("{date}", &now.format("%Y%m%d").to_string())
        .replace("{time}", &now.format("%H%M%S").to_string());
    if rendered.trim().is_empty() {
        format!("live_{room_id}")
    } else {
        rendered
    }
}

fn ensure_startup_active(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(anyhow!("直播录制启动已取消"))
    } else {
        Ok(())
    }
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

async fn mark_startup_recording(
    db: &DatabaseConnection,
    recording_id: i32,
    status: RecordingStatus,
    error: String,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let update = live_recording::ActiveModel {
        id: Set(recording_id),
        status: Set(status.to_string()),
        ended_at: Set(Some(now.clone())),
        error_msg: Set(Some(redact_diagnostics(&error))),
        updated_at: Set(now),
        ..Default::default()
    };
    if let Err(update_error) = update.update(db).await {
        warn!(
            recording_id,
            "failed to persist recording startup failure: {update_error}"
        );
    }
}

async fn persist_segment(
    db: &DatabaseConnection,
    recording_id: i32,
    segment_index: u32,
    path: &Path,
    status: &str,
) -> Result<()> {
    let file_size = tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.len() as i64)
        .unwrap_or(0);
    let now = chrono::Utc::now().to_rfc3339();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO live_recording_segments
             (recording_id, segment_index, path, file_size, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(recording_id, segment_index) DO UPDATE SET
             path=excluded.path, file_size=excluded.file_size,
             status=excluded.status, updated_at=excluded.updated_at"
            .to_owned(),
        [
            recording_id.into(),
            (segment_index as i32).into(),
            path.to_string_lossy().to_string().into(),
            file_size.into(),
            status.to_owned().into(),
            now.clone().into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn persist_closed_segments(db: &DatabaseConnection, recording_id: i32, paths: &[PathBuf]) {
    let ended_at = chrono::Utc::now().to_rfc3339();
    for (index, path) in paths.iter().enumerate() {
        let file_size = tokio::fs::metadata(path)
            .await
            .map(|metadata| metadata.len() as i64)
            .unwrap_or(0);
        let result = db
            .execute_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "UPDATE live_recording_segments
                 SET file_size=?, status='closed', ended_at=?, updated_at=?
                 WHERE recording_id=? AND segment_index=?"
                    .to_owned(),
                [
                    file_size.into(),
                    ended_at.clone().into(),
                    ended_at.clone().into(),
                    recording_id.into(),
                    (index as i32).into(),
                ],
            ))
            .await;
        if let Err(error) = result {
            warn!(
                recording_id,
                segment_index = index,
                "persist closed recording segment failed: {error}"
            );
        }
    }
}

async fn persist_merge_job(db: &DatabaseConnection, job: &MergeJobInfo) -> Result<()> {
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO live_merge_jobs
             (id, recording_id, status, progress, error_msg, source_segment_count,
              cancel_requested, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             status=excluded.status, progress=excluded.progress,
             error_msg=excluded.error_msg, source_segment_count=excluded.source_segment_count,
             cancel_requested=excluded.cancel_requested, updated_at=excluded.updated_at"
            .to_owned(),
        [
            job.id.clone().into(),
            job.recording_id.into(),
            job.status.clone().into(),
            (job.progress as i32).into(),
            job.error.clone().into(),
            (job.source_segment_count as i32).into(),
            (if job.cancel_requested { 1_i32 } else { 0_i32 }).into(),
            job.created_at.clone().into(),
            job.updated_at.clone().into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn prune_persisted_merge_jobs(db: &DatabaseConnection) -> Result<()> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "DELETE FROM live_merge_jobs
          WHERE status IN ('completed', 'failed', 'cancelled') AND updated_at < ?"
            .to_owned(),
        [cutoff.into()],
    ))
    .await?;
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        "DELETE FROM live_merge_jobs
          WHERE id IN (
              SELECT id FROM live_merge_jobs
               WHERE status IN ('completed', 'failed', 'cancelled')
               ORDER BY updated_at DESC LIMIT -1 OFFSET 200
          )"
        .to_owned(),
    ))
    .await?;
    Ok(())
}

async fn run_merge_job(
    inner: Arc<LiveRecorderInner>,
    job_id: String,
    recording_id: i32,
    ffmpeg_path: PathBuf,
    segments: Vec<PathBuf>,
    cancellation: CancellationToken,
) {
    update_merge_job(&inner, &job_id, "running", 10, None).await;
    let result = merge_segments_to_mp4_cancelable(&ffmpeg_path, &segments, &cancellation).await;
    match result {
        Ok(output) => {
            persist_closed_segments(&inner.db, recording_id, &segments).await;
            for segment in &segments {
                let _ = tokio::fs::remove_file(segment).await;
            }
            let update = live_recording::ActiveModel {
                id: Set(recording_id),
                status: Set("stopped".to_owned()),
                output_path: Set(Some(output.to_string_lossy().to_string())),
                error_msg: Set(None),
                is_recoverable: Set(false),
                updated_at: Set(chrono::Utc::now().to_rfc3339()),
                ..Default::default()
            };
            if let Err(error) = update.update(&inner.db).await {
                update_merge_job(&inner, &job_id, "failed", 90, Some(error.to_string())).await;
            } else {
                update_merge_job(&inner, &job_id, "completed", 100, None).await;
            }
        }
        Err(error) => {
            let status = if cancellation.is_cancelled() {
                "cancelled"
            } else {
                "failed"
            };
            update_merge_job(&inner, &job_id, status, 100, Some(error.to_string())).await;
        }
    }
    inner.merge_cancellations.lock().await.remove(&job_id);
}

async fn update_merge_job(
    inner: &Arc<LiveRecorderInner>,
    job_id: &str,
    status: &str,
    progress: u8,
    error: Option<String>,
) {
    let snapshot = if let Some(job) = inner.merge_jobs.lock().await.get_mut(job_id) {
        job.status = status.to_owned();
        job.progress = progress;
        job.error = error.map(|error| redact_diagnostics(&error));
        job.updated_at = chrono::Utc::now().to_rfc3339();
        Some(job.clone())
    } else {
        None
    };
    if let Some(job) = snapshot {
        if let Err(error) = persist_merge_job(&inner.db, &job).await {
            warn!(job_id, "persist merge job failed: {error}");
        }
    }
}

fn merge_job_from_row(row: &QueryResult) -> Result<MergeJobInfo> {
    Ok(MergeJobInfo {
        id: row.try_get("", "id")?,
        recording_id: row.try_get("", "recording_id")?,
        status: row.try_get("", "status")?,
        progress: row.try_get::<i32>("", "progress")?.clamp(0, 100) as u8,
        error: row.try_get("", "error_msg")?,
        source_segment_count: row.try_get::<i32>("", "source_segment_count")?.max(0) as usize,
        cancel_requested: row.try_get::<i32>("", "cancel_requested")? != 0,
        created_at: row.try_get("", "created_at")?,
        updated_at: row.try_get("", "updated_at")?,
    })
}

async fn find_recording_segments(directory: &Path, output_path: Option<&str>) -> Vec<PathBuf> {
    let Some(output_path) = output_path else {
        return Vec::new();
    };
    let Some(file_name) = Path::new(output_path)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return Vec::new();
    };
    let Some(prefix) = file_name.split("_segment_").next() else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return result;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with(&format!("{prefix}_segment_")) && name.ends_with(".flv")
            });
        if matches {
            result.push(path);
        }
    }
    result.sort();
    result
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn unexpected_exit_state_machine_requires_offline_confirmation_for_completion() {
        assert_eq!(
            unexpected_exit_action(Some(true), 0),
            UnexpectedExitAction::CompleteAfterOfflineConfirmation
        );
        assert_eq!(
            unexpected_exit_action(Some(false), 0),
            UnexpectedExitAction::Recover
        );
        assert_eq!(
            unexpected_exit_action(None, 0),
            UnexpectedExitAction::Recover
        );
    }

    #[test]
    fn unexpected_exit_state_machine_marks_exhausted_live_or_unknown_exit_recoverable() {
        assert_eq!(
            unexpected_exit_action(Some(false), MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS),
            UnexpectedExitAction::FailRecoverable
        );
        assert_eq!(
            unexpected_exit_action(None, MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS),
            UnexpectedExitAction::FailRecoverable
        );
    }

    #[test]
    fn recording_stop_reasons_are_stable_database_values() {
        assert_eq!(STOP_REASON_MANUAL, "manual_stop");
        assert_eq!(
            STOP_REASON_OFFLINE_END,
            "stream_ended_after_offline_confirmation"
        );
        assert_eq!(
            STOP_REASON_UNRECOVERABLE_EXIT,
            "ffmpeg_exit_while_live_or_unconfirmed"
        );
    }

    #[test]
    fn file_template_renders_known_tokens() {
        let now = chrono::Local
            .with_ymd_and_hms(2026, 8, 10, 21, 14, 5)
            .unwrap();
        let rendered = render_file_template("{room_id}_{title}_{date}", 8178490, "深夜点歌台", now);
        assert_eq!(rendered, "8178490_深夜点歌台_20260810");
        let with_time = render_file_template("{room_id}_{time}", 732, "标题", now);
        assert_eq!(with_time, "732_211405");
    }

    #[test]
    fn file_template_falls_back_for_empty_result() {
        let now = chrono::Local::now();
        assert_eq!(render_file_template("   ", 123, "标题", now), "live_123");
        assert_eq!(
            render_file_template("{missing}", 456, "标题", now),
            "{missing}"
        );
    }
}
