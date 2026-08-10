//! 看板视图：按博主分组的看板响应与单视频详情（抽屉）响应组装。

use crate::error::{ApiResponse, AppError};
use crate::models::{blogger, download_task, history};
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use crate::state::SharedState;
use axum::{extract::Query, extract::State, Json};
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
    page: Option<u64>,
    page_size: Option<u64>,
}

pub(super) async fn list_history(
    State(state): State<SharedState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let server_time = Local::now().timestamp();

    // 单视频详情查询：返回该 bvid 的 history + sidecar + 最新 download_task 状态
    if let Some(bvid) = q.bvid.as_deref() {
        let data = build_single_video_response(&state.business, bvid.trim(), server_time).await?;
        return Ok(Json(ApiResponse::success(data)));
    }

    let tab = q.tab.as_deref().unwrap_or("completed");
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(50).clamp(1, 50);
    let board = build_board_response(
        &state.business,
        &state.infra,
        tab,
        server_time,
        page,
        page_size,
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
    let show_relative_path = infra.settings_service.current().board.show_relative_path;

    // 按 uid 分组
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
        let video_list =
            build_video_list(business, videos, &task_by_bvid, show_relative_path).await;
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

    // 按 uid 排序，保证稳定
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
    show_relative_path: bool,
) -> Vec<Value> {
    stream::iter(videos.iter().cloned())
        .map(|h| async move {
            let sidecar = business
                .history_service
                .sidecar_status(&h.bvid, h.uid.as_deref(), h.file_path.as_deref())
                .await;
            let filepath = if show_relative_path {
                h.file_path
                    .as_deref()
                    .map(|p| business.history_service.to_relative_path(p))
            } else {
                None
            };

            let task = aggregate_task_progress(task_by_bvid.get(&h.bvid).map(Vec::as_slice), &h);

            json!({
                "bvid": h.bvid,
                "title": h.title,
                "source": h.source,
                "pub_timestamp": h.pub_timestamp,
                "pub_date": h.pub_date,
                "pic": h.pic,
                "duration": h.duration,
                "view": h.view,
                "state": h.state,
                "cover_local_path": h.cover_local_path,
                "file_path": filepath,
                "reupload_of": h.reupload_of,
                "pay_note": h.pay_note,
                "md5": h.md5,
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
    bvid: &str,
    server_time: i64,
) -> Result<Value, AppError> {
    let h = business.history_service.find_by_bvid(bvid).await;
    let Ok(Some(h)) = h else {
        return Err(AppError::NotFound("未找到该视频记录".to_string()));
    };

    let sidecar = business
        .history_service
        .sidecar_status(&h.bvid, h.uid.as_deref(), h.file_path.as_deref())
        .await;

    // 查博主信息（未监控博主时用 history 的 owner 快照兜底）
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
    let tasks = business.history_service.download_tasks_for_bvid(bvid).await;
    let task = aggregate_task_progress(Some(&tasks), &h);
    let task_info = json!({
        "progress_percent": task.progress,
        "speed": task.speed,
        "status": task.status,
        "downloaded_size": task.downloaded_size,
        "total_size": task.total_size,
        "task_id": task.task_id,
    });

    let files = business
        .history_service
        .scan_files(&h.bvid, h.uid.as_deref(), h.file_path.as_deref())
        .await;

    Ok(json!({
        "server_time": server_time,
        "video": {
            "bvid": h.bvid,
            "title": h.title,
            "source": h.source,
            "pub_timestamp": h.pub_timestamp,
            "pub_date": h.pub_date,
            "pic": h.pic,
            "duration": h.duration,
            "view": h.view,
            "state": h.state,
            "cover_local_path": h.cover_local_path,
            "file_path": h.file_path,
            "reupload_of": h.reupload_of,
            "pay_note": h.pay_note,
            "md5": h.md5,
            "md5_last_checked_at": h.md5_last_checked_at.map(|t| t.to_rfc3339()),
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
        };
    };
    // 状态优先级：downloading > paused > pending > failed > completed
    let status = ["downloading", "paused", "pending", "failed"]
        .into_iter()
        .find(|candidate| tasks.iter().any(|task| task.status == *candidate))
        .unwrap_or("completed")
        .to_string();
    // 取该状态下的第一个任务 id 作为代表（前端 pause/resume 据此定位单任务）
    let task_id = tasks
        .iter()
        .find(|task| task.status == status)
        .map(|task| task.id);
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
    }
}
