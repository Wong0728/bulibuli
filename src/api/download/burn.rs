//! 烧录任务：弹幕/字幕烧录任务启动、视频路径解析与烧录状态查询。

use crate::error::{ApiResponse, AppError};
use crate::models::burn::BurnTask;
use crate::services::subtitle_burner::SubtitleBurner;
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use crate::state::media::MediaState;
use crate::state::SharedState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::error;
use uuid::Uuid;

const BURN_TASK_TTL_SECONDS: i64 = 60 * 60;
const MAX_BURN_TASKS: usize = 200;

#[derive(Deserialize)]
pub(super) struct BurnRequest {
    bvid: String,
    source: String,
    video_path: Option<String>,
    history_id: Option<i32>,
}

pub(super) async fn burn(
    State(state): State<SharedState>,
    Json(req): Json<BurnRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    spawn_burn(
        &state.infra,
        &state.media,
        &state.business,
        &req.bvid,
        &req.source,
        req.video_path,
        req.history_id,
    )
    .await
}

/// 烧录来源的中文标签（日志用）。
fn burn_source_label(source: &str) -> &'static str {
    match source {
        "danmaku" => "弹幕",
        "subtitle" => "字幕",
        _ => "弹幕+字幕",
    }
}

async fn spawn_burn(
    infra: &InfraState,
    media: &MediaState,
    business: &BusinessState,
    bvid: &str,
    source: &str,
    video_path: Option<String>,
    history_id: Option<i32>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供BV号".to_string()));
    }
    let source = source.to_lowercase();
    if !matches!(source.as_str(), "danmaku" | "subtitle" | "both") {
        return Err(AppError::BadRequest(
            "source 必须是 danmaku / subtitle / both".to_string(),
        ));
    }

    let history = match history_id {
        Some(id) => business
            .history_service
            .find_by_id(id)
            .await?
            .filter(|history| history.bvid == bvid),
        None => business.history_service.find_by_bvid(bvid).await?,
    };
    let video_path =
        resolve_burn_video_path(infra, business, bvid, video_path, history.as_ref()).await?;
    if !video_path.exists() {
        return Err(AppError::NotFound("视频文件不存在".to_string()));
    }

    let task_id = Uuid::new_v4()
        .to_string()
        .chars()
        .take(8)
        .collect::<String>();
    let custom_ffmpeg = {
        let settings = infra.settings_service.current();
        let path = settings.ffmpeg.custom_path.trim().to_string();
        (!path.is_empty()).then_some(path)
    };
    // 烧录参数从 settings 读取，未配置时使用 BurnConfig::default()（行为与迭代前一致）。
    let burn_config = infra.settings_service.current().burn.to_burn_config();
    let burner =
        SubtitleBurner::with_burn_config(media.video_processor.clone(), custom_ffmpeg, burn_config);
    let burn_tasks = media.burn_tasks.clone();
    let burn_semaphore = media.burn_semaphore.clone();
    let history_service = business.history_service.clone();
    let monitor_service = business.monitor_service.clone();
    let bvid_string = bvid.to_string();
    let source_for_spawn = source.clone();
    let history_id_for_spawn = history.as_ref().map(|history| history.id);

    {
        let mut tasks = burn_tasks.lock().await;
        prune_burn_tasks(&mut tasks);
        let now = chrono::Utc::now().timestamp();
        tasks.insert(
            task_id.clone(),
            BurnTask {
                bvid: bvid_string.clone(),
                status: "queued".to_string(),
                message: "烧录任务已排队".to_string(),
                output_path: None,
                created_at: now,
                updated_at: now,
            },
        );
    }

    let task_id_for_spawn = task_id.clone();
    let download_dir = infra.paths.download_dir.clone();
    tokio::spawn(async move {
        let Ok(_permit) = burn_semaphore.acquire_owned().await else {
            let mut tasks = burn_tasks.lock().await;
            if let Some(task) = tasks.get_mut(&task_id_for_spawn) {
                task.status = "failed".to_string();
                task.message = "烧录队列已关闭".to_string();
                task.updated_at = chrono::Utc::now().timestamp();
            }
            return;
        };
        {
            let mut tasks = burn_tasks.lock().await;
            if let Some(t) = tasks.get_mut(&task_id_for_spawn) {
                t.status = "processing".to_string();
                t.message = "正在烧录，请稍候...".to_string();
                t.updated_at = chrono::Utc::now().timestamp();
            }
        }
        monitor_service
            .add_log(
                None,
                Some(&bvid_string),
                &format!("开始烧录（{}）", burn_source_label(&source_for_spawn)),
                "info",
            )
            .await;

        let result = match source_for_spawn.as_str() {
            "danmaku" => burner.burn_danmaku(&video_path).await,
            "subtitle" => burner.burn_subtitle(&video_path).await,
            _ => burner.burn_mixed(&video_path).await,
        };

        match result {
            Ok((success, output_path, message)) => {
                let mut tasks = burn_tasks.lock().await;
                if let Some(t) = tasks.get_mut(&task_id_for_spawn) {
                    t.status = if success {
                        "completed".to_string()
                    } else {
                        "failed".to_string()
                    };
                    t.message = redact_burn_message(&message);
                    t.output_path = output_path
                        .as_ref()
                        .and_then(|p| safe_relative_path(&download_dir, p));
                    t.updated_at = chrono::Utc::now().timestamp();
                }
                drop(tasks);

                if success {
                    let result = match history_id_for_spawn {
                        Some(id) => {
                            history_service
                                .mark_burned_by_id(id, &source_for_spawn, output_path.as_deref())
                                .await
                        }
                        None => {
                            history_service
                                .mark_burned(
                                    &bvid_string,
                                    &source_for_spawn,
                                    output_path.as_deref(),
                                )
                                .await
                        }
                    };
                    if let Err(e) = result {
                        error!("更新历史记录烧录状态失败 {bvid_string}: {e}");
                    }
                    monitor_service
                        .add_log(
                            None,
                            Some(&bvid_string),
                            &format!("烧录完成（{}）", burn_source_label(&source_for_spawn)),
                            "success",
                        )
                        .await;
                } else {
                    monitor_service
                        .add_log(
                            None,
                            Some(&bvid_string),
                            &format!("烧录未完成（{}）", burn_source_label(&source_for_spawn)),
                            "warning",
                        )
                        .await;
                }
            }
            Err(e) => {
                let mut tasks = burn_tasks.lock().await;
                if let Some(t) = tasks.get_mut(&task_id_for_spawn) {
                    t.status = "failed".to_string();
                    t.message = redact_burn_message(&format!("烧录出错: {e}"));
                    t.updated_at = chrono::Utc::now().timestamp();
                }
                drop(tasks);
                monitor_service
                    .add_log(
                        None,
                        Some(&bvid_string),
                        &format!("烧录出错（{}）：{e}", burn_source_label(&source_for_spawn)),
                        "error",
                    )
                    .await;
            }
        }
    });

    Ok(Json(ApiResponse::with_message(
        json!({ "task_id": task_id }),
        "烧录任务已启动",
    )))
}

async fn resolve_burn_video_path(
    infra: &InfraState,
    _business: &BusinessState,
    bvid: &str,
    video_path: Option<String>,
    history: Option<&crate::models::history::Model>,
) -> Result<std::path::PathBuf, AppError> {
    if let Some(p) = video_path.filter(|p| !p.is_empty()) {
        // 安全校验：用户指定的路径必须位于 download_dir 之下，防止路径遍历
        let user_path = std::path::PathBuf::from(&p);
        let canonical = std::fs::canonicalize(&user_path)
            .map_err(|_| AppError::BadRequest("视频路径无效或不存在".to_string()))?;
        let download_dir_canonical = std::fs::canonicalize(&infra.paths.download_dir)
            .unwrap_or_else(|_| infra.paths.download_dir.clone());
        if !canonical.starts_with(&download_dir_canonical) {
            return Err(AppError::BadRequest(
                "视频路径不在允许的下载目录范围内".to_string(),
            ));
        }
        return Ok(canonical);
    }
    if let Some(h) = history {
        if let Some(fp) = h.file_path.as_deref() {
            if let Ok(canonical) = std::fs::canonicalize(fp) {
                let root = std::fs::canonicalize(&infra.paths.download_dir)
                    .unwrap_or_else(|_| infra.paths.download_dir.clone());
                if canonical.starts_with(root) {
                    return Ok(canonical);
                }
            }
        }
    }
    let root = infra.paths.download_dir.clone();
    let bvid = bvid.to_owned();
    tokio::task::spawn_blocking(move || find_video_file(&root, &bvid))
        .await
        .map_err(|_| AppError::Internal("视频文件扫描任务失败".to_string()))?
        .ok_or_else(|| AppError::NotFound("未找到视频文件".to_string()))
}

pub(super) async fn burn_status(
    State(state): State<SharedState>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let mut tasks = state.media.burn_tasks.lock().await;
    prune_burn_tasks(&mut tasks);
    let task = tasks.get(&task_id).cloned();
    drop(tasks);

    match task {
        Some(t) => Ok(Json(ApiResponse::success(json!({
            "task_id": task_id,
            "status": t.status,
            "message": t.message,
            "output_path": t.output_path,
        })))),
        None => Err(AppError::NotFound("任务不存在".to_string())),
    }
}

fn find_video_file(dir: &Path, bvid: &str) -> Option<PathBuf> {
    const MAX_DEPTH: usize = 8;
    const MAX_ENTRIES: usize = 5_000;
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        let entries = std::fs::read_dir(directory).ok()?;
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_ENTRIES {
                return None;
            }
            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_file() {
                let name = path.file_name()?.to_string_lossy();
                if name.contains(bvid)
                    && ["mp4", "mkv", "flv", "avi", "mov", "webm"]
                        .iter()
                        .any(|ext| name.ends_with(ext))
                {
                    return Some(path);
                }
            } else if file_type.is_dir() && depth < MAX_DEPTH {
                pending.push((path, depth + 1));
            }
        }
    }
    None
}

fn safe_relative_path(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

fn redact_burn_message(message: &str) -> String {
    crate::services::live_recorder::ffmpeg_session::redact_diagnostics(message)
}

fn prune_burn_tasks(tasks: &mut HashMap<String, BurnTask>) {
    let now = chrono::Utc::now().timestamp();
    tasks.retain(|_, task| {
        task.status == "queued"
            || task.status == "processing"
            || now.saturating_sub(task.updated_at.max(task.created_at)) <= BURN_TASK_TTL_SECONDS
    });
    if tasks.len() <= MAX_BURN_TASKS {
        return;
    }
    let mut terminal = tasks
        .iter()
        .filter(|(_, task)| !matches!(task.status.as_str(), "queued" | "processing"))
        .map(|(id, task)| (id.clone(), task.updated_at))
        .collect::<Vec<_>>();
    terminal.sort_by_key(|(_, updated_at)| *updated_at);
    for (id, _) in terminal.into_iter().take(tasks.len() - MAX_BURN_TASKS) {
        tasks.remove(&id);
    }
}
