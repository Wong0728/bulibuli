use chrono::{DateTime, Local};
use sea_orm::entity::prelude::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "bloggers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub uid: String,
    pub name: Option<String>,
    pub min_interval: i32,
    pub max_interval: i32,
    pub is_running: bool,
    pub next_check: Option<DateTime<Local>>,
    pub created_at: Option<DateTime<Local>>,
    pub updated_at: Option<DateTime<Local>>,
    /// 博主头像 B 站 URL
    #[sea_orm(column_type = "Text", nullable)]
    pub face: Option<String>,
    /// 博主签名
    #[sea_orm(column_type = "Text", nullable)]
    pub sign: Option<String>,
    /// 博主等级
    #[sea_orm(nullable)]
    pub level: Option<i32>,
    /// 博主粉丝数（添加/监控刷新时抓取；NULL=未抓取）
    #[sea_orm(nullable)]
    pub fans: Option<i64>,
    /// 上次"已知"昵称（用于检测改名）
    #[sea_orm(nullable)]
    pub last_seen_name: Option<String>,
    /// 上次"已知"头像 URL（用于检测改头像）
    #[sea_orm(column_type = "Text", nullable)]
    pub last_seen_face: Option<String>,
    /// 上次检测到改名/改头像的时间
    pub last_seen_at: Option<DateTime<Local>>,
    /// 是否下载视频
    #[sea_orm(default_value = true)]
    pub download_video: Option<bool>,
    /// 是否下载弹幕
    #[sea_orm(default_value = true)]
    pub download_danmaku: Option<bool>,
    /// 是否下载评论
    #[sea_orm(default_value = true)]
    pub download_comments: Option<bool>,
    /// 是否下载封面
    #[sea_orm(default_value = true)]
    pub download_cover: Option<bool>,
    /// 是否自动烧录弹幕到视频
    #[sea_orm(default_value = false)]
    pub burn_danmaku: Option<bool>,
    /// 是否自动烧录 CC 字幕到视频
    #[sea_orm(default_value = false)]
    pub burn_subtitle: Option<bool>,
    /// 合集白名单正则（空=全部）
    #[sea_orm(nullable)]
    pub series_filter_regex: Option<String>,
    /// 活跃检查时段 JSON 数组（如 ["12:00-14:00","18:00-23:00"]；NULL/空=全天）
    #[sea_orm(column_type = "Text", nullable)]
    pub active_windows: Option<String>,
    /// 是否保存在“博主搜索”的已添加列表中。
    #[sea_orm(default_value = true)]
    pub is_saved: bool,
    /// 是否拥有“自动任务”配置；与 is_saved 相互独立。
    #[sea_orm(default_value = true)]
    pub has_auto_task: bool,
    /// 乐观锁版本号：每次博主配置变更 +1。调用方在写操作时传 `expected_version` 校验，
    /// 不匹配则返回 CONFLICT（详见 `services/conflict_guard.rs`）。
    #[sea_orm(default_value = 0)]
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub fn to_api(&self) -> serde_json::Value {
        // notice_visible：last_seen_at 非空时表示博主改名/改头像未确认
        let notice_visible = self.last_seen_at.is_some();
        serde_json::json!({
            "id": self.id,
            "uid": self.uid,
            "name": self.name,
            "min_interval": self.min_interval,
            "max_interval": self.max_interval,
            "is_running": self.is_running,
            "next_check": self.next_check.map(|t| t.timestamp()).unwrap_or(0),
            "created_at": self.created_at.map(|t| t.to_rfc3339()),
            "updated_at": self.updated_at.map(|t| t.to_rfc3339()),
            "face": self.face,
            "sign": self.sign,
            "level": self.level,
            "fans": self.fans.unwrap_or(0),
            "last_seen_name": self.last_seen_name,
            "last_seen_face": self.last_seen_face,
            "last_seen_at": self.last_seen_at.map(|t| t.to_rfc3339()),
            "notice_visible": notice_visible,
            "download_video": self.download_video.unwrap_or(true),
            "download_danmaku": self.download_danmaku.unwrap_or(true),
            "download_comments": self.download_comments.unwrap_or(true),
            "download_cover": self.download_cover.unwrap_or(true),
            "burn_danmaku": self.burn_danmaku.unwrap_or(false),
            "burn_subtitle": self.burn_subtitle.unwrap_or(false),
            "series_filter_regex": self.series_filter_regex.as_deref().unwrap_or(""),
            "active_windows": self.active_windows_list(),
            "is_saved": self.is_saved,
            "has_auto_task": self.has_auto_task,
        })
    }

    /// 解析 active_windows JSON 为字符串数组；解析失败或未配置时返回空数组。
    pub fn active_windows_list(&self) -> Vec<String> {
        self.active_windows
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default()
    }
}
