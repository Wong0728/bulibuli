//! 下载管理模块：以 aria2 为传输引擎的下载任务全生命周期管理。
//!
//! 子模块职责：
//! - `manager`：构造、监控启停与断点续传恢复
//! - `monitor`：monitor_loop 轮询 aria2 状态并驱动任务状态机
//! - `completion`：单任务完成处理（防抖闸门、去重归位、落库与封面）
//! - `queue`：任务入队、重试与移除
//! - `backoff`：重试退避与任务缓存清理
//! - `dispatch`：向 aria2 投递下载任务
//! - `engine`：下载引擎枚举分发（aria2 优先，完全不可用时降级原生兜底）
//! - `native`：reqwest 原生流式下载兜底
//! - `audio_retry`：音频失败自动重试与纯视频降级
//! - `post_process`：下载完成后的音视频合并触发
//! - `history_sync`：历史记录写入与封面落盘
//! - `storage`：下载目录派生与 MD5 去重归位
//! - `status`：状态查询、队列摘要与进度广播

mod audio_retry;
mod backoff;
mod completion;
mod dispatch;
mod engine;
mod history_sync;
mod manager;
mod monitor;
mod native;
mod post_process;
mod queue;
mod status;
mod storage;
mod video_retry;

use crate::config::{AppConfig, AppPaths};
use crate::services::{
    aria2::Aria2Manager, bili_api::BiliApi, concurrency_gate::ConcurrencyGate,
    download_state::DownloadStateService, progress_writer::ProgressWriter,
    settings::SettingsService, video_processor::VideoProcessor,
};
use crate::ws::WebSocketManager;
use sea_orm::DatabaseConnection;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
struct ProgressCache {
    progress_percent: i32,
    downloaded_size: i64,
    total_size: i64,
    speed: i64,
}

/// MD5 去重结果
#[derive(Debug)]
struct DedupeResult {
    final_filename: String,
    message: String,
}

#[derive(Clone, Debug)]
struct RetryBackoff {
    attempts: u32,
    next_retry_at: Instant,
}

/// 任务入队、重试和移除的统一结果。
/// `ok=false` 表示业务拒绝（重复任务、退避中、非法参数等），而非系统错误。
#[derive(Clone, Debug)]
pub struct TaskOutcome {
    pub ok: bool,
    pub message: String,
    pub download_id: Option<i32>,
}

impl TaskOutcome {
    pub fn accepted(message: impl Into<String>, download_id: i32) -> Self {
        Self {
            ok: true,
            message: message.into(),
            download_id: Some(download_id),
        }
    }

    pub fn done(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            download_id: None,
        }
    }

    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            download_id: None,
        }
    }
}

#[derive(Clone)]
pub struct DownloadManager {
    config: Arc<AppConfig>,
    paths: Arc<AppPaths>,
    db: DatabaseConnection,
    aria2: Arc<Aria2Manager>,
    bili_api: Arc<BiliApi>,
    video_processor: Arc<VideoProcessor>,
    ws: Arc<WebSocketManager>,
    monitor_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
    cancellation: CancellationToken,
    settings_service: Arc<SettingsService>,
    progress_writer: ProgressWriter,
    state_service: DownloadStateService,
    concurrency_gate: ConcurrencyGate,
    progress_cache: Arc<Mutex<HashMap<String, ProgressCache>>>,
    retry_backoff: Arc<Mutex<HashMap<String, RetryBackoff>>>,
    /// 正在合并中的 bvid 集合，用于避免 monitor_loop 重复触发同一 bvid 合并任务
    /// 使用 std::sync::Mutex（短时持有），通过 poison-safe 辅助方法访问
    merge_in_progress: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// 原生兜底下载器，仅在 aria2 子系统不可用时启用。
    native: native::NativeDownloader,
    /// 运行中的原生下载任务：task_id → 取消令牌（monitor_loop 据此跳过、remove_task 据此取消）
    native_tasks: Arc<Mutex<HashMap<i32, CancellationToken>>>,
    /// 上次 aria2 重建失败时刻：60 秒冷却内不重复拉起子进程（防重建风暴）
    aria2_recover_failed_at: Arc<Mutex<Option<Instant>>>,
    /// 新任务入队通知：monitor_loop 空闲退避期间据此立即唤醒，避免空闲时频繁查库
    queue_notify: Arc<tokio::sync::Notify>,
    /// add_task 串行化锁：键为 `{bvid}#{cid}:{task_type}`。防止并发重复请求
    /// 同时穿过"无存量任务"检查、创建出两行任务或对同一产物双重派发。
    add_task_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// 最近完成的任务（键同上）→ 完成时刻。完成后的短窗口内重复入队请求
    /// 直接幂等返回"产物已存在"，避免整段重下只为 SHA-256 比对。
    recent_completions: Arc<Mutex<HashMap<String, Instant>>>,
}

pub struct DownloadManagerDependencies {
    pub config: Arc<AppConfig>,
    pub paths: Arc<AppPaths>,
    pub db: DatabaseConnection,
    pub aria2: Arc<Aria2Manager>,
    pub bili_api: Arc<BiliApi>,
    pub video_processor: Arc<VideoProcessor>,
    pub ws: Arc<WebSocketManager>,
    pub settings_service: Arc<SettingsService>,
    pub cancellation: CancellationToken,
}

impl DownloadManager {
    /// 获取 merge_in_progress 的锁，自动处理 poison。
    /// 即使另一线程 panic 污染了锁，也能继续提供服务而非 propagate panic。
    fn lock_merge_set(&self) -> std::sync::MutexGuard<'_, std::collections::HashSet<String>> {
        match self.merge_in_progress.lock() {
            Ok(guard) => guard,
            Err(error) => {
                let mut guard = error.into_inner();
                guard.clear();
                guard
            }
        }
    }
}

/// 分P下载信息：多P任务创建时携带，单P任务传 `None`。
/// cid/page/part_title 会写入 download_task 对应列，并驱动缓存键、文件命名与合并隔离。
#[derive(Clone, Debug)]
pub struct PageInfo {
    pub cid: i64,
    pub page: i32,
    pub part_title: String,
}

/// 任务级缓存键：单P（cid=None）返回 bvid 保持存量行为；多P返回 `{bvid}#{cid}`。
/// 用于 progress_cache / merge_in_progress 等按分P粒度隔离的内存缓存，
/// 避免同一 bvid 的不同分P互相覆盖进度或误判合并进行中。
fn task_cache_key(bvid: &str, cid: Option<i64>) -> String {
    match cid {
        Some(c) => format!("{bvid}#{c}"),
        None => bvid.to_string(),
    }
}

pub(super) fn backoff_key(bvid: &str, cid: Option<i64>, task_type: &str) -> String {
    format!("{}_{}", task_cache_key(bvid, cid), task_type)
}

/// 文件名词根：单P（page=None）返回 bvid（保持存量 `{bvid}.ext` 命名）；
/// 多P返回 `{bvid}_p{page}`。用于视频/音频临时文件命名与 MD5 去重扫描前缀，
/// 隔离同 bvid 不同分P的文件，避免跨分P误匹配。
fn file_stem_for(bvid: &str, page: Option<i32>) -> String {
    match page {
        Some(p) => format!("{bvid}_p{p}"),
        None => bvid.to_string(),
    }
}

/// 校验 bvid 格式是否合法。
/// B 站 bvid 格式为 BV + 10 位字符（字符集为 base58 去重后的字母数字）。
/// 此校验防止恶意 bvid（如 "../etc/passwd"）触发的路径穿越或文件名注入。
fn is_valid_bvid(bvid: &str) -> bool {
    // 长度必须为 12：'BV' + 10
    if bvid.len() != 12 {
        return false;
    }
    // 必须以 'BV' 开头（大小写敏感，与 B 站一致）。
    if !bvid.starts_with("BV") {
        return false;
    }
    // 后 10 位必须为 base58 字符集（去除易混淆的 0/O/I/l）
    let rest = &bvid[2..];
    rest.chars()
        .all(|c| c.is_ascii_alphanumeric() && c != '0' && c != 'O' && c != 'I' && c != 'l')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_keys_and_file_stems_keep_pages_isolated() {
        assert_eq!(task_cache_key("BV1xx411c7mD", None), "BV1xx411c7mD");
        assert_eq!(task_cache_key("BV1xx411c7mD", Some(99)), "BV1xx411c7mD#99");
        assert_eq!(
            backoff_key("BV1xx411c7mD", Some(99), "video"),
            "BV1xx411c7mD#99_video"
        );
        assert_eq!(file_stem_for("BV1xx411c7mD", None), "BV1xx411c7mD");
        assert_eq!(file_stem_for("BV1xx411c7mD", Some(2)), "BV1xx411c7mD_p2");
    }

    #[test]
    fn bvid_validation_rejects_path_like_and_ambiguous_values() {
        assert!(is_valid_bvid("BV1xx411c7mD"));
        assert!(!is_valid_bvid("../etc/passwd"));
        assert!(!is_valid_bvid("BV1xx411c7m0D"));
        assert!(!is_valid_bvid("bv1xx411c7mD"));
    }

    #[test]
    fn task_outcomes_preserve_acceptance_contract() {
        let accepted = TaskOutcome::accepted("queued", 7);
        assert!(accepted.ok);
        assert_eq!(accepted.download_id, Some(7));
        assert!(TaskOutcome::done("done").ok);
        assert!(!TaskOutcome::rejected("duplicate").ok);
    }
}
