//! 看板列表视图：按博主分组的看板响应组装与路由入口。
//!
//! 按职责拆分的子模块：
//! - `detail`：单视频详情（抽屉）响应组装；
//! - `aggregate`：路径脱敏展示与多任务进度聚合。

mod aggregate;
mod detail;

use crate::error::{ApiResponse, AppError};
use crate::models::{blogger, download_task, history};
use crate::services::auth::ClientInfo;
use crate::services::history::{SidecarProbe, SidecarStatus};
use crate::state::business::BusinessState;
use crate::state::infra::InfraState;
use crate::state::SharedState;
use axum::{extract::Query, extract::State, Extension, Json};
use chrono::Local;
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
    /// `fresh=1` 时绕过看板缓存强制查库（前端收到终态 WS 事件后进入页面用）。
    fresh: Option<bool>,
}

pub(super) async fn list_history(
    State(state): State<SharedState>,
    Extension(client): Extension<ClientInfo>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let server_time = Local::now().timestamp();

    // 单视频详情查询：返回该 bvid 的 history、sidecar 和最新 download_task 状态。
    if let Some(bvid) = q.bvid.as_deref() {
        let can_open_directory = client.can_open_directory(&state.bili.security.current().mode);
        let data = detail::build_single_video_response(
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
    let can_open_directory = client.can_open_directory(&state.bili.security.current().mode);
    let board = build_board_response(
        &state.business,
        &state.infra,
        tab,
        server_time,
        page,
        page_size,
        can_open_directory,
        q.fresh.unwrap_or(false),
    )
    .await?;
    Ok(Json(ApiResponse::success(board)))
}

/// 构建看板分组响应。
#[allow(clippy::too_many_arguments)]
async fn build_board_response(
    business: &BusinessState,
    infra: &InfraState,
    tab: &str,
    server_time: i64,
    page: u64,
    page_size: u64,
    can_open_directory: bool,
    fresh: bool,
) -> Result<Value, AppError> {
    let board_page = business
        .history_service
        .board_page(tab, page, page_size, fresh)
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

    // sidecar 状态按整页批量探测：每个唯一目录只 read_dir 一次，
    // 替代旧版每行最多 8 次逐文件存在性探测（N+1 扇出，列表页线性劣化）。
    let sidecar_probes = groups
        .values()
        .flatten()
        .map(|h| {
            (
                h.id,
                SidecarProbe {
                    bvid: &h.bvid,
                    uid: h.uid.as_deref(),
                    video_path: h.file_path.as_deref(),
                },
            )
        })
        .collect::<Vec<_>>();
    let sidecar_results = business
        .history_service
        .sidecar_status_batch(
            &sidecar_probes
                .iter()
                .map(|(_, probe)| *probe)
                .collect::<Vec<_>>(),
        )
        .await;
    let sidecar_by_id = sidecar_probes
        .iter()
        .map(|(id, _)| *id)
        .zip(sidecar_results)
        .collect::<HashMap<i32, SidecarStatus>>();

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
            &sidecar_by_id,
        );
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
/// sidecar 状态由调用方整页批量探测后传入，此处不再逐行访问文件系统。
fn build_video_list(
    business: &BusinessState,
    videos: &[history::Model],
    task_by_bvid: &HashMap<String, Vec<download_task::Model>>,
    path_display_mode: &str,
    can_open_directory: bool,
    sidecar_by_id: &HashMap<i32, SidecarStatus>,
) -> Vec<Value> {
    videos
        .iter()
        .map(|h| {
            let sidecar = sidecar_by_id
                .get(&h.id)
                .cloned()
                .unwrap_or(SidecarStatus {
                    video: false,
                    danmaku: false,
                    comments: false,
                    subtitle: false,
                });
            let filepath = aggregate::display_path_for(
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
            let task = aggregate::aggregate_task_progress(matching_tasks.as_deref(), h);

            json!({
                "bvid": h.bvid,
                "history_id": h.id,
                "version": h.version,
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
                "cover_local_path": aggregate::display_path_for(&business.history_service, h.cover_local_path.as_deref(), path_display_mode),
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
                    "version": task.version,
                    "priority": task.priority,
                    "type": task.task_type,
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
        .collect()
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
