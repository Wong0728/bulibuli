//! 直播录制服务：管理并发录制、FFmpeg 监督和流地址分段轮换。
//!
//! 模块布局（按职责拆分，`impl LiveRecorder` / `impl RecordingWorker` 分散在
//! 多个兄弟文件中是本 crate 的惯例）：
//! - `lifecycle` / `session_start`：启动入口守门、停止/后台停止/关停，以及单场
//!   录制会话的装配（弹幕、数据库记录、FFmpeg、worker）。
//! - `queries` / `merge_job`：状态与历史查询、合并任务管理。
//! - `worker` / `supervision` / `finalize`：录制 worker 的事件循环、进程监督与
//!   流地址轮换、录制收尾。
//! - `danmu`：弹幕采集与互动归档 writer 的启动装配与生命周期监督。
//! - `merge_jobs` / `segments` / `startup`：合并任务持久化、分段文件辅助、
//!   启动守门辅助。
//! - `ffmpeg_session` / `ffmpeg_merge` / `ffmpeg_probe`：FFmpeg 录制进程、分段
//!   合并与媒体探测。
//! - `interactions` / `interaction_archive`：互动事件实时写入与收尾归档。
//! - `state`：录制状态、触发方式与异常退出状态机。

mod danmu;
mod ffmpeg_merge;
mod ffmpeg_probe;
pub mod ffmpeg_session;
mod finalize;
mod interaction_archive;
mod interactions;
mod lifecycle;
mod merge_job;
mod merge_jobs;
mod queries;
mod segments;
mod session_start;
mod startup;
mod state;
pub mod stream_url;
mod supervision;
mod worker;

#[cfg(test)]
mod tests;

use crate::config::AppPaths;
use crate::services::bili_api::models::live::LiveStreamUrl;
use crate::services::bili_api::BiliApi;
use crate::services::settings::SettingsService;
use crate::services::video_processor::VideoProcessor;
use anyhow::Result;
use danmu::DanmuCollectorEvent;
use ffmpeg_session::FfmpegSession;
pub use interactions::ArchivedLiveEvent;
use sea_orm::DatabaseConnection;
use serde::Serialize;
pub use state::{RecordingStatus, RecordingTrigger};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// 收尾任务（分段合并、停止收尾、录制 worker）的关机预算：普通后台任务只有 10 秒，
/// 会砍断正在写盘的合并；这里给足完成时间，超时后也会因已登记合并任务而下次启动续跑。
const FINALIZE_SHUTDOWN_BUDGET: Duration = Duration::from_secs(120);
// 互动归档收尾是两遍全量扫描（上限 50 万条 / 512 MB），数小时直播轻松超过旧值 5 秒；
// 超时 abort 会在写出中途截断 legacy JSON / XML，因此按最坏工作量放宽预算。
const DANMU_STOP_TIMEOUT: Duration = Duration::from_secs(120);
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const URL_REFRESH_MARGIN_SECS: i64 = 60;
const CONSERVATIVE_URL_REFRESH: Duration = Duration::from_secs(15 * 60);
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_RECORDING_FILE_SIZE: u64 = 200 * 1024 * 1024 * 1024;
const MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS: u32 = 3;
/// 恢复成功且稳定录制超过该时长后，重置恢复尝试额度。
const RECOVERY_ATTEMPTS_RESET_AFTER: Duration = Duration::from_secs(5 * 60);
const STOP_REASON_MANUAL: &str = "manual_stop";
const STOP_REASON_OFFLINE_END: &str = "stream_ended_after_offline_confirmation";
const STOP_REASON_UNRECOVERABLE_EXIT: &str = "ffmpeg_exit_while_live_or_unconfirmed";
const STOP_REASON_FAILED: &str = "recording_failed";
const STOP_REASON_COMPLETED: &str = "recording_completed";
// 正常启动的网络调用上限约 60s（bili_api 超时），两倍余量后仍卡在 Starting 的
// 只能是遗留条目（如启动任务被异常中止且清理代码未执行到）。
const STARTING_STALE_TIMEOUT: Duration = Duration::from_secs(120);

/// 对外暴露的录制信息。
#[derive(Clone, Debug, Serialize)]
pub struct RecordingInfo {
    pub room_id: i64,
    pub recording_id: Option<i32>,
    pub title: String,
    pub status: RecordingStatus,
    /// 内部文件系统路径；出于安全考虑不序列化到 API 响应。
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

type MergeJobMap = Arc<Mutex<HashMap<String, MergeJobInfo>>>;
type MergeCancellationMap = Arc<Mutex<HashMap<String, CancellationToken>>>;

/// 录制服务依赖。
pub struct LiveRecorderDeps {
    pub bili_api: Arc<BiliApi>,
    pub video_processor: Arc<VideoProcessor>,
    pub paths: Arc<AppPaths>,
    pub settings_service: Arc<SettingsService>,
    pub db: DatabaseConnection,
    pub background_tasks: Arc<crate::services::spawn_util::TaskRegistry>,
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
    background_tasks: Arc<crate::services::spawn_util::TaskRegistry>,
}

enum SessionEntry {
    Starting {
        snapshot: Arc<Mutex<RecordingInfo>>,
        cancellation: CancellationToken,
        /// 启动代际：stop()/start_with_options 跨 await 操作 sessions 时，
        /// 只允许移除自己创建的那一代 Starting，防止误删用户随后发起的新会话。
        generation: u64,
        /// 启动开始时刻：超过 STARTING_STALE_TIMEOUT 仍未完成的视为遗留条目。
        created_at: Instant,
    },
    Active(RecordingSessionHandle),
    Stopping {
        snapshot: Arc<Mutex<RecordingInfo>>,
        job_id: String,
    },
}

/// 全局启动代际计数器（单调递增）。
static START_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 并发额度只统计真正占用录制资源的会话：Starting/Active 计入，
/// Stopping（后台合并收尾中）不再占额度，避免用户收到“已达上限”但实际没有在录。
fn active_session_count(sessions: &HashMap<i64, SessionEntry>) -> usize {
    sessions
        .values()
        .filter(|entry| !matches!(entry, SessionEntry::Stopping { .. }))
        .count()
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
                background_tasks: deps.background_tasks,
            }),
        }
    }
}

/// 单场录制的运行时状态：worker 独占，字段仅由本目录内的实现模块访问。
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
    /// 下一次异常退出恢复的最早时刻：连续秒退时按退避拉开重试间隔，
    /// 避免恢复额度在数秒内被烧完。
    next_recovery_at: Option<Instant>,
    stop_reason: Option<&'static str>,
    is_recoverable: bool,
    unexpected_exit_detail: Option<String>,
    sessions: Arc<Mutex<HashMap<i64, SessionEntry>>>,
    merge_jobs: Arc<Mutex<HashMap<String, MergeJobInfo>>>,
    merge_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    background_tasks: Arc<crate::services::spawn_util::TaskRegistry>,
}
