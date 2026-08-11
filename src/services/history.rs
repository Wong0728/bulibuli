//! 历史记录服务：数据访问（`records`）与本地文件扫描（`file_scan`）。

mod file_scan;
mod records;

use crate::config::AppPaths;
use sea_orm::DatabaseConnection;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BOARD_CACHE_TTL: Duration = Duration::from_secs(2);
type BoardCacheKey = (String, u64, u64);
type BoardCacheEntry = (BoardPage, Instant);

/// 历史记录数据访问与侧车文件聚合服务。
///
/// 提供视频/弹幕/字幕文件的存在性检测，供看板卡片"✓/—"图标使用；
/// 同时承接 API 层的历史记录读写（API 层禁止直接操作数据库）。
pub struct HistoryService {
    db: DatabaseConnection,
    paths: Arc<AppPaths>,
    board_cache: tokio::sync::RwLock<HashMap<BoardCacheKey, BoardCacheEntry>>,
}

/// 侧车文件存在性结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SidecarStatus {
    /// 视频文件是否存在（history.file_path 指向的文件）
    pub video: bool,
    /// 弹幕文件是否存在（downloads/{uid}/danmaku/{bvid}.xml 或 .json）
    pub danmaku: bool,
    /// 评论文件是否存在（与当前视频同目录的 `{bvid}_comments.html`）
    pub comments: bool,
    /// 字幕文件是否存在（downloads/{uid}/subtitle/{bvid}.srt 或 .ass）
    pub subtitle: bool,
}

/// 已下载文件条目。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileEntry {
    pub file_type: String,
    pub name: String,
    pub path: String,
    pub size: i64,
    pub format: Option<String>,
    /// `manual`、`auto:<uid>` 或 `other`，供前端区分重复产物所在区域。
    pub location: String,
    /// 是否位于 history.file_path 指向的当前产物目录。
    pub is_current: bool,
    /// 时间戳归档版本；固定名最新文件为 None。
    pub version: Option<String>,
    /// 文件最后修改时间（Unix 秒）。
    pub modified_at: Option<i64>,
}

/// 看板数据库分页结果。任务和博主信息由调用方按本页键集合继续批量查询。
#[derive(Clone)]
pub struct BoardPage {
    pub histories: Vec<crate::models::history::Model>,
    pub total: u64,
    pub counts_by_uid: HashMap<String, HistoryCounts>,
}

#[derive(Clone, Default)]
pub struct HistoryCounts {
    pub downloading: i64,
    pub completed: i64,
    pub failed: i64,
    pub removed: i64,
    pub pay_blocked: i64,
}

impl HistoryService {
    pub fn new(db: DatabaseConnection, paths: Arc<AppPaths>) -> Self {
        Self {
            db,
            paths,
            board_cache: tokio::sync::RwLock::new(HashMap::new()),
        }
    }
}
