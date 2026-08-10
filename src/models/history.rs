use chrono::{DateTime, Local};
use sea_orm::entity::prelude::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "history")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub uid: Option<String>,
    pub bvid: String,
    /// 记录来源：auto=自动监控，manual=手动下载。
    #[sea_orm(column_type = "Text", default_value = "auto")]
    pub source: String,
    pub title: Option<String>,
    pub pub_date: Option<String>,
    pub pub_timestamp: Option<i64>,
    pub download_time: Option<DateTime<Local>>,
    pub file_path: Option<String>,
    pub next_download_index: i32,
    /// 视频封面 B 站原始 URL（落库以便后续重新下载封面）
    #[sea_orm(column_type = "Text", nullable)]
    pub pic: Option<String>,
    /// 视频时长（秒）
    #[sea_orm(nullable)]
    pub duration: Option<i64>,
    /// 播放量
    #[sea_orm(column_type = "BigInteger", nullable)]
    pub view: Option<i64>,
    /// 视频所处状态：completed / failed / removed / pay_blocked / pending / downloading / tampered
    #[sea_orm(default_value = "completed")]
    pub state: Option<String>,
    /// 本地封面文件绝对路径
    #[sea_orm(column_type = "Text", nullable)]
    pub cover_local_path: Option<String>,
    /// 充电/付费原因：upower_paid / upower_no_permission / ugc_pay_paid / ugc_pay_no_permission / pay_paid / pay_no_permission / state_under_review / state_deleted / playurl_failed / unknown
    #[sea_orm(nullable)]
    pub pay_note: Option<String>,
    /// 疑似重投：指向老 bvid（纯提示，不自动重下）
    #[sea_orm(nullable)]
    pub reupload_of: Option<String>,
    /// 文件 MD5（on_completion / periodic 校验后写入）
    #[sea_orm(nullable)]
    pub md5: Option<String>,
    /// MD5 上次校验时间
    pub md5_last_checked_at: Option<DateTime<Local>>,
    /// view 字段上次刷新时间（L1 worker）
    pub view_refreshed_at: Option<DateTime<Local>>,
    /// view 来源：snapshot（入库时一次性）/ live（L1 worker 刷新）
    #[sea_orm(nullable)]
    pub view_source: Option<String>,
    /// 弹幕是否已烧录进视频
    #[sea_orm(default_value = false)]
    pub burned_danmaku: Option<bool>,
    /// CC 字幕是否已烧录进视频
    #[sea_orm(default_value = false)]
    pub burned_subtitle: Option<bool>,
    /// UP 主名字快照（入库时从视频信息 owner 字段落库，未监控博主的看板分组兜底显示）
    #[sea_orm(nullable)]
    pub owner_name: Option<String>,
    /// UP 主头像 URL 快照（同上）
    #[sea_orm(column_type = "Text", nullable)]
    pub owner_face: Option<String>,
    /// 自动烧录调度状态：queued / running / failed / completed。
    pub auto_burn_status: Option<String>,
    /// 自动烧录累计尝试次数。
    #[sea_orm(default_value = 0)]
    pub auto_burn_attempts: i32,
    /// 自动烧录失败后的下一次允许重试时间。
    pub auto_burn_next_retry_at: Option<DateTime<Local>>,
    /// 计划侧车下载累计失败次数。
    #[sea_orm(default_value = 0)]
    pub sidecar_attempts: i32,
    /// 下一次需要检查或重试计划侧车下载的时间。
    pub next_sidecar_at: Option<DateTime<Local>>,
    /// 乐观锁版本号：每次历史记录变更 +1。调用方在写操作时传 `expected_version` 校验，
    /// 不匹配则返回 CONFLICT（详见 `services/conflict_guard.rs`）。
    #[sea_orm(default_value = 0)]
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub fn to_api(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "uid": self.uid,
            "bvid": self.bvid,
            "source": self.source,
            "title": self.title,
            "pub_date": self.pub_date,
            "pub_timestamp": self.pub_timestamp,
            "download_time": self.download_time.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            "file_path": self.file_path,
            "next_download_index": self.next_download_index,
            "pic": self.pic,
            "duration": self.duration,
            "view": self.view,
            "state": self.state,
            "cover_local_path": self.cover_local_path,
            "pay_note": self.pay_note,
            "reupload_of": self.reupload_of,
            "md5": self.md5,
            "md5_last_checked_at": self.md5_last_checked_at.map(|t| t.to_rfc3339()),
            "view_refreshed_at": self.view_refreshed_at.map(|t| t.to_rfc3339()),
            "view_source": self.view_source,
            "burned_danmaku": self.burned_danmaku.unwrap_or(false),
            "burned_subtitle": self.burned_subtitle.unwrap_or(false),
            "owner_name": self.owner_name,
            "owner_face": self.owner_face,
            "auto_burn_status": self.auto_burn_status,
            "auto_burn_attempts": self.auto_burn_attempts,
            "auto_burn_next_retry_at": self.auto_burn_next_retry_at.map(|t| t.to_rfc3339()),
            "sidecar_attempts": self.sidecar_attempts,
            "next_sidecar_at": self.next_sidecar_at.map(|t| t.to_rfc3339()),
        })
    }
}
