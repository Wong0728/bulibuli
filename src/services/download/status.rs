//! 状态查询与进度广播：get_status、队列摘要、优先级调整与 WS 推送。

use crate::domain::{
    BasicInfo, DownloadInfo, DownloadStage, DownloadStatus, FileInfo, TaskInfo, TaskKey, TaskKind,
    TaskSource,
};
use crate::models::{download_task, history};
use crate::services::progress_writer::ProgressSnapshot;
use anyhow::{anyhow, Result};
use chrono::{Duration, Local};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Set,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::str::FromStr;
use tracing::warn;

use super::{task_cache_key, DownloadManager};

impl DownloadManager {
    /// 队列摘要供健康页与前端轮询使用；不暴露下载链接或 Cookie。
    pub async fn queue_metrics(&self) -> Result<Value> {
        let tasks = download_task::Entity::find().all(&self.db).await?;
        let mut statuses = std::collections::BTreeMap::<String, i64>::new();
        let mut error_kinds = std::collections::BTreeMap::<String, i64>::new();
        let mut waiting_retry = 0_i64;
        let now = Local::now();
        for task in tasks {
            *statuses.entry(task.status.clone()).or_default() += 1;
            if let Some(kind) = task.error_kind {
                *error_kinds.entry(kind).or_default() += 1;
            }
            if task.next_retry_at.is_some_and(|time| time > now) {
                waiting_retry += 1;
            }
        }
        Ok(
            json!({ "statuses": statuses, "error_kinds": error_kinds, "waiting_retry": waiting_retry }),
        )
    }

    pub async fn set_priority(&self, bvid: &str, task_type: &str, priority: i32) -> Result<Value> {
        if !(1..=300).contains(&priority) {
            return Err(anyhow!("优先级必须在 1..=300"));
        }
        let task = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(download_task::Column::TaskType.eq(task_type))
            .one(&self.db)
            .await?
            .ok_or_else(|| anyhow!("未找到下载任务"))?;
        let mut model: download_task::ActiveModel = task.into();
        model.priority = Set(priority);
        model.update(&self.db).await?;
        Ok(json!({ "success": true, "priority": priority }))
    }

    pub async fn get_status(&self, uid: Option<&str>) -> Result<Value> {
        let mut query = download_task::Entity::find();
        if let Some(uid) = uid {
            // 通过 history 表反查 bvid 过滤
            let history_uids = history::Entity::find()
                .select_only()
                .column(history::Column::Bvid)
                .filter(history::Column::Uid.eq(uid))
                .into_tuple::<String>()
                .all(&self.db)
                .await?;
            let bvids = history_uids;
            if bvids.is_empty() {
                return Ok(json!({
                    "success": true,
                    "stats": {"pending": 0, "downloading": 0, "completed": 0, "failed": 0},
                    "statuses": {},
                }));
            }
            query = query.filter(download_task::Column::Bvid.is_in(bvids));
        }
        let recent_cutoff = Local::now() - Duration::hours(24);
        // 非终态任务（含 paused/merging）不设 24 小时时限：长期暂停的任务不能从快照里消失，
        // 否则前端全量替换后会丢失暂停态，恢复按钮找不到 task_id。
        let tasks = query
            .filter(
                Condition::any()
                    .add(download_task::Column::Status.is_in(DownloadStatus::ACTIVE_STATUSES))
                    .add(download_task::Column::UpdatedAt.gte(recent_cutoff)),
            )
            .order_by_desc(download_task::Column::UpdatedAt)
            .limit(500)
            .all(&self.db)
            .await?;
        let progress_cache = self.progress_cache.lock().await;
        let mut stats: HashMap<String, i64> = HashMap::new();
        let mut statuses = serde_json::Map::new();
        // 统一借用风格（与 get_status_by_blogger 保持一致）
        for t in &tasks {
            *stats.entry(t.status.clone()).or_insert(0) += 1;
            // 状态 map 键：单P为 `{bvid}_{type}`（不变），多P带分P号避免同 bvid 不同分P互相覆盖。
            let key = match t.page {
                Some(p) => format!("{}_p{}_{}", t.bvid, p, t.task_type),
                None => format!("{}_{}", t.bvid, t.task_type),
            };
            let cached = progress_cache.get(&task_cache_key(&t.bvid, t.cid));
            let status = DownloadStatus::from_str(&t.status).unwrap_or(DownloadStatus::Failed);
            let stage = serde_json::from_value::<DownloadStage>(json!(t.stage))
                .unwrap_or(DownloadStage::Queued);
            let task_info = TaskInfo {
                basic: BasicInfo {
                    bvid: t.bvid.clone(),
                    title: t.title.clone().unwrap_or_else(|| t.bvid.clone()),
                    source: if t.source.as_deref() == Some("manual") {
                        TaskSource::Manual
                    } else {
                        TaskSource::Auto
                    },
                    owner_uid: None,
                    owner_name: None,
                    cover: None,
                },
                file: FileInfo {
                    path: t.download_dir.clone(),
                    filename: t.filename.clone(),
                    total_size: cached.map_or(t.total_size, |value| value.total_size),
                    format: None,
                },
                download: DownloadInfo {
                    status: status.clone(),
                    stage,
                    progress_percent: cached
                        .map_or(t.progress_percent, |value| value.progress_percent),
                    downloaded_size: cached
                        .map_or(t.downloaded_size, |value| value.downloaded_size),
                    speed: cached.map_or(t.speed, |value| value.speed),
                    generation: t.generation,
                    error: t.error.clone(),
                },
            };
            statuses.insert(
                key,
                serde_json::json!({
                    "bvid": t.bvid,
                    "cid": t.cid,
                    "page": t.page,
                    "part_title": t.part_title,
                    "type": t.task_type,
                    "title": t.title,
                    "status": status,
                    "terminal": status.is_terminal(),
                    "task_id": t.id,
                    "version": t.version,
                    "priority": t.priority,
                    "progress_percent": task_info.download.progress_percent,
                    "downloaded_size": task_info.download.downloaded_size,
                    "total_size": task_info.file.total_size,
                    "speed": task_info.download.speed,
                    "error": t.error,
                    "error_kind": t.error_kind,
                    "fallback_reason": t.fallback_reason,
                    "task": task_info,
                    "updated_at": t.updated_at.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
                }),
            );
        }
        for s in ["pending", "downloading", "completed", "failed"] {
            stats.entry(s.to_string()).or_insert(0);
        }
        Ok(json!({
            "success": true,
            "stats": stats,
            "statuses": statuses,
        }))
    }

    /// 高频任务快照之外的轻量下载引擎健康状态。
    pub async fn get_health(&self) -> Value {
        let (connected, status, diagnostics) = self.aria2_status_pair().await;
        json!({
            "aria2_connected": connected,
            "aria2_status": status,
            "aria2_diagnostics": diagnostics,
        })
    }

    /// 统一获取 aria2 连接状态对：(is_available, status_str)。
    /// 两个 get_status* 接口风格保持一致，避免各自重复调用 is_available + status。
    async fn aria2_status_pair(&self) -> (bool, String, Value) {
        let diagnostics = self.aria2.diagnostics().await;
        let status = diagnostics["state"]
            .as_str()
            .unwrap_or("disconnected")
            .to_string();
        let connected = status == "connected";
        (connected, status, diagnostics)
    }

    /// 计算当前 bvid 下载流程的步骤信息。
    /// 手动下载：视频(含音频) + 封面 = 2 步
    /// 返回 (current_step, total_steps, step_label)
    fn compute_step_info_for(stage: &str, task_type: &str) -> (i32, i32, String) {
        if matches!(stage, "muxing" | "finalizing" | "done") {
            return (2, 2, "合并与整理".to_string());
        }
        let label = match task_type {
            "audio" => "音频",
            "cover" => "封面",
            _ => "视频",
        };
        (1, 2, label.to_string())
    }

    fn compute_step_info(task: &download_task::Model) -> (i32, i32, String) {
        Self::compute_step_info_for(&task.stage, &task.task_type)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn broadcast_progress(
        &self,
        task: &download_task::Model,
        status: &str,
        progress: i32,
        downloaded: i64,
        total: i64,
        speed: i64,
        error: Option<&str>,
    ) {
        // 调用方已持有 task，避免在轮询中重复查库。
        // 注意：完成防抖闸门（complete_once）由 handle_complete 独占消费，
        // 此处不得重复调用，否则会在真正的完成处理之前提前吃掉闸门
        let bvid = task.bvid.as_str();
        let task_type = task.task_type.as_str();
        let kind = match task_type {
            "audio" => TaskKind::Audio,
            "danmaku" => TaskKind::Danmaku,
            "comments" => TaskKind::Comments,
            "cover" => TaskKind::Cover,
            _ => TaskKind::Video,
        };
        self.progress_writer
            .submit(
                TaskKey {
                    bvid: bvid.to_string(),
                    kind,
                    page: task.page,
                },
                ProgressSnapshot {
                    task_id: task.id,
                    generation: task.generation,
                    progress_percent: progress.clamp(0, 100),
                    downloaded_size: downloaded,
                    total_size: total,
                    speed,
                },
            )
            .await;
        // 计算步骤信息：手动下载 = 视频(含音频合并)+封面 = 2步
        // step_label 表示当前正在进行的子任务名称
        let (step, total_steps, step_label) = Self::compute_step_info(task);

        let mut payload = json!({
            "task_id": task.id,
            "bvid": bvid,
            // 标题/优先级随推送下发：前端收到新任务首条进度时可直接建条目，
            // 不必等下一次全量快照（此前最长 10 秒的空窗会让小文件
            // “下载完成都没在队列里出现过”）。
            "title": task.title.as_deref().unwrap_or(bvid),
            "priority": task.priority,
            "cid": task.cid,
            "page": task.page,
            "generation": task.generation,
            "part_title": task.part_title,
            "type": task_type,
            "status": status,
            "progress_percent": progress,
            "downloaded_size": downloaded,
            "total_size": total,
            "speed": speed,
            "step": step,
            "total_steps": total_steps,
            "step_label": step_label,
        });
        if let Some(e) = error {
            payload["error"] = json!(e);
        }
        if let Err(error) = self.ws.broadcast_download_progress(payload).await {
            warn!("推送下载进度失败 bvid={bvid}: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DownloadManager;

    #[test]
    fn progress_steps_describe_download_and_finalize_stages() {
        assert_eq!(
            DownloadManager::compute_step_info_for("downloading", "video"),
            (1, 2, "视频".to_string())
        );
        assert_eq!(
            DownloadManager::compute_step_info_for("downloading", "audio"),
            (1, 2, "音频".to_string())
        );
        assert_eq!(
            DownloadManager::compute_step_info_for("finalizing", "video"),
            (2, 2, "合并与整理".to_string())
        );
    }
}
