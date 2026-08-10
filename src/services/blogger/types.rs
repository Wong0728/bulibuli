/// 新增博主参数（DB 写入部分；资料抓取由调用方完成）。
pub struct NewBlogger {
    pub uid: String,
    pub name: Option<String>,
    pub min_interval: i32,
    pub max_interval: i32,
    pub face: Option<String>,
    pub sign: Option<String>,
    pub level: Option<i32>,
    pub fans: Option<i64>,
    pub download_video: bool,
    pub download_danmaku: bool,
    pub download_comments: bool,
    pub download_cover: bool,
    pub burn_danmaku: bool,
    pub burn_subtitle: bool,
    pub series_filter_regex: Option<String>,
    pub active_windows: Option<String>,
    pub monitor_enabled: bool,
    pub is_saved: bool,
    pub has_auto_task: bool,
}

/// 博主配置更新参数（None 表示不修改；输入校验由 API 层完成）。
#[derive(Default)]
pub struct BloggerUpdate {
    pub uid: Option<String>,
    pub name: Option<String>,
    pub min_interval: Option<i32>,
    pub max_interval: Option<i32>,
    pub download_video: Option<bool>,
    pub download_danmaku: Option<bool>,
    pub download_comments: Option<bool>,
    pub download_cover: Option<bool>,
    pub burn_danmaku: Option<bool>,
    pub burn_subtitle: Option<bool>,
    pub series_filter_regex: Option<String>,
    /// 外层 None=不修改，内层 None=清空（恢复全天检查）。
    pub active_windows: Option<Option<String>>,
    pub monitor_enabled: Option<bool>,
    pub is_saved: Option<bool>,
    pub has_auto_task: Option<bool>,
}

pub enum MonitorToggle {
    NotFound,
    AlreadyInState,
    Updated,
}
