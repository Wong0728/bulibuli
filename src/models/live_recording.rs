//! 直播录制记录实体。
//!
//! 表结构由 `m20260807_000004_create_live_recordings` 迁移创建。

use sea_orm::entity::prelude::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "live_recordings")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// 直播间真实 ID（长号）
    pub room_id: i64,
    /// 短号（0 表示无短号）
    pub short_id: i64,
    /// 主播 UID
    pub uid: i64,
    /// 直播标题
    pub title: String,
    /// 封面 URL
    pub cover: String,
    /// 录制状态：recording / completed / failed / stopped
    pub status: String,
    /// 录制文件路径（MP4 或 FLV）
    pub output_path: Option<String>,
    /// 弹幕文件路径（JSON）
    pub danmu_path: Option<String>,
    /// 文件大小（字节）
    pub file_size: i64,
    /// 录制时长（秒）
    pub duration: i64,
    /// 开始时间 ISO8601
    pub started_at: String,
    /// 结束时间 ISO8601
    pub ended_at: Option<String>,
    /// 错误信息
    pub error_msg: Option<String>,
    /// 触发方式（manual / live_start / recovery）。列名 record_trigger：
    /// `trigger` 是 SQLite 关键字，m017 已把物理列改名，Rust 字段名保持不变。
    #[sea_orm(column_name = "record_trigger")]
    pub trigger: String,
    pub event_path: Option<String>,
    pub xml_path: Option<String>,
    pub summary_path: Option<String>,
    pub capture_mode: String,
    pub interaction_status: String,
    pub interaction_error: Option<String>,
    pub danmaku_count: i64,
    pub unique_user_count: i64,
    pub free_gift_count: i64,
    pub paid_gift_count: i64,
    pub sc_count: i64,
    pub guard_count: i64,
    pub peak_watched: i64,
    pub dropped_event_count: i64,
    pub estimated_paid_value: f64,
    pub stop_reason: Option<String>,
    pub segment_index: i32,
    pub restart_attempts: i32,
    pub checkpointed_at: Option<String>,
    pub is_recoverable: bool,
    /// 创建时间
    pub created_at: String,
    /// 更新时间
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
