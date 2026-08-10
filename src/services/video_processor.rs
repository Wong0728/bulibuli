//! FFmpeg 视频处理服务：类型与结构定义在本文件，
//! ffmpeg 探测见 `ffmpeg_detect`，音视频合并见 `merge`，纯视频 remux 与清理见 `remux`。

mod ffmpeg_detect;
mod merge;
mod remux;

use crate::config::AppPaths;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub type MergeCallback = Box<dyn FnOnce(MergeResult) + Send + Sync>;
pub type ProgressCallback = Box<dyn Fn(MergeProgress) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct MergeResult {
    pub success: bool,
    pub task_id: String,
    pub output_path: Option<PathBuf>,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct MergeProgress {
    pub task_id: String,
    pub status: String,
    pub progress_percent: i32,
    pub current_time: f64,
    pub duration: f64,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct MergeTaskInfo {
    pub task_id: String,
    pub video_path: PathBuf,
    pub audio_path: PathBuf,
    pub output_path: PathBuf,
    pub status: String,
    pub progress_percent: i32,
    pub current_time: f64,
    pub duration: f64,
    pub start_time: Option<chrono::DateTime<chrono::Local>>,
    pub message: String,
}

#[derive(Clone)]
pub struct VideoProcessor {
    paths: Arc<AppPaths>,
    custom_ffmpeg_path: Option<String>,
    tasks: Arc<Mutex<HashMap<String, MergeTaskInfo>>>,
}

impl VideoProcessor {
    pub fn new(paths: Arc<AppPaths>) -> Self {
        Self {
            paths,
            custom_ffmpeg_path: None,
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
