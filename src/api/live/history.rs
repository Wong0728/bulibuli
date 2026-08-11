use super::{EventsQuery, HistoryQuery};
use crate::error::{ApiResponse, AppError};
use crate::services::live_recorder::ArchivedLiveEvent;
use crate::state::SharedState;
use axum::extract::{Path, Query, State};
use axum::Json;
use futures::{stream, StreamExt};
use serde_json::json;
use std::path::Path as FsPath;

pub(super) async fn events(
    State(state): State<SharedState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 100);
    let current_recording_id = state
        .media
        .live_recorder
        .status(query.room_id)
        .await
        .and_then(|info| info.recording_id);
    if query.recording_id.is_some() && query.recording_id != current_recording_id {
        return Err(AppError::Conflict(
            "直播录制实例已切换，请重新同步事件".to_string(),
        ));
    }
    let events = state
        .media
        .live_recorder
        .events(query.room_id, query.after_seq, limit)
        .await;
    let next_seq = events
        .last()
        .map(|event| event.seq)
        .unwrap_or(query.after_seq);
    let event_views = events.iter().map(event_view).collect::<Vec<_>>();
    Ok(Json(ApiResponse::success(
        json!({"events": event_views, "next_seq": next_seq, "recording_id": current_recording_id}),
    )))
}

/// 将采集协议命令映射为用户可理解的展示分类。
///
/// 只做稳定的展示分类，不尝试解析所有 B 站版本化 payload；原始事件仍由归档层保存。
fn event_view(event: &ArchivedLiveEvent) -> serde_json::Value {
    let category = event_category(event);
    json!({
        "schema_version": event.schema_version,
        "seq": event.seq,
        "received_at": event.received_at,
        "media_time_ms": event.media_time_ms,
        "segment_index": event.segment_index,
        "cmd": event.cmd,
        "event_type": event.event_type,
        "event_category": category,
        "event_visible": category == "user",
        "data": event.data,
        "raw": event.raw,
        "history_backfill": event.history_backfill,
    })
}

fn event_category(event: &ArchivedLiveEvent) -> &'static str {
    match event.event_type.as_str() {
        "danmaku" | "gift" | "super_chat" | "guard" | "interact" | "link_mic_pk" => "user",
        "watched" => "stats",
        "unknown" => match event.cmd.as_str() {
            "INTERACT_WORD_V2" | "ENTRY_EFFECT" | "LIKE_INFO_V3_CLICK" => "user",
            "WATCHED_CHANGE" | "ONLINE_RANK_V3" | "ONLINE_RANK_COUNT" | "LIKE_INFO_V3_UPDATE" => {
                "stats"
            }
            "STOP_LIVE_ROOM_LIST" => "system",
            _ => "unknown",
        },
        _ => "unknown",
    }
}

#[cfg(test)]
mod event_tests {
    use super::*;
    use serde_json::json;

    fn event(cmd: &str, event_type: &str) -> ArchivedLiveEvent {
        ArchivedLiveEvent {
            schema_version: 1,
            seq: 1,
            received_at: String::new(),
            media_time_ms: 0,
            segment_index: 0,
            cmd: cmd.to_string(),
            event_type: event_type.to_string(),
            data: json!({}),
            raw: None,
            history_backfill: false,
        }
    }

    #[test]
    fn classifies_protocol_noise_without_discarding_it() {
        assert_eq!(event_category(&event("WATCHED_CHANGE", "watched")), "stats");
        assert_eq!(event_category(&event("ONLINE_RANK_V3", "unknown")), "stats");
        assert_eq!(event_category(&event("ENTRY_EFFECT", "unknown")), "user");
        assert_eq!(
            event_category(&event("STOP_LIVE_ROOM_LIST", "unknown")),
            "system"
        );
        assert_eq!(
            event_view(&event("ONLINE_RANK_V3", "unknown"))["event_visible"],
            false
        );
    }
}

pub(super) async fn history(
    State(state): State<SharedState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let rows = state
        .media
        .live_recorder
        .history(query.limit.unwrap_or(30))
        .await?;
    let items = stream::iter(rows.iter().map(history_view))
        .buffered(8)
        .collect::<Vec<_>>()
        .await;
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

pub(super) async fn history_item(
    State(state): State<SharedState>,
    Path(recording_id): Path<i32>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = state
        .media
        .live_recorder
        .history_item(recording_id)
        .await?
        .ok_or_else(|| AppError::NotFound("录制历史不存在".into()))?;
    Ok(Json(ApiResponse::success(history_view(&row).await)))
}

pub(super) async fn recovery(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let items = state.media.live_recorder.recovery_items().await?;
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

pub(super) async fn open_history_directory(
    State(state): State<SharedState>,
    Path(recording_id): Path<i32>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let row = state
        .media
        .live_recorder
        .history_item(recording_id)
        .await?
        .ok_or_else(|| AppError::NotFound("录制历史不存在".into()))?;
    let output = row
        .output_path
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("该录制没有可打开的输出文件".into()))?;
    let directory = std::path::Path::new(output)
        .parent()
        .ok_or_else(|| AppError::BadRequest("录制输出路径无效".into()))?;
    if !directory.is_dir() {
        return Err(AppError::NotFound("录制输出目录不存在".into()));
    }
    open::that(directory)
        .map_err(|_| AppError::Internal("open recording directory failed".to_owned()))?;
    Ok(Json(ApiResponse::with_message(
        json!({"recording_id": recording_id}),
        "已打开录制目录",
    )))
}

pub(super) async fn history_view(row: &crate::models::live_recording::Model) -> serde_json::Value {
    let output_path = row.output_path.as_deref().map(FsPath::new);
    let event_path = row.event_path.as_deref().map(FsPath::new);
    let burned_path = row.output_path.as_deref().and_then(|path| {
        let source = FsPath::new(path);
        source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(|stem| source.with_file_name(format!("{stem}_弹幕版.mp4")))
    });
    let (has_output, has_events, has_burned) = tokio::join!(
        path_exists(output_path),
        path_exists(event_path),
        path_exists(burned_path.as_deref()),
    );
    json!({
        "id": row.id, "room_id": row.room_id, "title": row.title, "cover": row.cover,
        "status": row.status, "started_at": row.started_at, "ended_at": row.ended_at,
        "duration": row.duration, "file_size": row.file_size, "error_msg": row.error_msg.as_deref().map(public_error),
        "trigger": row.trigger, "capture_mode": row.capture_mode,
        "interaction_status": row.interaction_status, "interaction_error": row.interaction_error.as_deref().map(public_error),
        "danmaku_count": row.danmaku_count, "unique_user_count": row.unique_user_count,
        "segment_index": row.segment_index, "restart_attempts": row.restart_attempts,
        "stop_reason": row.stop_reason, "is_recoverable": row.is_recoverable,
        "has_output": has_output,
        "has_events": has_events,
        "has_burned": has_burned,
    })
}

async fn path_exists(path: Option<&FsPath>) -> bool {
    match path {
        Some(path) => tokio::fs::try_exists(path).await.unwrap_or(false),
        None => false,
    }
}

pub(super) fn public_error(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.contains("://") {
        return crate::services::live_recorder::ffmpeg_session::redact_diagnostics(trimmed);
    }
    let bytes = trimmed.as_bytes();
    let windows_absolute =
        bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/');
    if trimmed.starts_with('/') || trimmed.starts_with("\\\\") || windows_absolute {
        "diagnostic redacted".to_owned()
    } else {
        trimmed.to_owned()
    }
}
