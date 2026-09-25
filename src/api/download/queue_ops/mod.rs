//! 下载任务队列接口。
//!
//! 按职责拆分的子模块：
//! - `common`：批量版本守卫与响应信封等共享工具；
//! - `enqueue`：手动添加下载任务（`/api/download/add`）；
//! - `start`：一键启动解析下载（`/api/download/start`，普通视频/多P）；
//! - `season`：番剧/课程分集批量下载；
//! - `task_ops`：任务状态/优先级/暂停恢复/重试/移除等队列操作。

mod common;
mod enqueue;
mod season;
mod start;
mod task_ops;

use serde::Deserialize;

/// 分P/分集选择器：普通视频多P与番剧/课程分集共用。
#[derive(Deserialize)]
pub(crate) struct PageSelector {
    cid: i64,
    page: i32,
    #[serde(default)]
    part: String,
    /// 番剧/课程分集专用：ep_id（番剧）或 ep_id（课程，对应 B 站 id 字段）。
    /// 普通视频分 P 不需要传。
    #[serde(default)]
    ep_id: Option<u64>,
    /// 番剧/课程分集专用：该集对应的 bvid（番剧每集有独立 bvid）。
    /// 普通视频分 P 不需要传，沿用顶层 bvid。
    #[serde(default)]
    bvid: Option<String>,
    /// 课程分集专用：avid（课程 playurl 接口要求 avid 而非 bvid）。
    #[serde(default)]
    aid: Option<i64>,
}

/// `POST /api/download/start` 请求体：普通视频与番剧/课程共用。
#[derive(Deserialize)]
pub(crate) struct StartDownloadRequest {
    bvid: String,
    qn: Option<i32>,
    uid: Option<String>,
    /// 多P视频时前端携带所选分P列表；为空时下默认 cid（单任务，保持现状）。
    #[serde(default)]
    pages: Vec<PageSelector>,
    /// 媒体类型：缺省/"video" 走普通视频取流；"pgc" 走番剧取流；"cheese" 走课程取流。
    /// 番剧/课程分集下载时，每个 PageSelector 携带 ep_id + bvid（+ aid 用于课程）。
    #[serde(default)]
    media_type: Option<String>,
    /// 番剧/课程下载时由前端透传的季标题（用作任务主标题，避免逐集再查一次）。
    #[serde(default)]
    season_title: Option<String>,
}

pub(crate) use enqueue::add_download;
pub(crate) use start::start_download;
pub(crate) use task_ops::{
    get_health, get_status, pause_download, queue_metrics, remove_download, resume_download,
    retry_all, retry_download, set_priority,
};
