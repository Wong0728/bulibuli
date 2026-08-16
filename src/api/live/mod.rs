use crate::error::{ApiResponse, AppError};
use crate::models::burn::BurnTask;
use crate::services::auth::ClientInfo;
use crate::services::live_recorder::RecordingTrigger;
use crate::services::live_source::{
    schedule_from_json, CaptureMode, NewLiveSource, UpdateLiveSource, WeeklySchedule,
};
use crate::services::security_config::can_open_directory;
use crate::services::subtitle_burner::{DanmakuItem, SubtitleBurner};
use crate::state::SharedState;
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/live/room-info", get(room_info))
        .route("/api/live/start", post(start_recording))
        .route("/api/live/stop", post(stop_recording))
        .route("/api/live/status", get(recording_status))
        .route("/api/live/dashboard", get(dashboard))
        .route("/api/live/source/add", post(add_source))
        .route("/api/live/source/update", post(update_source))
        .route("/api/live/source/delete", post(delete_source))
        .route("/api/live/events", get(history::events))
        .route("/api/live/history", get(history::history))
        .route(
            "/api/live/history/{recording_id}",
            get(history::history_item),
        )
        .route("/api/live/history/{recording_id}/merge", post(start_merge))
        .route(
            "/api/live/history/{recording_id}/burn-danmaku",
            post(burn_recording_danmaku),
        )
        .route(
            "/api/live/history/{recording_id}/open-directory",
            post(history::open_history_directory),
        )
        .route("/api/live/recovery", get(history::recovery))
        .route("/api/live/merge/{job_id}", get(merge_job))
        .route("/api/live/merge/{job_id}/cancel", post(cancel_merge))
}

mod history;

#[derive(Deserialize)]
struct RoomQuery {
    room_id: i64,
}
#[derive(Deserialize)]
struct RoomBody {
    room_id: i64,
}
#[derive(Deserialize)]
struct EventsQuery {
    room_id: i64,
    recording_id: Option<i32>,
    #[serde(default)]
    after_seq: u64,
    limit: Option<usize>,
}
#[derive(Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}
#[derive(Deserialize)]
struct AddSourceBody {
    room_id: i64,
    auto_record_enabled: Option<bool>,
    weekly_schedule: Option<WeeklySchedule>,
    capture_mode: Option<CaptureMode>,
    max_qn: Option<i32>,
}

async fn room_info(
    State(state): State<SharedState>,
    Query(query): Query<RoomQuery>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if query.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    let cookies = state.infra.settings_service.cookie_header().await?;
    let init = state
        .bili
        .bili_api
        .live_room_init(query.room_id, &cookies)
        .await?;
    let info = state
        .bili
        .bili_api
        .live_get_info(init.room_id, &cookies)
        .await?;
    let profile = state
        .bili
        .bili_api
        .get_user_info(init.uid, &cookies)
        .await
        .ok();
    let recording = state.media.live_recorder.status(init.room_id).await;
    let source = state
        .business
        .live_source_service
        .find(init.room_id)
        .await?;
    Ok(Json(ApiResponse::success(json!({
        "room_id": init.room_id, "short_id": init.short_id, "uid": init.uid,
        "anchor_name": profile.as_ref().map(|p| p.name.as_str()).unwrap_or(""),
        "face": profile.as_ref().map(|p| p.face.as_str()).unwrap_or(""),
        "title": info.title, "live_status": init.live_status,
        "live_status_text": match init.live_status { 0 => "未开播", 1 => "直播中", 2 => "轮播中", _ => "未知" },
        "online": info.online, "user_cover": info.user_cover, "area_name": info.area_name,
        "parent_area_name": info.parent_area_name, "live_time": info.live_time, "tags": info.tags,
        "is_portrait": info.is_portrait, "encrypted": init.encrypted, "is_recording": recording.is_some(),
        "recording_status": recording.as_ref().map(|v| v.status.to_string()),
        "can_start": init.is_live() && recording.is_none(), "is_saved": source.is_some(),
    }))))
}

async fn start_recording(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    let source_before = state
        .business
        .live_source_service
        .find(body.room_id)
        .await?;
    let mode = source_before
        .as_ref()
        .map(|source| CaptureMode::parse(&source.capture_mode).unwrap_or_default())
        .unwrap_or(CaptureMode::Standard);
    if source_before.is_some() {
        state
            .business
            .live_source_service
            .set_manual_latch(body.room_id, false)
            .await?;
    }
    let info = match state
        .media
        .live_recorder
        .start_with_options(body.room_id, RecordingTrigger::Manual, mode)
        .await
    {
        Ok(info) => info,
        Err(error) => {
            if let Some(source) = source_before {
                let _ = state
                    .business
                    .live_source_service
                    .set_manual_stop_session(body.room_id, source.manual_stop_session_key)
                    .await;
            }
            return Err(error.into());
        }
    };
    Ok(Json(ApiResponse::with_message(json!(info), "录制已开始")))
}

async fn stop_recording(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    let session_key = match state.infra.settings_service.cookie_header().await {
        Ok(cookies) => state
            .bili
            .bili_api
            .live_room_init(body.room_id, &cookies)
            .await
            .ok()
            .filter(|init| init.is_live() && init.live_time > 0)
            .map(|init| format!("{}:{}", init.room_id, init.live_time)),
        Err(_) => None,
    };
    let source_before = state
        .business
        .live_source_service
        .find(body.room_id)
        .await?;
    if session_key.is_some() {
        state
            .business
            .live_source_service
            .set_manual_stop_session(body.room_id, session_key.clone())
            .await?;
    } else {
        state
            .business
            .live_source_service
            .set_manual_latch(body.room_id, true)
            .await?;
    }
    let job = match state.media.live_recorder.request_stop(body.room_id).await {
        Ok(job) => job,
        Err(error) => {
            if let Some(source) = source_before.clone() {
                if source.manual_stop_session_key.is_some() {
                    let _ = state
                        .business
                        .live_source_service
                        .set_manual_stop_session(body.room_id, source.manual_stop_session_key)
                        .await;
                } else {
                    let _ = state
                        .business
                        .live_source_service
                        .set_manual_latch(body.room_id, source.manual_stop_latched)
                        .await;
                }
            }
            return Err(AppError::BadRequest(
                crate::services::live_recorder::ffmpeg_session::redact_diagnostics(
                    &error.to_string(),
                ),
            ));
        }
    };
    Ok(Json(ApiResponse::with_message(
        json!({"operation_id": job.id, "recording_id": job.recording_id, "status": job.status, "progress": job.progress}),
        "停止请求已接受，正在后台收尾与合并",
    )))
}

async fn recording_status(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let sessions = state.media.live_recorder.status_all().await;
    Ok(Json(ApiResponse::success(
        json!({"count": sessions.len(), "sessions": sessions}),
    )))
}

async fn dashboard(
    State(state): State<SharedState>,
    Extension(client): Extension<ClientInfo>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let can_open_directory = can_open_directory(&state.bili.security.current().mode, client.ip);
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
            "id": source.id, "room_id": source.room_id, "short_id": source.short_id, "uid": source.uid,
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
        "disk": disk_status(&state.infra.paths.download_dir),
    }))))
}

/// 录制目录所在磁盘的余量；读取失败时返回 null，前端按“未知”降级展示。
fn disk_status(dir: &Path) -> serde_json::Value {
    match (fs2::available_space(dir), fs2::total_space(dir)) {
        (Ok(available), Ok(total)) => json!({
            "available_bytes": available,
            "total_bytes": total,
            "path_hidden": true,
        }),
        _ => serde_json::Value::Null,
    }
}

async fn add_source(
    State(state): State<SharedState>,
    Json(body): Json<AddSourceBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    let cookies = state.infra.settings_service.cookie_header().await?;
    let init = state
        .bili
        .bili_api
        .live_room_init(body.room_id, &cookies)
        .await?;
    let info = state
        .bili
        .bili_api
        .live_get_info(init.room_id, &cookies)
        .await?;
    let profile = state
        .bili
        .bili_api
        .get_user_info(init.uid, &cookies)
        .await
        .ok();
    let source = state
        .business
        .live_source_service
        .add(NewLiveSource {
            room_id: init.room_id,
            short_id: init.short_id,
            uid: init.uid,
            anchor_name: profile
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| format!("UID {}", init.uid)),
            face: profile.as_ref().map(|p| p.face.clone()).unwrap_or_default(),
            title: info.title,
            cover: info.user_cover,
            auto_record_enabled: body.auto_record_enabled.unwrap_or(false),
            weekly_schedule: body.weekly_schedule,
            capture_mode: body.capture_mode.unwrap_or_default(),
            max_qn: body
                .max_qn
                .unwrap_or(crate::services::live_source::default_max_qn()),
        })
        .await
        .map_err(|e| AppError::Conflict(e.to_string()))?;
    state.business.live_monitor.wake_room(source.room_id).await;
    Ok(Json(ApiResponse::with_message(
        json!(source),
        "直播源已添加",
    )))
}

async fn update_source(
    State(state): State<SharedState>,
    Json(body): Json<UpdateLiveSource>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let source = state
        .business
        .live_source_service
        .update(body)
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    state.business.live_monitor.wake_room(source.room_id).await;
    Ok(Json(ApiResponse::with_message(
        json!(source),
        "直播源设置已保存",
    )))
}

async fn delete_source(
    State(state): State<SharedState>,
    Json(body): Json<RoomBody>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if body.room_id <= 0 {
        return Err(AppError::BadRequest("直播间号必须为正整数".into()));
    }
    if state
        .media
        .live_recorder
        .status(body.room_id)
        .await
        .is_some()
    {
        return Err(AppError::Conflict("请先停止当前录制再删除直播源".into()));
    }
    let deleted = state
        .business
        .live_source_service
        .delete(body.room_id)
        .await?;
    Ok(Json(ApiResponse::success(json!({"deleted": deleted}))))
}

async fn start_merge(
    State(state): State<SharedState>,
    axum::extract::Path(recording_id): axum::extract::Path<i32>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let job = state
        .media
        .live_recorder
        .retry_merge(recording_id)
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(Json(ApiResponse::with_message(
        json!(job),
        "录制合并任务已后台创建",
    )))
}

async fn burn_recording_danmaku(
    State(state): State<SharedState>,
    axum::extract::Path(recording_id): axum::extract::Path<i32>,
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
        .map(Path::new)
        .filter(|path| path.exists())
        .ok_or_else(|| AppError::BadRequest("该录制没有可烧录的输出视频".into()))?
        .to_path_buf();
    let events_path = row
        .event_path
        .as_deref()
        .map(Path::new)
        .filter(|path| path.exists())
        .ok_or_else(|| AppError::BadRequest("该录制没有互动归档，无法烧录弹幕".into()))?
        .to_path_buf();
    let task_key = format!("live-recording-{recording_id}");
    // 去重检查与插入之间不再释放锁：否则两个并发请求都能通过检查各自 spawn
    // 一个 ffmpeg（对照 download/burn.rs 的实现——先插占位再 spawn，无此窗口）。
    // items 的读取放在持锁期间完成，load_live_burn_items 是本地文件解析，耗时可预期。
    let (items, task_id, task_guard) = {
        let mut task_guard = state.media.burn_tasks.lock().await;
        crate::models::burn::prune_burn_tasks(&mut task_guard);
        if task_guard.values().any(|task| {
            task.bvid == task_key && crate::models::burn::burn_status_active(&task.status)
        }) {
            return Err(AppError::Conflict("该录制的烧录任务已在进行中".into()));
        }
        let items = load_live_burn_items(&events_path)
            .await
            .map_err(|error| AppError::BadRequest(format!("读取互动归档失败: {error}")))?;
        if items.is_empty() {
            return Err(AppError::BadRequest("互动归档中没有弹幕或 SC".into()));
        }
        let task_id: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
        task_guard.insert(
            task_id.clone(),
            BurnTask {
                bvid: task_key.clone(),
                status: "queued".to_string(),
                message: "烧录任务已排队".to_string(),
                output_path: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
        );
        (items, task_id, task_guard)
    };
    let settings = state.infra.settings_service.current();
    let burn_config = settings.burn.to_burn_config();
    let custom_path = settings.ffmpeg.custom_path.trim().to_string();
    let custom_ffmpeg = (!custom_path.is_empty()).then_some(custom_path);
    let burner = SubtitleBurner::with_burn_config(
        state.media.video_processor.clone(),
        settings.ffmpeg.mode.clone(),
        custom_ffmpeg,
        burn_config,
    );
    let burn_tasks = state.media.burn_tasks.clone();
    let burn_semaphore = state.media.burn_semaphore.clone();
    let response_task_id = task_id.clone();
    let download_dir = state.infra.paths.download_dir.clone();
    drop(task_guard);
    tokio::spawn(async move {
        let Ok(_permit) = burn_semaphore.acquire_owned().await else {
            let mut tasks = burn_tasks.lock().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = "failed".to_string();
                task.message = "获取烧录并发槽失败".to_string();
                task.updated_at = chrono::Utc::now().timestamp();
            }
            return;
        };
        {
            let mut tasks = burn_tasks.lock().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = "running".to_string();
                task.message = "正在烧录互动弹幕，请勿关闭程序".to_string();
                task.updated_at = chrono::Utc::now().timestamp();
            }
        }
        let result = burner.burn_live_interactions(&output, items).await;
        let mut tasks = burn_tasks.lock().await;
        if let Some(task) = tasks.get_mut(&task_id) {
            match result {
                Ok((true, path, message)) => {
                    task.status = "completed".to_string();
                    task.message =
                        crate::services::live_recorder::ffmpeg_session::redact_diagnostics(
                            &message,
                        );
                    task.output_path = path
                        .as_deref()
                        .and_then(|value| value.strip_prefix(&download_dir).ok())
                        .map(|value| value.to_string_lossy().replace('\\', "/"));
                    task.updated_at = chrono::Utc::now().timestamp();
                }
                Ok((false, _, message)) => {
                    task.status = "failed".to_string();
                    task.message =
                        crate::services::live_recorder::ffmpeg_session::redact_diagnostics(
                            &message,
                        );
                    task.updated_at = chrono::Utc::now().timestamp();
                }
                Err(error) => {
                    task.status = "failed".to_string();
                    task.message =
                        crate::services::live_recorder::ffmpeg_session::redact_diagnostics(
                            &format!("烧录失败: {error}"),
                        );
                    task.updated_at = chrono::Utc::now().timestamp();
                }
            }
        }
    });
    Ok(Json(ApiResponse::with_message(
        json!({"task_id": response_task_id, "status": "queued"}),
        "烧录任务已排队，完成后会生成带弹幕的版本",
    )))
}

/// 从直播互动 JSONL 归档提取可烧录条目：弹幕保留归档中的模式、字号和颜色，SC 走顶部固定轨道。
async fn load_live_burn_items(events_path: &Path) -> anyhow::Result<Vec<DanmakuItem>> {
    let content = tokio::fs::read_to_string(events_path).await?;
    let mut items = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let time_secs = value
            .get("media_time_ms")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0) as f64
            / 1000.0;
        let data = value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match value.get("event_type").and_then(serde_json::Value::as_str) {
            Some("danmaku") => {
                let text = data
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if text.is_empty() {
                    continue;
                }
                let source_mode = data.get("mode").and_then(live_json_i64).unwrap_or(1);
                let (mode, bottom) = live_danmaku_mode(source_mode);
                let size = data
                    .get("font_size")
                    .and_then(live_json_i64)
                    .unwrap_or(25)
                    .clamp(1, 100) as i32;
                let color = live_danmaku_color(data.get("color"));
                items.push(DanmakuItem {
                    text,
                    time: time_secs,
                    mode: mode.to_string(),
                    size,
                    color,
                    bottom,
                });
            }
            Some("super_chat") => {
                let uname = data
                    .get("uname")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("匿名");
                let message = data
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let price = data
                    .get("price")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                items.push(DanmakuItem {
                    text: format!("SC ¥{price} {uname}: {message}"),
                    time: time_secs,
                    mode: "TOP".to_string(),
                    size: 30,
                    color: "FFB300".to_string(),
                    bottom: false,
                });
            }
            _ => {}
        }
        if items.len() >= 200_000 {
            break;
        }
    }
    Ok(items)
}

fn live_json_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
}

fn live_danmaku_mode(mode: i64) -> (&'static str, bool) {
    match mode {
        4 => ("BOTTOM", true),
        5 => ("TOP", false),
        _ => ("R2L", false),
    }
}

fn live_danmaku_color(value: Option<&serde_json::Value>) -> String {
    let color = value.and_then(live_json_i64).unwrap_or(0xFFFFFF) & 0xFFFFFF;
    format!("{color:06X}")
}

async fn merge_job(
    State(state): State<SharedState>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let job = state
        .media
        .live_recorder
        .merge_job(&job_id)
        .await
        .ok_or_else(|| AppError::NotFound("合并任务不存在".into()))?;
    Ok(Json(ApiResponse::success(json!(job))))
}

async fn cancel_merge(
    State(state): State<SharedState>,
    axum::extract::Path(job_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let job = state
        .media
        .live_recorder
        .cancel_merge(&job_id)
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(Json(ApiResponse::with_message(
        json!(job),
        "已请求取消合并任务",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_error_strips_signed_url_tokens() {
        let message = "获取流失败: https://cdn.example.com/live.flv?sign=abc&token=secret";
        let result = history::public_error(message);
        assert!(result.contains("https://cdn.example.com/live.flv"));
        assert!(!result.contains("token=secret"));
        assert!(!result.contains("sign=abc"));
    }

    #[test]
    fn public_error_redacts_absolute_paths() {
        assert_eq!(
            history::public_error("D:\\downloads\\live\\out.flv"),
            "diagnostic redacted"
        );
        assert_eq!(
            history::public_error("/var/data/out.flv"),
            "diagnostic redacted"
        );
    }

    #[test]
    fn public_error_keeps_plain_messages() {
        assert_eq!(history::public_error(" 认证被拒绝 "), "认证被拒绝");
    }

    #[tokio::test]
    async fn live_burn_items_extract_danmaku_and_super_chat() {
        let dir =
            std::env::temp_dir().join(format!("live-burn-test-{}", uuid::Uuid::new_v4().simple()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("events.jsonl");
        let content = [
            r#"{"seq":1,"media_time_ms":1500,"event_type":"danmaku","data":{"text":"你好","mode":4,"font_size":33,"color":1122867}}"#,
            r#"{"seq":2,"media_time_ms":3000,"event_type":"super_chat","data":{"uname":"某人","price":30,"message":"唱得好"}}"#,
            r#"{"seq":3,"media_time_ms":4000,"event_type":"gift","data":{"gift_name":"烟花"}}"#,
            r#"{"seq":4,"media_time_ms":5000,"event_type":"danmaku","data":{"text":"   "}}"#,
        ]
        .join("\n");
        tokio::fs::write(&path, content).await.unwrap();
        let items = load_live_burn_items(&path).await.unwrap();
        let _ = tokio::fs::remove_dir_all(&dir).await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "你好");
        assert!((items[0].time - 1.5).abs() < 1e-9);
        assert_eq!(items[0].mode, "BOTTOM");
        assert_eq!(items[0].size, 33);
        assert_eq!(items[0].color, "112233");
        assert!(items[0].bottom);
        assert_eq!(items[1].mode, "TOP");
        assert!(items[1].text.contains("SC ¥30"));
        assert!(items[1].text.contains("唱得好"));
    }
}
