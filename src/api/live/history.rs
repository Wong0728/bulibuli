use super::{EventsQuery, HistoryQuery};
use crate::error::{ApiResponse, AppError};
use crate::services::danmu_collector::commands::{
    command_base, is_link_command, is_stats_command, is_system_command, system_event_label,
};
use crate::services::live_recorder::ArchivedLiveEvent;
use crate::services::security_config::can_open_directory;
use crate::state::SharedState;
use axum::extract::{Path, Query, State};
use axum::Json;
use futures::{stream, StreamExt};
use serde_json::{json, Value};
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
        "event_label": event_label(event),
        "display_text": event_display_text(event),
        "data": event.data,
        "raw": event.raw,
        "history_backfill": event.history_backfill,
    })
}

fn event_category(event: &ArchivedLiveEvent) -> &'static str {
    match event.event_type.as_str() {
        "danmaku" | "gift" | "super_chat" | "guard" | "interact" | "like" | "entry"
        | "link_mic_pk" => "user",
        "watched" | "stats" => "stats",
        "system" | "capture_gap" => "system",
        "unknown" => {
            let cmd = command_base(&event.cmd);
            if matches!(
                cmd,
                "INTERACT_WORD"
                    | "INTERACT_WORD_V2"
                    | "INTERACT_WORD_V3"
                    | "WELCOME"
                    | "WELCOME_GUARD"
                    | "ENTRY_EFFECT"
                    | "LIKE_INFO_V3_CLICK"
            ) {
                "user"
            } else if matches!(cmd, "WATCHED_CHANGE" | "LIKE_INFO_V3_UPDATE")
                || is_stats_command(cmd)
            {
                "stats"
            } else if matches!(cmd, "LIVE" | "PREPARING")
                || is_system_command(cmd)
                || is_link_command(cmd)
            {
                if is_link_command(cmd) {
                    "user"
                } else {
                    "system"
                }
            } else {
                "unknown"
            }
        }
        _ => "unknown",
    }
}

fn event_payload(event: &ArchivedLiveEvent) -> &Value {
    event
        .raw
        .as_ref()
        .and_then(|raw| raw.get("data"))
        .unwrap_or(&event.data)
}

fn payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    })
}

fn payload_i64(payload: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        payload.get(*key).and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
                .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
        })
    })
}

fn clean_entry_text(value: String) -> String {
    let mut result = value;
    let mut search_from = 0;
    while let Some(start_rel) = result[search_from..].find("<%") {
        let start = search_from + start_rel;
        let Some(end_rel) = result[start + 2..].find("%>") else {
            break;
        };
        let end = start + 2 + end_rel;
        let inner = result[start + 2..end].to_owned();
        result.replace_range(start..end + 2, &inner);
        search_from = start + inner.len();
    }
    result
}

fn event_label(event: &ArchivedLiveEvent) -> &'static str {
    match event.event_type.as_str() {
        "danmaku" => "弹幕",
        "gift" => "礼物",
        "super_chat" => "SC",
        "guard" => "上舰",
        "interact" => "进场互动",
        "like" => "点赞",
        "entry" => "进场特效",
        "link_mic_pk" => "连麦 / PK",
        "watched" => "看过人数",
        "stats" => "统计",
        "system" | "capture_gap" => "系统",
        "unknown" => match event_category(event) {
            "user" => "用户互动",
            "stats" => "统计",
            "system" => "系统",
            _ => "未识别命令",
        },
        _ => "事件",
    }
}

fn event_display_text(event: &ArchivedLiveEvent) -> String {
    let payload = event_payload(event);
    let cmd = command_base(&event.cmd);
    match event.event_type.as_str() {
        "danmaku" => payload_string(payload, &["text"]).unwrap_or_default(),
        "gift" => format!(
            "{} ×{}",
            payload_string(payload, &["gift_name", "giftName"])
                .unwrap_or_else(|| "礼物".to_owned()),
            payload_i64(payload, &["num"]).unwrap_or(1)
        ),
        "super_chat" => format!(
            "SC ¥{}：{}",
            payload_i64(payload, &["price"]).unwrap_or(0),
            payload_string(payload, &["message", "text"]).unwrap_or_default()
        ),
        "guard" => format!(
            "上舰 等级 {}",
            payload_i64(payload, &["guard_level"]).unwrap_or(0)
        ),
        "interact" => match payload_i64(payload, &["msg_type"]).unwrap_or(0) {
            2 => "关注了直播间".to_owned(),
            3 => "分享了直播间".to_owned(),
            _ => "进入直播间".to_owned(),
        },
        "like" => {
            payload_string(payload, &["text", "like_text"]).unwrap_or_else(|| "点赞了".to_owned())
        }
        "entry" => payload_string(payload, &["text", "copy_writing"])
            .map(clean_entry_text)
            .unwrap_or_else(|| "进场特效".to_owned()),
        "watched" => format!(
            "看过人数：{}",
            payload_i64(payload, &["count", "num"]).unwrap_or(0)
        ),
        "stats" => payload_string(payload, &["text", "label"])
            .or_else(|| {
                (payload_i64(payload, &["value", "count", "num"]).is_some()).then(|| {
                    format!(
                        "{}：{}",
                        event_label(event),
                        payload_i64(payload, &["value", "count", "num"]).unwrap_or(0)
                    )
                })
            })
            .unwrap_or_else(|| "统计更新".to_owned()),
        "system" => {
            if cmd == "LIVE" {
                "直播开始".to_owned()
            } else if cmd == "PREPARING" {
                "直播结束".to_owned()
            } else {
                payload_string(payload, &["text", "message", "msg"])
                    .unwrap_or_else(|| system_event_label(cmd).to_owned())
            }
        }
        "capture_gap" => format!(
            "互动采集发生丢失（{} 条）",
            payload_i64(payload, &["dropped"]).unwrap_or(0)
        ),
        "link_mic_pk" => payload_string(payload, &["text", "message", "msg"])
            .unwrap_or_else(|| format!("连麦 / PK：{cmd}")),
        "unknown" => {
            if cmd == "ENTRY_EFFECT" {
                payload_string(payload, &["copy_writing", "msg", "message"])
                    .map(clean_entry_text)
                    .unwrap_or_else(|| "进场特效".to_owned())
            } else if cmd == "LIKE_INFO_V3_CLICK" {
                payload_string(payload, &["like_text", "msg", "message"])
                    .unwrap_or_else(|| "点赞了".to_owned())
            } else if cmd == "WATCHED_CHANGE" {
                format!(
                    "看过人数：{}",
                    payload_i64(payload, &["num", "count"]).unwrap_or(0)
                )
            } else if matches!(cmd, "ONLINE_RANK_V3" | "ONLINE_RANK_COUNT") {
                "在线榜数据更新".to_owned()
            } else if cmd == "LIKE_INFO_V3_UPDATE" {
                "点赞数更新".to_owned()
            } else if matches!(cmd, "LIVE" | "PREPARING") {
                if cmd == "LIVE" {
                    "直播开始"
                } else {
                    "直播结束"
                }
                .to_owned()
            } else if is_system_command(cmd) {
                system_event_label(cmd).to_owned()
            } else if is_link_command(cmd) {
                format!("连麦 / PK：{cmd}")
            } else {
                format!("未知命令：{}", if cmd.is_empty() { "空命令" } else { cmd })
            }
        }
        _ => "事件更新".to_owned(),
    }
}

pub(super) async fn history(
    State(state): State<SharedState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let can_open_directory = can_open_directory(&state.bili.security.current().mode);
    let rows = state
        .media
        .live_recorder
        .history(query.limit.unwrap_or(30))
        .await?;
    let items = stream::iter(rows.iter().cloned())
        .map(move |row| async move { history_view(&row, can_open_directory).await })
        .buffered(8)
        .collect::<Vec<_>>()
        .await;
    Ok(Json(ApiResponse::success(json!({ "items": items }))))
}

pub(super) async fn history_item(
    State(state): State<SharedState>,
    Path(recording_id): Path<i32>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let can_open_directory = can_open_directory(&state.bili.security.current().mode);
    let row = state
        .media
        .live_recorder
        .history_item(recording_id)
        .await?
        .ok_or_else(|| AppError::NotFound("录制历史不存在".into()))?;
    Ok(Json(ApiResponse::success(
        history_view(&row, can_open_directory).await,
    )))
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
    if !can_open_directory(&state.bili.security.current().mode) {
        return Err(AppError::BadRequest("仅本机访问支持打开所在目录".into()));
    }
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

pub(super) async fn history_view(
    row: &crate::models::live_recording::Model,
    can_open_directory: bool,
) -> serde_json::Value {
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
        "can_open_directory": can_open_directory,
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

    #[test]
    fn gives_known_legacy_commands_readable_text() {
        let mut entry = event("ENTRY_EFFECT", "unknown");
        entry.raw = Some(json!({
            "cmd": "ENTRY_EFFECT",
            "data": {"copy_writing": "欢迎 <%用户A%>"}
        }));
        assert_eq!(event_category(&entry), "user");
        assert_eq!(event_label(&entry), "用户互动");
        assert_eq!(event_display_text(&entry), "欢迎 用户A");

        let unknown = event("SOME_NEW_CMD", "unknown");
        assert_eq!(event_category(&unknown), "unknown");
        assert_eq!(event_display_text(&unknown), "未知命令：SOME_NEW_CMD");
    }
}
