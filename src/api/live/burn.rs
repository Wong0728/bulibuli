//! 直播录制互动弹幕烧录接口：把录制归档里的弹幕/SC 烧进合并后的成品视频。

use crate::error::{ApiResponse, AppError};
use crate::models::burn::BurnTask;
use crate::services::subtitle_burner::{DanmakuItem, SubtitleBurner};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};

/// 持久化直播烧录任务状态快照（每次状态迁移后调用）。
async fn persist_live_burn_snapshot(
    db: &sea_orm::DatabaseConnection,
    tasks: &crate::state::media::BurnTasks,
    task_id: &str,
) {
    let task = tasks.lock().await.get(task_id).cloned();
    if let Some(task) = task {
        if let Err(error) = crate::models::burn::persist_burn_task(db, task_id, &task).await {
            tracing::error!(task_id, "持久化直播烧录任务状态失败: {error}");
        }
    }
}

/// 烧录只认可「已合并成品」。合并失败时 finalize 仍会把分段前缀留在 output_path
/// 里供 retry_merge 找回分段，只按文件存在性放行会让用户以为整场都录了下来，
/// 实际烧到的只有一个分段。
fn burnable_output_path(output_path: Option<&str>) -> Result<PathBuf, String> {
    let Some(raw) = output_path else {
        return Err("该录制没有可烧录的输出视频".to_owned());
    };
    let path = Path::new(raw);
    if path.extension().and_then(|ext| ext.to_str()) != Some("mp4") {
        return Err("直播分段尚未合并成功，请先重试合并后再烧录".to_owned());
    }
    if !path.is_file() {
        return Err("该录制没有可烧录的输出视频".to_owned());
    }
    Ok(path.to_path_buf())
}

pub(super) async fn burn_recording_danmaku(
    State(state): State<SharedState>,
    axum::extract::Path(recording_id): axum::extract::Path<i32>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let row = state
        .media
        .live_recorder
        .history_item(recording_id)
        .await?
        .ok_or_else(|| AppError::NotFound("录制历史不存在".into()))?;
    let output = burnable_output_path(row.output_path.as_deref()).map_err(AppError::BadRequest)?;
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
    let (items, task_id, task_guard, initial_task) = {
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
        let initial_task = task_guard.get(&task_id).cloned();
        (items, task_id, task_guard, initial_task)
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
    let db = state.infra.db.clone();
    drop(task_guard);
    if let Some(task) = initial_task {
        crate::models::burn::persist_burn_task(&db, &task_id, &task)
            .await
            .map_err(|error| AppError::Internal(format!("保存直播烧录任务失败: {error}")))?;
    }
    let panic_tasks = burn_tasks.clone();
    let panic_db = db.clone();
    let panic_task_id = task_id.clone();
    let rejected_tasks = burn_tasks.clone();
    let rejected_db = db.clone();
    let rejected_task_id = task_id.clone();
    let accepted = state.infra.background_tasks.spawn_with_panic(
        "live_burn_task",
        async move {
            let Ok(_permit) = burn_semaphore.acquire_owned().await else {
                let mut tasks = burn_tasks.lock().await;
                if let Some(task) = tasks.get_mut(&task_id) {
                    task.status = "failed".to_string();
                    task.message = "获取烧录并发槽失败".to_string();
                    task.updated_at = chrono::Utc::now().timestamp();
                }
                drop(tasks);
                persist_live_burn_snapshot(&db, &burn_tasks, &task_id).await;
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
            persist_live_burn_snapshot(&db, &burn_tasks, &task_id).await;
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
            drop(tasks);
            persist_live_burn_snapshot(&db, &burn_tasks, &task_id).await;
        },
        move || async move {
            let mut tasks = panic_tasks.lock().await;
            if let Some(task) = tasks.get_mut(&panic_task_id) {
                task.status = "failed".to_string();
                task.message = "直播烧录任务异常终止".to_string();
                task.updated_at = chrono::Utc::now().timestamp();
            }
            drop(tasks);
            persist_live_burn_snapshot(&panic_db, &panic_tasks, &panic_task_id).await;
        },
    );
    if !accepted {
        let mut tasks = rejected_tasks.lock().await;
        if let Some(task) = tasks.get_mut(&rejected_task_id) {
            task.status = "failed".to_string();
            task.message = "应用正在关闭，直播烧录任务未启动".to_string();
            task.updated_at = chrono::Utc::now().timestamp();
        }
        drop(tasks);
        persist_live_burn_snapshot(&rejected_db, &rejected_tasks, &rejected_task_id).await;
        return Err(AppError::Internal(
            "应用正在关闭，无法启动直播烧录任务".to_string(),
        ));
    }
    Ok(Json(ApiResponse::with_message(
        json!({"task_id": response_task_id, "status": "queued"}),
        "烧录任务已排队，完成后会生成带弹幕的版本",
    )))
}

/// 从直播互动 JSONL 归档提取可烧录条目：弹幕保留归档中的模式、字号和颜色，SC 走顶部固定轨道。
///
/// 逐行流式解析，不整文件读入内存：长场次的 `events.jsonl` 可达数百 MB，
/// `read_to_string` 会在烧录瞬间打出一个与录制时长成正比的内存尖峰（且遇到
/// 非 UTF-8 字节直接整体失败，而现在只跳过坏行）。
async fn load_live_burn_items(events_path: &Path) -> anyhow::Result<Vec<DanmakuItem>> {
    const MAX_BURN_ITEMS: usize = 200_000;
    let file = tokio::fs::File::open(events_path).await?;
    let mut reader = BufReader::new(file);
    let mut raw: Vec<u8> = Vec::new();
    let mut items = Vec::new();
    loop {
        raw.clear();
        // read_until 而不是 lines()：后者会整条流地校验 UTF-8，一个坏字节就报废全场归档
        if reader.read_until(b'\n', &mut raw).await? == 0 {
            break;
        }
        let Ok(line) = std::str::from_utf8(&raw) else {
            continue;
        };
        if let Some(item) = live_burn_item_from_jsonl_line(line) {
            items.push(item);
            if items.len() >= MAX_BURN_ITEMS {
                break;
            }
        }
    }
    Ok(items)
}

/// 解析单行事件；非 JSON、空行与不可烧录的事件类型返回 `None`（跳过而非中断）。
fn live_burn_item_from_jsonl_line(line: &str) -> Option<DanmakuItem> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value = serde_json::from_str::<Value>(line).ok()?;
    let time_secs = value
        .get("media_time_ms")
        .and_then(Value::as_i64)
        .unwrap_or(0) as f64
        / 1000.0;
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    match value.get("event_type").and_then(Value::as_str) {
        Some("danmaku") => {
            let text = data
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                return None;
            }
            let source_mode = data.get("mode").and_then(live_json_i64).unwrap_or(1);
            let (mode, bottom) = live_danmaku_mode(source_mode);
            let size = data
                .get("font_size")
                .and_then(live_json_i64)
                .unwrap_or(25)
                .clamp(1, 100) as i32;
            let color = live_danmaku_color(data.get("color"));
            Some(DanmakuItem {
                text,
                time: time_secs,
                mode: mode.to_string(),
                size,
                color,
                bottom,
            })
        }
        Some("super_chat") => {
            let uname = data.get("uname").and_then(Value::as_str).unwrap_or("匿名");
            let message = data.get("message").and_then(Value::as_str).unwrap_or("");
            let price = data.get("price").and_then(Value::as_i64).unwrap_or(0);
            Some(DanmakuItem {
                text: format!("SC ¥{price} {uname}: {message}"),
                time: time_secs,
                mode: "TOP".to_string(),
                size: 30,
                color: "FFB300".to_string(),
                bottom: false,
            })
        }
        _ => None,
    }
}

fn live_json_i64(value: &Value) -> Option<i64> {
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

fn live_danmaku_color(value: Option<&Value>) -> String {
    let color = value.and_then(live_json_i64).unwrap_or(0xFFFFFF) & 0xFFFFFF;
    format!("{color:06X}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 测试临时目录清理属 best-effort：失败仅残留临时文件
        tokio::fs::remove_dir_all(&dir).await.ok();
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

    #[tokio::test]
    async fn live_burn_items_skip_unparsable_lines_without_losing_the_rest() {
        // 归档里混入坏行（截断 JSON / 非法 UTF-8）时：整文件 read_to_string 会直接
        // 报 "stream did not contain valid UTF-8"，一场好归档就此烧不了；
        // 逐行读取只跳过坏行。
        let dir =
            std::env::temp_dir().join(format!("live-burn-utf8-{}", uuid::Uuid::new_v4().simple()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("events.jsonl");
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(b"{\"seq\":1,\"broken\n");
        bytes.extend_from_slice(&[0xFF, 0xFE, b'\n']);
        let good = r#"{"seq":2,"media_time_ms":2500,"event_type":"danmaku","data":{"text":"还在","mode":1}}"#;
        bytes.extend_from_slice(good.as_bytes());
        bytes.push(b'\n');
        tokio::fs::write(&path, &bytes).await.unwrap();
        let items = load_live_burn_items(&path)
            .await
            .expect("坏行不应让整场归档不可用");
        // 测试临时目录清理属 best-effort：失败仅残留临时文件
        tokio::fs::remove_dir_all(&dir).await.ok();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "还在");
        assert!((items[0].time - 2.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn burn_gate_only_accepts_merged_output() {
        let dir =
            std::env::temp_dir().join(format!("live-burn-gate-{}", uuid::Uuid::new_v4().simple()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // 合并失败时落库的分段路径：文件真实存在，但不是整场成品。
        let segment = dir.join("room_segment_0002.flv");
        tokio::fs::write(&segment, b"segment").await.unwrap();
        let unverified = dir.join("room_segment_0001.mp4.unverified");
        tokio::fs::write(&unverified, b"salvage").await.unwrap();
        let merged = dir.join("room_segment_0001.mp4");
        tokio::fs::write(&merged, b"merged").await.unwrap();
        let missing = dir.join("room_segment_0009.mp4");

        assert!(burnable_output_path(None)
            .unwrap_err()
            .contains("没有可烧录的输出视频"));
        assert!(burnable_output_path(segment.to_str())
            .unwrap_err()
            .contains("尚未合并"));
        assert!(burnable_output_path(unverified.to_str())
            .unwrap_err()
            .contains("尚未合并"));
        assert!(burnable_output_path(missing.to_str())
            .unwrap_err()
            .contains("没有可烧录的输出视频"));
        assert_eq!(burnable_output_path(merged.to_str()).unwrap(), merged);
        // 测试临时目录清理属 best-effort：失败仅残留临时文件
        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
