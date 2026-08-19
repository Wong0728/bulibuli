//! 看板视图：按博主分组的看板响应与单视频详情（抽屉）响应组装。

use crate::error::{ApiResponse, AppError};
use crate::models::{blogger, download_task, history};
use crate::services::auth::ClientInfo;
use crate::services::security_config::can_open_directory;
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use crate::state::SharedState;
use axum::{extract::Query, extract::State, Extension, Json};
use chrono::Local;
use futures::{stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
pub(super) struct ListQuery {
    /// `downloading` / `completed` / `failed`。
    tab: Option<String>,
    /// 单视频详情查询（抽屉用）。
    bvid: Option<String>,
    /// 多 P 详情的精确 history 主键；缺省时兼容按 bvid 取最新记录。
    history_id: Option<i32>,
    page: Option<u64>,
    page_size: Option<u64>,
}

pub(super) async fn list_history(
    State(state): State<SharedState>,
    Extension(client): Extension<ClientInfo>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let server_time = Local::now().timestamp();

    // 单视频详情查询：返回该 bvid 的 history、sidecar 和最新 download_task 状态。
    if let Some(bvid) = q.bvid.as_deref() {
        let can_open_directory = can_open_directory(&state.bili.security.current().mode, client.ip);
        let data = build_single_video_response(
            &state.business,
            &state.infra,
            state.media.video_processor.clone(),
            bvid.trim(),
            q.history_id,
            server_time,
            can_open_directory,
        )
        .await?;
        return Ok(Json(ApiResponse::success(data)));
    }

    let tab = q.tab.as_deref().unwrap_or("completed");
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(50).clamp(1, 50);
    let can_open_directory = can_open_directory(&state.bili.security.current().mode, client.ip);
    let board = build_board_response(
        &state.business,
        &state.infra,
        tab,
        server_time,
        page,
        page_size,
        can_open_directory,
    )
    .await?;
    Ok(Json(ApiResponse::success(board)))
}

/// 构建看板分组响应。
async fn build_board_response(
    business: &BusinessState,
    infra: &InfraState,
    tab: &str,
    server_time: i64,
    page: u64,
    page_size: u64,
    can_open_directory: bool,
) -> Result<Value, AppError> {
    let board_page = business
        .history_service
        .board_page(tab, page, page_size)
        .await?;
    let histories = board_page.histories;
    let total = board_page.total;
    let page_bvids = histories
        .iter()
        .map(|history| history.bvid.clone())
        .collect::<Vec<_>>();
    let page_uids = histories
        .iter()
        .filter_map(|history| history.uid.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    // 当前页需要的关联数据均按键批量查询，不读取完整表。
    let bloggers = business
        .blogger_service
        .find_many_by_uids(&page_uids)
        .await?;
    let blogger_map: HashMap<String, blogger::Model> = bloggers
        .iter()
        .map(|b| (b.uid.clone(), b.clone()))
        .collect();
    let tasks = business
        .history_service
        .download_tasks_for_bvids(&page_bvids)
        .await?;
    // 按 bvid 聚合：一个 bvid 可能有多条 task（video/audio）
    let mut task_by_bvid: HashMap<String, Vec<download_task::Model>> = HashMap::new();
    for t in &tasks {
        task_by_bvid
            .entry(t.bvid.clone())
            .or_default()
            .push(t.clone());
    }
    let settings = infra.settings_service.current();
    let configured_path_display_mode =
        if settings.board.path_display_mode == "hidden" && settings.board.show_relative_path {
            "relative".to_string()
        } else {
            settings.board.path_display_mode.clone()
        };
    let path_display_mode = if configured_path_display_mode == "absolute" && !can_open_directory {
        "relative".to_string()
    } else {
        configured_path_display_mode
    };

    // 按 UID 分组。
    let mut groups: HashMap<String, Vec<history::Model>> = HashMap::new();
    for h in histories {
        let uid = h.uid.clone().unwrap_or_else(|| "unknown".to_string());
        groups.entry(uid).or_default().push(h);
    }

    // 7. 组装响应
    let mut result_groups: Vec<Value> = Vec::new();
    // 遍历所有博主，确保即使没有视频也显示（仅当有 history 时）
    for (uid, videos) in &groups {
        let b = blogger_map.get(uid);
        let counts = board_page
            .counts_by_uid
            .get(uid)
            .map(Counts::from)
            .unwrap_or_default();
        // 未监控博主（bloggers 表查不到，如手动下载的 UP 主）：用 history 里的 owner 快照兜底
        let fallback_name = videos.iter().find_map(|v| v.owner_name.clone());
        let fallback_face = videos.iter().find_map(|v| v.owner_face.clone());
        let video_list = build_video_list(
            business,
            videos,
            &task_by_bvid,
            &path_display_mode,
            can_open_directory,
        )
        .await;
        result_groups.push(json!({
            "uid": uid,
            "name": b.and_then(|b| b.name.clone()).or(fallback_name),
            "face": b.and_then(|b| b.face.clone()).or(fallback_face),
            "last_seen_name": b.and_then(|b| b.last_seen_name.clone()),
            "last_seen_face": b.and_then(|b| b.last_seen_face.clone()),
            "last_seen_at": b.and_then(|b| b.last_seen_at.map(|t| t.to_rfc3339())),
            "notice_visible": b.and_then(|b| b.last_seen_at).is_some(),
            "counts": counts,
            "videos": video_list,
        }));
    }

    // 按 UID 排序，保证结果稳定。
    result_groups.sort_by(|a, b| {
        a["uid"]
            .as_str()
            .unwrap_or("")
            .cmp(b["uid"].as_str().unwrap_or(""))
    });

    // 全局 counts（跨所有博主求和）：前端三个子 tab 徽章直接用它，
    // 从全量记录汇总，保证空 Tab 不会把其他状态的计数清零。
    let mut global_counts = Counts::default();
    for c in board_page.counts_by_uid.values() {
        global_counts.downloading += c.downloading;
        global_counts.completed += c.completed;
        global_counts.failed += c.failed;
        global_counts.removed += c.removed;
        global_counts.pay_blocked += c.pay_blocked;
    }

    Ok(json!({
        "server_time": server_time,
        "tab": tab,
        "counts": global_counts,
        "page": page,
        "page_size": page_size,
        "total": total,
        "items": result_groups,
    }))
}

/// 构建视频列表（含 sidecar 状态 + 下载进度）。
async fn build_video_list(
    business: &BusinessState,
    videos: &[history::Model],
    task_by_bvid: &HashMap<String, Vec<download_task::Model>>,
    path_display_mode: &str,
    can_open_directory: bool,
) -> Vec<Value> {
    stream::iter(videos.iter().cloned())
        .map(|h| async move {
            let sidecar = business
                .history_service
                .sidecar_status(&h.bvid, h.uid.as_deref(), h.file_path.as_deref())
                .await;
            let filepath = display_path_for(
                &business.history_service,
                h.file_path.as_deref(),
                path_display_mode,
            );

            let matching_tasks = task_by_bvid.get(&h.bvid).map(|tasks| {
                tasks
                    .iter()
                    .filter(|task| task.cid == h.cid)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let task = aggregate_task_progress(matching_tasks.as_deref(), &h);

            json!({
                "bvid": h.bvid,
                "history_id": h.id,
                "cid": h.cid,
                "page": h.page,
                "part_title": h.part_title,
                "title": h.title,
                "source": h.source,
                "pub_timestamp": h.pub_timestamp,
                "pub_date": h.pub_date,
                "pic": h.pic,
                "duration": h.duration,
                "view": h.view,
                "state": h.state,
                "cover_local_path": display_path_for(&business.history_service, h.cover_local_path.as_deref(), path_display_mode),
                "file_path": filepath,
                "relative_path": h.file_path.as_deref().and_then(|path| business.history_service.to_relative_path(path)),
                "can_open_directory": can_open_directory,
                "reupload_of": h.reupload_of,
                "pay_note": h.pay_note,
                "md5": h.md5,
                "sha256": h.sha256,
                "download_time": h.download_time.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
                "view_refreshed_at": h.view_refreshed_at.map(|t| t.to_rfc3339()),
                "sidecar": sidecar,
                "burned": {
                    "danmaku": h.burned_danmaku.unwrap_or(false),
                    "subtitle": h.burned_subtitle.unwrap_or(false),
                },
                "task": {
                    "progress_percent": task.progress,
                    "speed": task.speed,
                    "status": task.status,
                    "downloaded_size": task.downloaded_size,
                    "total_size": task.total_size,
                    "task_id": task.task_id,
                    "priority": task.priority,
                },
                // 失败原因冒泡：仅在 failed/merge_failed 时填充，前端用来解释「为什么没下成功」。
                // 其他状态下为 null，前端可用 `if (v.failure)` 显式判断。
                "failure": if task.status == "failed" || task.status == "merge_failed" {
                    json!({
                        "message": task.error,
                        "kind": task.error_kind,
                        "fallback_reason": task.fallback_reason,
                    })
                } else {
                    Value::Null
                },
            })
        })
        .buffered(4)
        .collect()
        .await
}

/// 单视频详情响应（抽屉用）。
async fn build_single_video_response(
    business: &BusinessState,
    infra: &InfraState,
    video_processor: std::sync::Arc<crate::services::video_processor::VideoProcessor>,
    bvid: &str,
    history_id: Option<i32>,
    server_time: i64,
    can_open_directory: bool,
) -> Result<Value, AppError> {
    // DB 错误（锁超时/磁盘故障）应返回 500 而非混入 404，避免排障被误导。
    let h = match history_id {
        Some(id) => business
            .history_service
            .find_by_id(id)
            .await
            .map(|value| value.filter(|history| history.bvid == bvid))?,
        None => business.history_service.find_by_bvid(bvid).await?,
    };
    let Some(h) = h else {
        return Err(AppError::NotFound("未找到该视频记录".to_string()));
    };

    let sidecar = business
        .history_service
        .sidecar_status(&h.bvid, h.uid.as_deref(), h.file_path.as_deref())
        .await;

    // 查询博主信息；未监控博主时使用 history 的 owner 快照兜底。
    let blogger_info = if let Some(uid) = h.uid.as_deref() {
        business
            .blogger_service
            .find_by_uid(uid)
            .await
            .ok()
            .flatten()
            .map(|b| {
                json!({
                    "uid": b.uid,
                    "name": b.name,
                    "face": b.face,
                    "sign": b.sign,
                    "level": b.level,
                    "last_seen_name": b.last_seen_name,
                    "last_seen_face": b.last_seen_face,
                    "last_seen_at": b.last_seen_at.map(|t| t.to_rfc3339()),
                    "notice_visible": b.last_seen_at.is_some(),
                })
            })
            .or_else(|| {
                (h.owner_name.is_some() || h.owner_face.is_some()).then(|| {
                    json!({
                        "uid": uid,
                        "name": h.owner_name.clone(),
                        "face": h.owner_face.clone(),
                        "notice_visible": false,
                    })
                })
            })
    } else {
        None
    };

    // 查最新 download_task
    let tasks = business
        .history_service
        .download_tasks_for_history(&h)
        .await;
    let task = aggregate_task_progress(Some(&tasks), &h);
    let task_info = json!({
        "progress_percent": task.progress,
        "speed": task.speed,
        "status": task.status,
        "downloaded_size": task.downloaded_size,
        "total_size": task.total_size,
        "task_id": task.task_id,
        "priority": task.priority,
        // 失败元数据：失败原因冒泡到抽屉，前端可展示「为什么失败」详情。
        "error": task.error,
        "error_kind": task.error_kind,
        "fallback_reason": task.fallback_reason,
    });

    let files = business
        .history_service
        .scan_files(&h.bvid, h.uid.as_deref(), h.file_path.as_deref())
        .await;

    let settings = infra.settings_service.current();
    let can_browser_download = settings.board.browser_download_enabled;
    // 烧录能力：探测当前 FFmpeg 是否含 ass 滤镜与视频编码器（结果带缓存），
    // 不支持时抽屉会把烧录按钮置灰，避免点击后才失败。
    let custom_ffmpeg = {
        let path = settings.ffmpeg.custom_path.trim().to_string();
        (!path.is_empty()).then_some(path)
    };
    let can_burn = crate::services::subtitle_burner::ffmpeg_supports_burn(
        video_processor,
        settings.ffmpeg.mode.as_str(),
        custom_ffmpeg.as_deref(),
    )
    .await;
    let configured_path_display_mode =
        if settings.board.path_display_mode == "hidden" && settings.board.show_relative_path {
            "relative".to_string()
        } else {
            settings.board.path_display_mode.clone()
        };
    let path_display_mode = if configured_path_display_mode == "absolute" && !can_open_directory {
        "relative".to_string()
    } else {
        configured_path_display_mode
    };
    let file_path = display_path_for(
        &business.history_service,
        h.file_path.as_deref(),
        &path_display_mode,
    );
    let cover_local_path = display_path_for(
        &business.history_service,
        h.cover_local_path.as_deref(),
        &path_display_mode,
    );
    let relative_path = h
        .file_path
        .as_deref()
        .and_then(|path| business.history_service.to_relative_path(path));
    let files = files
        .into_iter()
        .map(|file| file_entry_view(&business.history_service, file, &path_display_mode))
        .collect::<Vec<_>>();

    Ok(json!({
        "server_time": server_time,
        "video": {
            "bvid": h.bvid,
            "history_id": h.id,
            "cid": h.cid,
            "page": h.page,
            "part_title": h.part_title,
            "title": h.title,
            "source": h.source,
            "pub_timestamp": h.pub_timestamp,
            "pub_date": h.pub_date,
            "pic": h.pic,
            "duration": h.duration,
            "view": h.view,
            "state": h.state,
            "cover_local_path": cover_local_path,
            "file_path": file_path,
            "relative_path": relative_path,
            "can_open_directory": can_open_directory,
            "can_browser_download": can_browser_download,
            "can_burn": can_burn,
            "reupload_of": h.reupload_of,
            "pay_note": h.pay_note,
            "md5": h.md5,
            "md5_last_checked_at": h.md5_last_checked_at.map(|t| t.to_rfc3339()),
            "sha256": h.sha256,
            "sha256_last_checked_at": h.sha256_last_checked_at.map(|t| t.to_rfc3339()),
            "download_time": h.download_time.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            "view_refreshed_at": h.view_refreshed_at.map(|t| t.to_rfc3339()),
            "sidecar": sidecar,
            "task": task_info,
            "blogger": blogger_info,
            "files": files,
            "burned": {
                "danmaku": h.burned_danmaku.unwrap_or(false),
                "subtitle": h.burned_subtitle.unwrap_or(false),
            },
        }
    }))
}

fn display_path_for(
    history_service: &crate::services::history::HistoryService,
    path: Option<&str>,
    mode: &str,
) -> Option<String> {
    let relative = path.and_then(|value| history_service.to_relative_path(value))?;
    match mode {
        "relative" => Some(relative),
        "absolute" => history_service
            .resolve_download_relative_path(&relative)
            .map(|value| value.to_string_lossy().replace('\\', "/")),
        _ => None,
    }
}

fn file_entry_view(
    history_service: &crate::services::history::HistoryService,
    file: crate::services::history::FileEntry,
    mode: &str,
) -> Value {
    let display_path = display_path_for(history_service, Some(&file.path), mode);
    json!({
        "file_type": file.file_type,
        "name": file.name,
        "path": file.path,
        "display_path": display_path,
        "size": file.size,
        "format": file.format,
        "location": file.location,
        "is_current": file.is_current,
        "version": file.version,
        "modified_at": file.modified_at,
    })
}

#[derive(Clone, Default, serde::Serialize)]
struct Counts {
    downloading: i64,
    completed: i64,
    failed: i64,
    removed: i64,
    pay_blocked: i64,
}

impl From<&crate::services::history::HistoryCounts> for Counts {
    fn from(value: &crate::services::history::HistoryCounts) -> Self {
        Self {
            downloading: value.downloading,
            completed: value.completed,
            failed: value.failed,
            removed: value.removed,
            pay_blocked: value.pay_blocked,
        }
    }
}

struct AggregatedTaskProgress {
    progress: i32,
    speed: i64,
    status: String,
    downloaded_size: i64,
    total_size: i64,
    /// 当前聚合状态对应的任务 ID，供前端 pause/resume 等单任务操作使用。
    /// 多任务时优先返回 downloading/paused 任务，便于暂停按钮定位到活跃任务。
    task_id: Option<i32>,
    /// 代表任务的下载优先级（1..=300，默认 100），供前端调整控件显示。
    priority: i32,
    /// 失败原因（来自代表任务的 `error` 字段）。仅在状态为 failed/merge_failed 时有值。
    error: Option<String>,
    /// 结构化错误分类（如 `Paywall`、`PermissionDenied`），供前端映射为友好中文。
    error_kind: Option<String>,
    /// 画质/编码降级原因（如「大会员不可用，已降级 1080P」），供用户排查。
    fallback_reason: Option<String>,
}

/// 统一看板与抽屉的任务聚合优先级：下载中 > 已暂停 > 等待 > 失败 > 终态。
/// paused 单独列出（优先级介于 downloading 与 pending 之间），避免被误判为 completed。
fn aggregate_task_progress(
    tasks: Option<&[download_task::Model]>,
    history: &history::Model,
) -> AggregatedTaskProgress {
    let Some(tasks) = tasks.filter(|tasks| !tasks.is_empty()) else {
        return AggregatedTaskProgress {
            progress: 100,
            speed: 0,
            status: history
                .state
                .clone()
                .unwrap_or_else(|| "completed".to_string()),
            downloaded_size: 0,
            total_size: 0,
            task_id: None,
            priority: 100,
            error: None,
            error_kind: None,
            fallback_reason: None,
        };
    };
    // 状态优先级：downloading > paused > pending > failed > completed
    let status = ["downloading", "paused", "pending", "failed"]
        .into_iter()
        .find(|candidate| tasks.iter().any(|task| task.status == *candidate))
        .unwrap_or("completed")
        .to_string();
    // 取该状态下的第一个任务作为代表（前端 pause/resume/priority 据此定位单任务）
    let representative = tasks.iter().find(|task| task.status == status);
    let task_id = representative.map(|task| task.id);
    let priority = representative.map(|task| task.priority).unwrap_or(100);
    // 失败元数据：仅在 failed/merge_failed 时把数据库里的 error 字段冒泡给前端，
    // 让用户看到「为什么会失败」（B站风控/权限/网络/账号失效等）。
    let error_meta = representative.and_then(|task| {
        if status == "failed" || status == "merge_failed" {
            Some((
                task.error.clone(),
                task.error_kind.clone(),
                task.fallback_reason.clone(),
            ))
        } else {
            None
        }
    });
    AggregatedTaskProgress {
        progress: tasks
            .iter()
            .map(|task| task.progress_percent)
            .max()
            .unwrap_or(0),
        speed: tasks.iter().map(|task| task.speed).sum(),
        status,
        downloaded_size: tasks.iter().map(|task| task.downloaded_size).sum(),
        total_size: tasks.iter().map(|task| task.total_size).sum(),
        task_id,
        priority,
        error: error_meta.as_ref().and_then(|(e, _, _)| e.clone()),
        error_kind: error_meta.as_ref().and_then(|(_, k, _)| k.clone()),
        fallback_reason: error_meta.as_ref().and_then(|(_, _, r)| r.clone()),
    }
}
