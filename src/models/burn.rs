use serde::Serialize;
use std::collections::HashMap;

/// 终态烧录任务在内存表中的保留时长（秒）。
pub const BURN_TASK_TTL_SECONDS: i64 = 60 * 60;
/// 内存烧录任务表容量上限。
pub const MAX_BURN_TASKS: usize = 200;

/// 烧录任务的非终态状态。download 与 live 两个模块写同一张
/// `state.media.burn_tasks` 表，词表统一在此，防止两侧漂移
/// （此前一侧写 "processing"、另一侧按 "running" 清理）。
pub fn burn_status_active(status: &str) -> bool {
    matches!(status, "queued" | "processing" | "running")
}

/// 统一的烧录任务表清理：保留非终态任务与 TTL 内的任务，
/// 超出容量时按最旧 updated_at 淘汰终态条目。
pub fn prune_burn_tasks(tasks: &mut HashMap<String, BurnTask>) {
    let now = chrono::Utc::now().timestamp();
    tasks.retain(|_, task| {
        burn_status_active(&task.status)
            || now.saturating_sub(task.updated_at.max(task.created_at)) <= BURN_TASK_TTL_SECONDS
    });
    if tasks.len() <= MAX_BURN_TASKS {
        return;
    }
    let mut terminal = tasks
        .iter()
        .filter(|(_, task)| !burn_status_active(&task.status))
        .map(|(id, task)| (id.clone(), task.updated_at))
        .collect::<Vec<_>>();
    terminal.sort_by_key(|(_, updated_at)| *updated_at);
    for (id, _) in terminal.into_iter().take(tasks.len() - MAX_BURN_TASKS) {
        tasks.remove(&id);
    }
}

/// 字幕或弹幕烧录接口返回的可序列化状态。
#[derive(Clone, Serialize)]
pub struct BurnTask {
    pub bvid: String,
    pub status: String,
    pub message: String,
    pub output_path: Option<String>,
    #[serde(skip_serializing)]
    pub created_at: i64,
    #[serde(skip_serializing)]
    pub updated_at: i64,
}
