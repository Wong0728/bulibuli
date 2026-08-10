//! 烧录任务：弹幕/字幕烧录任务启动、视频路径解析与烧录状态查询。

use crate::error::{ApiResponse, AppError};
use crate::services::subtitle_burner::SubtitleBurner;
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use crate::state::media::{BurnTask, MediaState};
use crate::state::SharedState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::error;
use uuid::Uuid;

#[derive(Deserialize)]
pub(super) struct BurnRequest {
    bvid: String,
    source: String,
    video_path: Option<String>,
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

    let video_path = resolve_burn_video_path(infra, business, bvid, video_path).await?;
    if !video_path.exists() {
        return Err(AppError::NotFound(format!(
            "视频文件不存在: {}",
            video_path.display()
        )));
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

    {
        let mut tasks = burn_tasks.lock().await;
        tasks.insert(
            task_id.clone(),
            BurnTask {
                bvid: bvid_string.clone(),
                status: "queued".to_string(),
                message: "烧录任务已排队".to_string(),
                output_path: None,
            },
        );
    }

    let task_id_for_spawn = task_id.clone();
    tokio::spawn(async move {
        let Ok(_permit) = burn_semaphore.acquire_owned().await else {
            let mut tasks = burn_tasks.lock().await;
            if let Some(task) = tasks.get_mut(&task_id_for_spawn) {
                task.status = "failed".to_string();
                task.message = "烧录队列已关闭".to_string();
            }
            return;
        };
        {
            let mut tasks = burn_tasks.lock().await;
            if let Some(t) = tasks.get_mut(&task_id_for_spawn) {
                t.status = "processing".to_string();
                t.message = "正在烧录，请稍候...".to_string();
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
                    t.message = message;
                    t.output_path = output_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string());
                }
                drop(tasks);

                if success {
                    if let Err(e) = history_service
                        .mark_burned(&bvid_string, &source_for_spawn, output_path.as_deref())
                        .await
                    {
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
                    t.message = format!("烧录出错: {e}");
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
    business: &BusinessState,
    bvid: &str,
    video_path: Option<String>,
) -> Result<std::path::PathBuf, AppError> {
    if let Some(p) = video_path.filter(|p| !p.is_empty()) {
        // 安全校验：用户指定的路径必须位于 download_dir 之下，防止路径遍历
        let user_path = std::path::PathBuf::from(&p);
        let canonical = std::fs::canonicalize(&user_path)
            .map_err(|_| AppError::BadRequest(format!("视频路径无效或不存在: {p}")))?;
        let download_dir_canonical = std::fs::canonicalize(&infra.paths.download_dir)
            .unwrap_or_else(|_| infra.paths.download_dir.clone());
        if !canonical.starts_with(&download_dir_canonical) {
            return Err(AppError::BadRequest(
                "视频路径不在允许的下载目录范围内".to_string(),
            ));
        }
        return Ok(canonical);
    }
    let h = business.history_service.find_by_bvid(bvid).await?;
    if let Some(h) = h {
        if let Some(fp) = h.file_path {
            if std::path::Path::new(&fp).exists() {
                return Ok(std::path::PathBuf::from(fp));
            }
        }
    }
    find_video_file(&infra.paths.download_dir, bvid)
        .ok_or_else(|| AppError::NotFound(format!("未找到视频文件，BV号: {bvid}")))
}

pub(super) async fn burn_status(
    State(state): State<SharedState>,
    axum::extract::Path(task_id): axum::extract::Path<String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let tasks = state.media.burn_tasks.lock().await;
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

fn find_video_file(dir: &std::path::Path, bvid: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name()?.to_string_lossy();
            if name.contains(bvid)
                && ["mp4", "mkv", "flv", "avi", "mov", "webm"]
                    .iter()
                    .any(|ext| name.ends_with(ext))
            {
                return Some(path);
            }
        } else if path.is_dir() {
            if let Some(found) = find_video_file(&path, bvid) {
                return Some(found);
            }
        }
    }
    None
}
