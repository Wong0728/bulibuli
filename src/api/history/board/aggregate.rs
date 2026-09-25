//! 看板/抽屉共用的展示工具：路径脱敏展示与多任务进度聚合。

use crate::models::{download_task, history};

/// 按展示模式把下载根目录内的路径转成对用户可见的形式。
pub(super) fn display_path_for(
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

pub(super) struct AggregatedTaskProgress {
    pub(super) progress: i32,
    pub(super) speed: i64,
    pub(super) status: String,
    pub(super) downloaded_size: i64,
    pub(super) total_size: i64,
    /// 当前聚合状态对应的任务 ID，供前端 pause/resume 等单任务操作使用。
    /// 多任务时优先返回 downloading/paused 任务，便于暂停按钮定位到活跃任务。
    pub(super) task_id: Option<i32>,
    /// 代表任务的下载优先级（1..=300，默认 100），供前端调整控件显示。
    pub(super) priority: i32,
    /// 代表任务的乐观锁版本，供 pause/resume/priority 使用。
    pub(super) version: i32,
    /// 代表任务的下载类型，供抽屉 retry/remove 选择正确任务分组。
    pub(super) task_type: String,
    /// 失败原因（来自代表任务的 `error` 字段）。仅在状态为 failed/merge_failed 时有值。
    pub(super) error: Option<String>,
    /// 结构化错误分类（如 `Paywall`、`PermissionDenied`），供前端映射为友好中文。
    pub(super) error_kind: Option<String>,
    /// 画质/编码降级原因（如「大会员不可用，已降级 1080P」），供用户排查。
    pub(super) fallback_reason: Option<String>,
}

/// 统一看板与抽屉的任务聚合优先级：下载中 > 已暂停 > 等待 > 失败 > 终态。
/// paused 单独列出（优先级介于 downloading 与 pending 之间），避免被误判为 completed。
pub(super) fn aggregate_task_progress(
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
            version: 0,
            task_type: "video".to_string(),
            error: None,
            error_kind: None,
            fallback_reason: None,
        };
    };
    // 状态优先级：downloading > retrying > merging > paused > pending > failed > completed。
    // retrying/merging 属运行态，缺失会被 unwrap 成 completed 导致看板伪装成已完成。
    let status = [
        "downloading",
        "retrying",
        "merging",
        "paused",
        "pending",
        "failed",
    ]
    .into_iter()
    .find(|candidate| tasks.iter().any(|task| task.status == *candidate))
    .unwrap_or("completed")
    .to_string();
    // 取该状态下的第一个任务作为代表（前端 pause/resume/priority 据此定位单任务）
    let representative = tasks.iter().find(|task| task.status == status);
    let task_id = representative.map(|task| task.id);
    let priority = representative.map(|task| task.priority).unwrap_or(100);
    let version = representative.map(|task| task.version).unwrap_or(0);
    let task_type = representative
        .map(|task| task.task_type.clone())
        .unwrap_or_else(|| "video".to_string());
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
        version,
        task_type,
        error: error_meta.as_ref().and_then(|(e, _, _)| e.clone()),
        error_kind: error_meta.as_ref().and_then(|(_, k, _)| k.clone()),
        fallback_reason: error_meta.as_ref().and_then(|(_, _, r)| r.clone()),
    }
}
