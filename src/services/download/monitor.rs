//! 下载监控循环：轮询 aria2 状态并驱动任务状态机（generation 守卫防陈旧写入）。

use crate::domain::{DownloadStage, DownloadStatus};
use crate::models::download_task;
use crate::services::file_safety::ensure_disk_space;
use futures::{stream, StreamExt};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tracing::{debug, error, info, warn};

use super::completion::CompleteOutcome;
use super::{task_cache_key, DownloadManager, ProgressCache};

impl DownloadManager {
    async fn queue_terminal_failure(
        &self,
        task: &download_task::Model,
        error: String,
        updates: &mut Vec<(String, i32, i64, download_task::ActiveModel)>,
    ) {
        let mut model: download_task::ActiveModel = task.clone().into();
        model.status = Set("failed".to_string());
        model.error = Set(Some(error.clone()));
        model.speed = Set(0);
        updates.push((task.bvid.clone(), task.id, task.generation, model));
        self.progress_cache
            .lock()
            .await
            .remove(&task_cache_key(&task.bvid, task.cid));
    }

    /// 按 generation 守卫应用一次任务字段更新：仅当数据库中该任务的 generation
    /// 仍等于轮询快照时才写入。用户重试会递增 generation，此守卫用于丢弃陈旧快照。
    /// 返回是否成功写入。
    pub(super) async fn apply_guarded_update(
        &self,
        bvid: &str,
        task_id: i32,
        expected_generation: i64,
        model: download_task::ActiveModel,
    ) -> bool {
        match download_task::Entity::update_many()
            .set(model)
            .filter(download_task::Column::Id.eq(task_id))
            .filter(download_task::Column::Generation.eq(expected_generation))
            .exec(&self.db)
            .await
        {
            Ok(result) => {
                if result.rows_affected == 1 {
                    true
                } else {
                    warn!("跳过陈旧任务状态写入（generation 已变化）: {bvid}");
                    false
                }
            }
            Err(sea_orm::DbErr::RecordNotUpdated) => {
                warn!("跳过陈旧任务状态写入（generation 已变化）: {bvid}");
                false
            }
            Err(error) => {
                error!("更新下载任务失败 {bvid}: {error}");
                false
            }
        }
    }

    async fn apply_guarded_updates(
        &self,
        updates: Vec<(String, i32, i64, download_task::ActiveModel)>,
    ) -> HashSet<i32> {
        if updates.is_empty() {
            return HashSet::new();
        }
        let mut updated_ids = HashSet::new();
        let transaction = match self.db.begin().await {
            Ok(transaction) => transaction,
            Err(error) => {
                error!("开始下载任务批量更新事务失败: {error}");
                return HashSet::new();
            }
        };
        for (bvid, task_id, generation, model) in updates {
            match download_task::Entity::update_many()
                .set(model)
                .filter(download_task::Column::Id.eq(task_id))
                .filter(download_task::Column::Generation.eq(generation))
                .exec(&transaction)
                .await
            {
                Ok(result) if result.rows_affected == 1 => {
                    updated_ids.insert(task_id);
                }
                Ok(_) => {
                    warn!("跳过陈旧任务批量写入（generation 已变化）: {bvid}");
                }
                Err(sea_orm::DbErr::RecordNotUpdated) => {
                    warn!("跳过陈旧任务状态写入（generation 已变化）: {bvid}");
                }
                Err(error) => {
                    error!("批量更新下载任务失败 {bvid}: {error}");
                    return HashSet::new();
                }
            }
        }
        if let Err(error) = transaction.commit().await {
            error!("提交下载任务批量更新事务失败: {error}");
            return HashSet::new();
        }
        updated_ids
    }

    pub(super) async fn monitor_loop(&self) {
        // gid 缺失任务的首见时间：超过宽限期仍无 gid 则判失败（防悬置）
        let mut gidless_since: HashMap<i32, Instant> = HashMap::new();
        // 活跃 2 秒 / 空闲退避 10 秒：空队列时降低查库频率，
        // 新任务入队经 queue_notify 立即唤醒，不牺牲响应速度
        const ACTIVE_TICK: std::time::Duration = std::time::Duration::from_secs(2);
        const IDLE_TICK: std::time::Duration = std::time::Duration::from_secs(10);
        let mut tick = ACTIVE_TICK;
        let mut idle_rounds: u32 = 0;
        // aria2 不可用计数器，需在循环外部声明才能正确累积
        let mut aria2_fail_count: u32 = 0;
        let mut status_failures: HashMap<i32, u8> = HashMap::new();
        let mut disk_full_notified = false;
        let mut last_audio_retry = Instant::now() - std::time::Duration::from_secs(30);
        let mut last_disk_check = Instant::now() - std::time::Duration::from_secs(15);
        let mut disk_space_error: Option<String> = None;

        loop {
            tokio::select! {
                _ = self.cancellation.cancelled() => break,
                _ = tokio::time::sleep(tick) => {}
                _ = self.queue_notify.notified() => {}
            }

            if last_audio_retry.elapsed() >= std::time::Duration::from_secs(30) {
                self.check_audio_retry().await;
                last_audio_retry = Instant::now();
            }

            let all_tasks = match download_task::Entity::find()
                .filter(download_task::Column::Status.is_in(vec!["downloading", "pending"]))
                .all(&self.db)
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    error!("查询下载任务失败: {e}");
                    continue;
                }
            };

            // 原生任务由后台任务驱动；本循环只处理 aria2 GID 任务。
            let tasks = {
                let native = self.native_tasks.lock().await;
                all_tasks
                    .iter()
                    .filter(|t| !native.contains_key(&t.id))
                    .cloned()
                    .collect::<Vec<_>>()
            };

            debug!(
                operation = "download_queue_poll",
                queue_len = all_tasks.len(),
                aria2_queue_len = tasks.len(),
                idle_rounds,
                "下载队列轮询"
            );

            // 防泄漏：任务被外部删除时同步清掉 gid 缺失计时
            let current_ids: HashSet<i32> = tasks.iter().map(|t| t.id).collect();
            gidless_since.retain(|id, _| current_ids.contains(id));
            status_failures.retain(|id, _| current_ids.contains(id));

            if all_tasks.is_empty() {
                // 空队列：连续空闲 3 轮后退避到长间隔，入队通知会立即唤醒
                idle_rounds = idle_rounds.saturating_add(1);
                if idle_rounds >= 3 {
                    tick = IDLE_TICK;
                }
                continue;
            }
            idle_rounds = 0;
            tick = ACTIVE_TICK;

            // 先检查 aria2 是否可用
            if last_disk_check.elapsed() >= std::time::Duration::from_secs(15) {
                disk_space_error = ensure_disk_space(&self.paths.download_dir, None)
                    .await
                    .err()
                    .map(|error| error.to_string());
                last_disk_check = Instant::now();
            }
            if let Some(error) = disk_space_error.as_deref() {
                if let Err(pause_error) = self.aria2.pause_all().await {
                    warn!("磁盘空间不足后暂停 aria2 任务失败: {pause_error}");
                }
                for task in &all_tasks {
                    if let Err(transition_error) = self
                        .state_service
                        .transition(
                            task.id,
                            task.generation,
                            DownloadStatus::Paused,
                            DownloadStage::Transferring,
                        )
                        .await
                    {
                        warn!(
                            task_id = task.id,
                            "磁盘空间不足时持久化暂停状态失败: {transition_error}"
                        );
                    }
                }
                let mut native = self.native_tasks.lock().await;
                for task in &all_tasks {
                    if let Some(token) = native.remove(&task.id) {
                        token.cancel();
                    }
                }
                if !disk_full_notified {
                    if let Err(notify_error) = self
                        .ws
                        .broadcast_system("download:disk-full", json!({ "message": error }))
                        .await
                    {
                        warn!("推送磁盘空间不足事件失败: {notify_error}");
                    }
                    disk_full_notified = true;
                }
                continue;
            }
            if disk_full_notified {
                if let Err(error) = self
                    .ws
                    .broadcast_system("download:disk-recovered", json!({}))
                    .await
                {
                    warn!("推送磁盘恢复事件失败: {error}");
                }
                disk_full_notified = false;
            }

            let aria2_available = self.aria2.is_available().await;
            if !aria2_available {
                aria2_fail_count += 1;
                // 连续 3 次（约 6 秒）aria2 不可用，将所有下载中任务标记为失败
                if aria2_fail_count >= 3 {
                    warn!("Aria2 持续不可用，将 {} 个下载任务标记为失败", tasks.len());
                    for task in &tasks {
                        let mut model: download_task::ActiveModel = task.clone().into();
                        model.status = Set("failed".to_string());
                        model.error = Set(Some("Aria2 下载器不可用".to_string()));
                        model.speed = Set(0);
                        // generation 守卫：避免覆盖用户重试后重置的新状态。
                        let updated = self
                            .apply_guarded_update(&task.bvid, task.id, task.generation, model)
                            .await;
                        // 任务终态：清理 DB 节流与进度缓存
                        self.progress_cache
                            .lock()
                            .await
                            .remove(&task_cache_key(&task.bvid, task.cid));
                        if updated {
                            self.broadcast_progress(
                                task,
                                "failed",
                                task.progress_percent,
                                task.downloaded_size,
                                task.total_size,
                                0,
                                Some("Aria2 下载器不可用"),
                            )
                            .await;
                        }
                    }
                    aria2_fail_count = 0;
                }
                continue;
            }
            aria2_fail_count = 0;

            let mut to_update: Vec<(String, i32, i64, download_task::ActiveModel)> = Vec::new();
            let mut deferred_progress: Vec<(download_task::Model, String, i32, i64, i64, i64)> =
                Vec::new();
            let mut completed_bvids: Vec<(String, Option<i64>, Option<String>)> = Vec::new();
            let gid_tasks = tasks
                .iter()
                .filter_map(|task| task.gid.clone().map(|gid| (task.id, gid)))
                .collect::<Vec<_>>();
            let mut aria_statuses = stream::iter(gid_tasks)
                .map(|(task_id, gid)| async move {
                    (task_id, self.aria2.get_download_status(&gid).await)
                })
                .buffer_unordered(8)
                .collect::<HashMap<_, _>>()
                .await;

            for task in tasks {
                let Some(gid) = &task.gid else {
                    // gid 缺失：add_task_inner 先插行、投递 aria2 成功后才回写 gid，
                    // 正常窗口不足 1 秒；若投递失败会留下 gid=NULL 的行，
                    // 缺少引擎进度超过 60 秒后判失败，避免任务永久悬空。
                    let first_seen = *gidless_since.entry(task.id).or_insert_with(Instant::now);
                    if first_seen.elapsed() >= std::time::Duration::from_secs(60) {
                        gidless_since.remove(&task.id);
                        warn!("任务 {} 超过 60 秒无 aria2 GID，标记为失败", task.bvid);
                        self.queue_terminal_failure(
                            &task,
                            "任务缺少 aria2 GID，无法监控（投递可能失败）".to_string(),
                            &mut to_update,
                        )
                        .await;
                    }
                    continue;
                };
                gidless_since.remove(&task.id);
                {
                    let Some(status_result) = aria_statuses.remove(&task.id) else {
                        continue;
                    };
                    match status_result {
                        Ok(status) => {
                            status_failures.remove(&task.id);
                            let mut model: download_task::ActiveModel = task.clone().into();
                            // 更新内存进度缓存，减少数据库查询压力（按分P粒度隔离）
                            {
                                let mut cache = self.progress_cache.lock().await;
                                cache.insert(
                                    task_cache_key(&task.bvid, task.cid),
                                    ProgressCache {
                                        progress_percent: status.progress_percent,
                                        downloaded_size: status.downloaded_size,
                                        total_size: status.total_size,
                                        speed: status.speed,
                                    },
                                );
                            }

                            match status.status.as_str() {
                                "complete" => {
                                    match self.handle_complete(&task, &status).await {
                                        CompleteOutcome::Skip { clear_throttle } => {
                                            if clear_throttle {
                                                self.progress_cache
                                                    .lock()
                                                    .await
                                                    .remove(&task_cache_key(&task.bvid, task.cid));
                                            }
                                            continue;
                                        }
                                        CompleteOutcome::Finished { uid } => {
                                            // 任务已终态，清理 DB 写入节流缓存，避免长期运行后 HashMap 无限增长
                                            self.progress_cache
                                                .lock()
                                                .await
                                                .remove(&task_cache_key(&task.bvid, task.cid));
                                            // 添加到下载历史并触发音视频合并检查
                                            completed_bvids.push((
                                                task.bvid.clone(),
                                                task.cid,
                                                uid,
                                            ));
                                        }
                                    }
                                }
                                "error" => {
                                    if task.status != "failed" {
                                        info!(
                                            "[DownloadManager] 下载失败: {} ({}) 错误: {}",
                                            task.bvid,
                                            task.task_type,
                                            status.error_message.as_deref().unwrap_or("未知错误")
                                        );
                                        // 失败计入坏 CDN 熔断（两次即熔断 10 分钟），
                                        // 重试重新解析 URL 时 choose_url 会避开该 host
                                        if let Some(host) = task
                                            .url
                                            .as_deref()
                                            .and_then(|u| url::Url::parse(u).ok())
                                            .and_then(|u| u.host_str().map(str::to_owned))
                                        {
                                            self.bili_api.bad_cdns().record_failure(&host).await;
                                        }
                                        // 手动下载任务不携带 UID，避免混入博主自动监测日志。
                                        let uid = if task.source.as_deref() == Some("manual") {
                                            None
                                        } else {
                                            self.get_blogger_uid_from_history(&task.bvid).await
                                        };
                                        self.log_bvid(
                                            &task.bvid,
                                            uid.as_deref(),
                                            &format!(
                                                "下载失败（{}）：{}",
                                                task.task_type,
                                                status
                                                    .error_message
                                                    .as_deref()
                                                    .unwrap_or("未知错误")
                                            ),
                                            "error",
                                        )
                                        .await;
                                    }
                                    self.queue_terminal_failure(
                                        &task,
                                        status
                                            .error_message
                                            .clone()
                                            .unwrap_or_else(|| "未知下载错误".to_string()),
                                        &mut to_update,
                                    )
                                    .await;
                                }
                                "active" => {
                                    let filename_changed = !status.filename.is_empty()
                                        && status.filename != "Unknown"
                                        && task.filename.as_deref() != Some(&status.filename);
                                    if filename_changed {
                                        model.filename = Set(Some(status.filename.clone()));
                                    }
                                    if task.status != "downloading" || filename_changed {
                                        if task.status != "downloading" {
                                            info!(
                                                "[DownloadManager] 开始下载: {} ({})",
                                                task.bvid, task.task_type
                                            );
                                        }
                                        model.status = Set("downloading".to_string());
                                        to_update.push((
                                            task.bvid.clone(),
                                            task.id,
                                            task.generation,
                                            model,
                                        ));
                                    }
                                    deferred_progress.push((
                                        task.clone(),
                                        "downloading".to_string(),
                                        status.progress_percent,
                                        status.downloaded_size,
                                        status.total_size,
                                        status.speed,
                                    ));
                                }
                                "waiting" if task.status != "pending" => {
                                    info!(
                                        "[DownloadManager] 任务等待中: {} ({})",
                                        task.bvid, task.task_type
                                    );
                                    model.status = Set("pending".to_string());
                                    to_update.push((
                                        task.bvid.clone(),
                                        task.id,
                                        task.generation,
                                        model,
                                    ));
                                    deferred_progress.push((
                                        task.clone(),
                                        "pending".to_string(),
                                        task.progress_percent,
                                        task.downloaded_size,
                                        task.total_size,
                                        0,
                                    ));
                                }
                                "paused" if task.status != "paused" => {
                                    info!(
                                        "[DownloadManager] 任务已暂停: {} ({})",
                                        task.bvid, task.task_type
                                    );
                                    model.status = Set("paused".to_string());
                                    to_update.push((
                                        task.bvid.clone(),
                                        task.id,
                                        task.generation,
                                        model,
                                    ));
                                    deferred_progress.push((
                                        task.clone(),
                                        "paused".to_string(),
                                        task.progress_percent,
                                        task.downloaded_size,
                                        task.total_size,
                                        0,
                                    ));
                                }
                                "waiting" | "paused" => {}
                                "stopped" | "removed" => {
                                    // 任务被停止或移除，标记为失败
                                    if task.status != "failed" {
                                        info!(
                                            "[DownloadManager] 任务停止: {} ({}) 状态: {}",
                                            task.bvid, task.task_type, status.status
                                        );
                                    }
                                    self.queue_terminal_failure(
                                        &task,
                                        format!("任务已停止 (aria2 状态: {})", status.status),
                                        &mut to_update,
                                    )
                                    .await;
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            let failures = status_failures.entry(task.id).or_default();
                            *failures = failures.saturating_add(1);
                            warn!("获取 aria2 状态失败 (gid={gid}, attempt={failures}): {e}");
                            if *failures >= 3 {
                                status_failures.remove(&task.id);
                                self.queue_terminal_failure(
                                    &task,
                                    format!("Aria2 状态连续 3 次查询失败: {e}"),
                                    &mut to_update,
                                )
                                .await;
                            }
                        }
                    }
                }
            }

            // 状态/文件名批量写入与进度广播解耦：aria2 任务落库时已是 downloading，
            // 若以“本轮是否更新”为门槛，稳态下进度推送与 DB 落盘会完全停摆
            // （DB 进度从 0 直接跳 100，WS 无下载中事件）。进度提交本身已有
            // generation 守卫与合并去抖，可无条件执行。
            let _ = self.apply_guarded_updates(to_update).await;
            for (task, status, progress, downloaded, total, speed) in deferred_progress {
                self.broadcast_progress(&task, &status, progress, downloaded, total, speed, None)
                    .await;
            }

            for (bvid, cid, uid) in completed_bvids {
                if let Err(e) = self.on_task_completed(&bvid, cid, uid.as_deref()).await {
                    error!("处理完成任务失败 {bvid}: {e}");
                }
            }
        }
    }
}
