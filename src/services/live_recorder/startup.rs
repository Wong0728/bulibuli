//! 启动准备与守门辅助：文件名模板渲染、录制清晰度上限、磁盘闸门判定、
//! 启动失败落库与恢复状态判定。

use super::ffmpeg_session::redact_diagnostics;
use super::state::RecordingStatus;
use crate::models::{live_recording, live_source};
use anyhow::{anyhow, Result};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// 读取每个直播源的清晰度上限；未配置或非法时返回 None，交给 B 站默认原画。
pub(super) async fn source_max_qn(db: &DatabaseConnection, room_id: i64) -> Option<i32> {
    live_source::Entity::find()
        .filter(live_source::Column::RoomId.eq(room_id))
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|source| source.max_qn)
        .filter(|qn| *qn > 0)
}

/// 渲染录制文件名模板，支持 {room_id} {title} {date} {time} 占位符；
/// 结果未经清洗，调用方需自行 sanitize。
pub(super) fn render_file_template(
    template: &str,
    room_id: i64,
    title: &str,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    let rendered = template
        .replace("{room_id}", &room_id.to_string())
        .replace("{title}", title)
        .replace("{date}", &now.format("%Y%m%d").to_string())
        .replace("{time}", &now.format("%H%M%S").to_string());
    if rendered.trim().is_empty() {
        format!("live_{room_id}")
    } else {
        rendered
    }
}

pub(super) fn ensure_startup_active(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(anyhow!("直播录制启动已取消"))
    } else {
        Ok(())
    }
}

pub(super) fn user_agent() -> &'static str {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
}

pub(super) fn referer() -> &'static str {
    "https://live.bilibili.com/"
}

/// 健康检查的磁盘闸门判定结果。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DiskGate {
    /// 空间充足
    Ok,
    /// 实测可用空间低于阈值
    BelowThreshold,
    /// 探测本身失败（权限、临时 IO 错误、网络盘离线）
    ProbeFailed,
}

/// 把"探测失败"与"确实不足"分开：`probe` 为 `None` 时绝不能当成 0 字节可用，
/// 否则一次 `available_space` 报错就会停掉本来能正常录的场次。
pub(super) fn disk_gate(probe: Option<u64>, min_free_bytes: u64) -> DiskGate {
    match probe {
        Some(available) if available < min_free_bytes => DiskGate::BelowThreshold,
        Some(_) => DiskGate::Ok,
        None => DiskGate::ProbeFailed,
    }
}

/// 启动阶段失败的统一落库：无论写库是否成功都不掩盖原始错误。
pub(super) async fn mark_startup_recording(
    db: &DatabaseConnection,
    recording_id: i32,
    status: RecordingStatus,
    error: String,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let update = live_recording::ActiveModel {
        id: Set(recording_id),
        status: Set(status.to_string()),
        ended_at: Set(Some(now.clone())),
        error_msg: Set(Some(redact_diagnostics(&error))),
        updated_at: Set(now),
        ..Default::default()
    };
    if let Err(update_error) = update.update(db).await {
        warn!(
            recording_id,
            "failed to persist recording startup failure: {update_error}"
        );
    }
}

/// 由记录状态、产物文件与剩余分段数推断恢复状态，供恢复入口与收尾日志共用。
pub(super) fn recovery_state(status: &str, has_output: bool, segment_count: usize) -> &'static str {
    match (status, has_output, segment_count > 0) {
        ("completed", true, _) => "complete",
        ("completed", false, true) => "output_missing_recoverable",
        (_, _, true) => "segments_pending",
        (_, true, false) => "output_pending_db_commit",
        _ => "artifacts_missing",
    }
}
