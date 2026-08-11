use chrono::{DateTime, Local};
use sea_orm::entity::prelude::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "download_tasks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub bvid: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub quality: i32,
    #[sea_orm(column_name = "type")]
    pub task_type: String,
    /// 分P的 cid：单P投稿为 NULL（保持存量行为），多P投稿为对应分P的 cid。
    /// 任务去重键为 (bvid, cid, type)，NULL 与现状语义一致。
    #[sea_orm(nullable)]
    pub cid: Option<i64>,
    /// 分P序号（从 1 开始）：单P为 NULL，多P为该分P页码。
    #[sea_orm(nullable)]
    pub page: Option<i32>,
    /// 分P标题：单P为 NULL，多P为该分P的 part 文本，用于文件命名 {part} 变量。
    #[sea_orm(column_type = "Text", nullable)]
    pub part_title: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub progress_percent: i32,
    pub downloaded_size: i64,
    pub total_size: i64,
    pub speed: i64,
    pub filename: Option<String>,
    pub gid: Option<String>,
    /// 原始下载 URL，用于断点续传恢复
    pub original_url: Option<String>,
    /// 下载目录绝对路径，用于完成后定位文件（避免跨天日期变化导致找不到文件）
    pub download_dir: Option<String>,
    /// 任务来源：auto（自动监控）/ manual（手动下载）；NULL 视为 auto（存量旧数据）。
    /// 仅 auto 任务的下载完成/失败日志携带 UID 写入博主日志。
    #[sea_orm(nullable)]
    pub source: Option<String>,
    pub generation: i64,
    pub completion_triggered: bool,
    pub stage: String,
    /// 调度优先级：数值越大越靠前（manual=300, retry=200, auto=100）。
    #[sea_orm(default_value = 100)]
    pub priority: i32,
    /// 已尝试次数，跨重启保留。
    #[sea_orm(default_value = 0)]
    pub attempts: i32,
    /// 下一次允许自动重试的时间。
    pub next_retry_at: Option<DateTime<Local>>,
    /// 结构化失败分类，供队列和界面展示。
    pub error_kind: Option<String>,
    /// 实际选用的视频画质与编码；与请求值分开保存以记录降级。
    pub selected_quality: Option<i32>,
    pub selected_codec: Option<String>,
    pub fallback_reason: Option<String>,
    /// 博主头像 URL（创建任务时从 blogger 表快照，用于前端展示）
    #[sea_orm(column_type = "Text", nullable)]
    pub face_url: Option<String>,
    pub created_at: Option<DateTime<Local>>,
    pub updated_at: Option<DateTime<Local>>,
    /// 乐观锁版本号：每次状态变更 +1。调用方在写操作时传 `expected_version` 校验，
    /// 不匹配则返回 CONFLICT（详见 `services/conflict_guard.rs`）。
    #[sea_orm(default_value = 0)]
    pub version: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
