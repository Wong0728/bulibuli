//! 生命周期管理：构造、监控启停与断点续传恢复。

use crate::models::{download_task, log};
use crate::services::{
    concurrency_gate::ConcurrencyGate, download_state::DownloadStateService,
    progress_writer::ProgressWriter,
};
use anyhow::Result;
use chrono::Local;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use super::queue::page_info_from_task;
use super::{file_stem_for, DownloadManager, DownloadManagerDependencies};
use crate::services::spawn_util::wait_join_handle;

impl DownloadManager {
    pub async fn new(dependencies: DownloadManagerDependencies) -> Result<Self> {
        let DownloadManagerDependencies {
            config,
            paths,
            db,
            aria2,
            bili_api,
            video_processor,
            ws,
            settings_service,
            cancellation,
            background_tasks,
        } = dependencies;
        let settings = settings_service.current();
        if let Err(error) = aria2.init(settings.as_ref()).await {
            warn!("Aria2 初始化失败，将保留原生下载兜底: {error}");
        }
        let max_parallel = settings.parallel_download.max_parallel;
        let native = super::native::NativeDownloader::new()?;

        Ok(Self {
            config,
            paths,
            db: db.clone(),
            aria2,
            bili_api,
            video_processor,
            ws,
            monitor_handle: Arc::new(Mutex::new(None)),
            disk_resume_handle: Arc::new(Mutex::new(None)),
            cancellation: cancellation.clone(),
            settings_service,
            background_tasks,
            progress_writer: ProgressWriter::start(db.clone(), cancellation.child_token()),
            state_service: DownloadStateService::new(db.clone()),
            concurrency_gate: ConcurrencyGate::new(max_parallel),
            progress_cache: Arc::new(Mutex::new(HashMap::new())),
            retry_backoff: Arc::new(Mutex::new(HashMap::new())),
            merge_in_progress: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            native,
            native_tasks: Arc::new(Mutex::new(HashMap::new())),
            aria2_recover_failed_at: Arc::new(Mutex::new(None)),
            queue_notify: Arc::new(tokio::sync::Notify::new()),
            add_task_locks: Arc::new(Mutex::new(HashMap::new())),
            recent_completions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(super) async fn settings_value(&self) -> Result<Value> {
        // 运行时设置的唯一信息源是 SettingsService 的 ArcSwap 快照（保存即热更新），
        // 这里直接序列化当前快照即可，无需额外缓存层。
        Ok(serde_json::to_value(
            self.settings_service.current().as_ref(),
        )?)
    }

    /// 写一条带 bvid 的库日志（抽屉“日志”区按 bvid 过滤展示）。
    /// 与 monitor::add_log 同构，作用于下载/烧录管线，使抽屉日志能看到真实活动。
    pub(super) async fn log_bvid(&self, bvid: &str, uid: Option<&str>, message: &str, level: &str) {
        let entry = log::ActiveModel {
            level: Set(level.to_string()),
            message: Set(message.to_string()),
            uid: Set(uid.map(|s| s.to_string())),
            bvid: Set(Some(bvid.to_string())),
            created_at: Set(Some(Local::now())),
            ..Default::default()
        };
        if let Err(e) = entry.insert(&self.db).await {
            warn!("[DownloadManager] 写入 bvid 日志失败 {bvid}: {e}");
        }
    }

    pub async fn start_monitor(&self) {
        if self.monitor_handle.lock().await.is_some() {
            return;
        }
        // 断点续传：恢复上次未完成的任务
        self.resume_pending_tasks().await;
        let manager = self.clone();
        let handle = tokio::spawn(async move {
            manager.monitor_loop().await;
        });
        *self.monitor_handle.lock().await = Some(handle);
    }

    /// 断点续传重建前重新解析下载 URL。
    /// B 站 m4s/m4a 的 CDN URL 带 deadline 签名（约 2h），重启后旧 URL 大概率 403，
    /// 因此 video/audio 必须重新解析；cover/danmaku/comments 的 URL 稳定或走 API，沿用旧值。
    /// 返回 `None` 表示重新解析失败（视频被删/风控等），调用方据此置为失败终态。
    pub(super) async fn resolve_resume_url(&self, task: &download_task::Model) -> Option<String> {
        // 恢复任务时使用当前登录态重新解析 URL。
        let cookies = self
            .settings_service
            .cookie_header()
            .await
            .unwrap_or_default();
        match task.task_type.as_str() {
            "video" => {
                let streams = self
                    .bili_api
                    .get_video_urls(&task.bvid, &cookies, 4048, Some(task.quality), task.cid)
                    .await
                    .ok()?;
                // 优先取按 task.quality 选中的流；否则退回最高可用画质
                streams
                    .selected_quality
                    .map(|q| q.url)
                    .or_else(|| streams.qualities.first().map(|q| q.url.clone()))
            }
            "audio" => {
                let audio = self
                    .bili_api
                    .get_audio_url(
                        &task.bvid,
                        task.cid,
                        &cookies,
                        &self
                            .settings_service
                            .current()
                            .query
                            .audio_quality_preference,
                    )
                    .await
                    .ok()??;
                Some(audio.audio_url)
            }
            // cover/danmaku/comments：URL 稳定或后续走 API 重取，沿用旧值即可
            _ => task
                .original_url
                .as_deref()
                .or(task.url.as_deref())
                .map(str::to_string),
        }
    }

    /// 断点续传：程序重启后恢复未完成的下载任务
    async fn resume_pending_tasks(&self) {
        let tasks = match download_task::Entity::find()
            .filter(
                download_task::Column::Status.is_in(crate::domain::DownloadStatus::RESUME_STATUSES),
            )
            .all(&self.db)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                error!("断点续传：查询未完成任务失败: {e}");
                return;
            }
        };
        if tasks.is_empty() {
            return;
        }
        info!("断点续传：发现 {} 个未完成任务，尝试恢复...", tasks.len());

        for task in tasks {
            // 检查 GID 是否仍在 aria2 session 中
            if let Some(ref gid) = task.gid {
                if self.aria2.get_download_status(gid).await.is_ok() {
                    info!(
                        "断点续传：任务 {} GID={} 仍在 aria2 中，继续监控",
                        task.bvid, gid
                    );
                    continue;
                }
            }
            // 崩溃窗口收窄：aria2 可能已完成下载但 DB 未及同步（重启后 GID 已失效）。
            // 成品仍在磁盘上时直接补写完成态并跳过重建，避免整段重下并以
            // --allow-overwrite 覆盖成品（纯浪费带宽且存在覆盖风险）。
            let dir = self.task_download_dir(&task).await;
            let stem = file_stem_for(&task.bvid, task.page);
            if Self::completed_product_exists(&dir, &stem, &task.task_type).await {
                match self
                    .state_service
                    .complete_once(task.id, task.generation)
                    .await
                {
                    Ok(true) => {
                        info!("断点续传：{} 成品已存在，跳过重下直接标记完成", task.bvid);
                        continue;
                    }
                    // 闸门未通过（已完成/generation 变化）：按原流程继续重建收敛
                    Ok(false) => {}
                    Err(e) => warn!("断点续传：{} 补写完成状态失败: {e}", task.bvid),
                }
            }
            // GID 不存在，需重建任务。B 站 m4s/m4a 的 CDN URL 带 deadline 签名（约 2h），
            // 音视频 CDN URL 带短期签名，恢复任务时必须重新解析。
            let url = match self.resolve_resume_url(&task).await {
                Some(u) if !u.is_empty() => u,
                _ => {
                    warn!("断点续传：任务 {} 无法解析下载链接，标记为失败", task.bvid);
                    let mut model: download_task::ActiveModel = task.into();
                    model.status = Set("failed".to_string());
                    model.error = Set(Some(
                        "重启后无法恢复：下载链接已失效且重新解析失败".to_string(),
                    ));
                    if let Err(error) = model.update(&self.db).await {
                        error!("持久化不可恢复任务失败: {error}");
                    }
                    continue;
                }
            };
            let cookies = self
                .settings_service
                .cookie_header()
                .await
                .unwrap_or_default();
            let title = task.title.as_deref().unwrap_or(&task.bvid);
            info!("断点续传：重建任务 {} ({})", task.bvid, task.task_type);
            // 重新添加到 aria2（保留任务原有来源，存量 NULL 按 auto 处理）
            let task_source = task.source.clone().unwrap_or_else(|| "auto".to_string());
            let page_info = page_info_from_task(&task);
            // 选出的任务本身就是 downloading（gid 已失效），add_task_inner 的
            // "正在下载中"去重会把它拒绝。先置回 pending 让重建通过。
            {
                let mut model: download_task::ActiveModel = task.clone().into();
                model.status = Set("pending".to_string());
                if let Err(error) = model.update(&self.db).await {
                    warn!("断点续传：重置任务 {} 状态失败: {error}", task.bvid);
                }
            }
            match self
                .add_task(
                    &task.bvid,
                    title,
                    &url,
                    &cookies,
                    task.quality,
                    &task.task_type,
                    None,
                    &task_source,
                    page_info.as_ref(),
                    None,
                )
                .await
            {
                Ok(outcome) if outcome.ok => {
                    info!("断点续传：任务 {} 已重新加入队列", task.bvid)
                }
                Ok(outcome) => {
                    // 去重拒绝（同 bvid/cid/type 已有活跃任务）等业务性失败：
                    // 恢复流程不再推进，标记 failed 避免滞留 downloading。
                    warn!(
                        "断点续传：任务 {} 重新入队被拒绝: {}",
                        task.bvid, outcome.message
                    );
                    let mut model: download_task::ActiveModel = task.into();
                    model.status = Set("failed".to_string());
                    model.error = Set(Some(format!("重启后恢复被拒绝: {}", outcome.message)));
                    if let Err(update_error) = model.update(&self.db).await {
                        error!("持久化恢复失败状态失败: {update_error}");
                    }
                }
                Err(e) => {
                    error!("断点续传：恢复任务 {} 失败: {e}", task.bvid);
                    let mut model: download_task::ActiveModel = task.into();
                    model.status = Set("failed".to_string());
                    model.error = Set(Some(format!("重启后恢复失败: {e}")));
                    if let Err(update_error) = model.update(&self.db).await {
                        error!("持久化恢复失败状态失败: {update_error}");
                    }
                }
            }
        }
    }

    pub async fn stop_monitor(&self) {
        if let Err(error) = self.progress_writer.shutdown().await {
            error!("关闭进度写入器失败: {error}");
        }
        self.cancellation.cancel();
        if let Some(handle) = self.monitor_handle.lock().await.take() {
            wait_join_handle("download_monitor", handle, Duration::from_secs(10)).await;
        }
        if let Some(handle) = self.disk_resume_handle.lock().await.take() {
            wait_join_handle("download_disk_resume", handle, Duration::from_secs(10)).await;
        }
    }
}
