//! 直播页看板聚合接口：一次轮询返回源列表、运行时快照、监控健康度与磁盘余量。

use crate::error::{ApiResponse, AppError};
use crate::services::auth::ClientInfo;
use crate::services::live_source::schedule_from_json;
use crate::state::SharedState;
use axum::{extract::State, Extension, Json};
use serde_json::{json, Value};
use std::path::Path;

pub(super) async fn dashboard(
    State(state): State<SharedState>,
    Extension(client): Extension<ClientInfo>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let can_open_directory = client.can_open_directory(&state.bili.security.current().mode);
    let (sources, runtime, sessions, monitor, risk_notice, merge_jobs, recovery) = tokio::join!(
        state.business.live_source_service.list(),
        state.business.live_monitor.runtime_snapshot(),
        state.media.live_recorder.status_all(),
        state.business.live_monitor.health_snapshot(),
        state.business.live_monitor.risk_snapshot(),
        state.media.live_recorder.merge_jobs(),
        state.media.live_recorder.recovery_items(),
    );
    let sources = sources?;
    let recovery = recovery?;
    let items = sources.into_iter().map(|source| {
        let run = runtime.get(&source.room_id);
        json!({
            "id": source.id, "version": source.version, "room_id": source.room_id, "short_id": source.short_id, "uid": source.uid,
            "anchor_name": source.anchor_name, "face": source.face, "title": source.title, "cover": source.cover,
            "auto_record_enabled": source.auto_record_enabled, "capture_mode": source.capture_mode,
            "max_qn": source.max_qn,
            "weekly_schedule": schedule_from_json(source.weekly_schedule.as_deref()),
            "schedule_all_day": source.weekly_schedule.is_none(), "manual_stop_latched": source.manual_stop_latched,
            "runtime": run,
        })
    }).collect::<Vec<_>>();
    Ok(Json(ApiResponse::success(json!({
        "sources": items, "sessions": sessions,
        "monitor": monitor,
        "risk_notice": risk_notice,
        "can_open_directory": can_open_directory,
        "synced_at": chrono::Utc::now().to_rfc3339(), "server_now": chrono::Local::now().to_rfc3339(),
        "server_timezone": chrono::Local::now().format("%Z %:z").to_string(), "poll_interval_secs": 30,
        "merge_jobs": merge_jobs,
        "recovery": recovery,
        "disk": disk_status(&state.infra.paths.download_dir).await,
    }))))
}

/// 录制目录所在磁盘的余量；读取失败时返回 null，前端按“未知”降级展示。
///
/// 走 spawn_blocking：fs2 是同步系统调用，网络盘/休眠中的磁盘可以把一次查询卡住数秒，
/// 而 dashboard 是直播页 30 秒一次的常驻轮询，不能占用 runtime worker。
async fn disk_status(dir: &Path) -> Value {
    let dir = dir.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        match (fs2::available_space(&dir), fs2::total_space(&dir)) {
            (Ok(available), Ok(total)) => json!({
                "available_bytes": available,
                "total_bytes": total,
                "path_hidden": true,
            }),
            _ => Value::Null,
        }
    })
    .await;
    // 阻塞任务 panic（含应用关闭期被强杀）同样降级为"未知"，不让 dashboard 失败。
    result.unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disk_status_reports_unknown_for_unreadable_directory() {
        assert_eq!(
            disk_status(Path::new("Z:\\definitely-not-a-real-drive-path")).await,
            Value::Null
        );
        let known = disk_status(&std::env::temp_dir()).await;
        assert!(
            known.get("available_bytes").is_some(),
            "可读目录应给出可用字节数，实际: {known}"
        );
    }
}
