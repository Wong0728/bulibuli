//! 下载完成处理：完成防抖闸门、MD5 去重归位、历史落库与封面下载。
//!
//! 由 `monitor` 的轮询循环在 aria2 报告 `complete` 时调用。

use crate::models::download_task;
use crate::services::aria2::Aria2Status;
use sea_orm::Set;
use tracing::{error, info, warn};

use super::{file_stem_for, DownloadManager};

/// `handle_complete` 的处理结果，驱动 monitor_loop 的后续动作。
pub(super) enum CompleteOutcome {
    /// 跳过本任务（已完成 / generation 变化 / 闸门失败）。
    /// `clear_throttle` 为 true 时需清理 DB 写入节流缓存。
    Skip { clear_throttle: bool },
    /// 完成副作用已执行；uid 供 on_task_completed 触发音视频合并。
    Finished { uid: Option<String> },
}

impl DownloadManager {
    /// 处理单个 aria2 `complete` 任务：闸门判定 → MD5 去重 → 补写展示字段 →
    /// 写历史 → 记日志 → 下封面 → 广播进度。
    pub(super) async fn handle_complete(
        &self,
        task: &download_task::Model,
        status: &Aria2Status,
    ) -> CompleteOutcome {
        // 条件更新只允许匹配 generation 的首次完成事件执行副作用。
        match self
            .state_service
            .complete_once(task.id, task.generation)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                // 已完成或 generation 已变化，跳过覆盖写与副作用
                return CompleteOutcome::Skip {
                    clear_throttle: true,
                };
            }
            Err(e) => {
                error!("完成防抖闸门失败 {}: {e}", task.bvid);
                return CompleteOutcome::Skip {
                    clear_throttle: false,
                };
            }
        }
        if task.status != "completed" {
            info!(
                "[DownloadManager] 下载完成: {} ({}) 进度 100%",
                task.bvid, task.task_type
            );
        }

        // 获取下载目录与 UP 主 uid（用于 MD5 去重与历史记录）
        let uid = self.get_blogger_uid_from_history(&task.bvid).await;
        let dir = self.task_download_dir(task).await;
        // 文件名词根：单P为 bvid，多P为 `{bvid}_p{page}`，用于去重扫描与临时文件命名。
        let stem = file_stem_for(&task.bvid, task.page);

        // Aria2 返回的文件名（处理空/Unknown 退化情况）
        let aria2_filename = if status.filename.is_empty() || status.filename == "Unknown" {
            task.filename
                .clone()
                .unwrap_or_else(|| format!("{}.{}", stem, task.task_type))
        } else {
            status.filename.clone()
        };

        // 判断是否为 .downloading 临时文件：触发 MD5 去重流程
        let (final_filename, dedupe_message) = if let Some(base_name) =
            aria2_filename.strip_suffix(".downloading")
        {
            let temp_path = dir.join(&aria2_filename);
            match self
                .dedupe_and_finalize_file(&temp_path, base_name, &task.bvid, &stem, &task.task_type)
                .await
            {
                Ok(result) => (result.final_filename, Some(result.message)),
                Err(e) => {
                    error!("MD5 去重失败 {}: {e}", task.bvid);
                    if let Err(error) = tokio::fs::rename(&temp_path, dir.join(base_name)).await {
                        warn!("归位 aria2 临时文件失败: {error}");
                    }
                    (
                        base_name.to_string(),
                        Some(format!("MD5 去重失败，已保留文件: {e}")),
                    )
                }
            }
        } else {
            (aria2_filename, None)
        };

        // complete_once 已原子落库 status=completed/stage=finalizing；
        // 此处仅补写展示字段(文件名/进度/大小/速度)，同样按 generation 守卫。
        let mut model: download_task::ActiveModel = task.clone().into();
        model.progress_percent = Set(100);
        model.downloaded_size = Set(status.total_size);
        model.total_size = Set(status.total_size);
        model.speed = Set(0);
        model.filename = Set(Some(final_filename.clone()));
        self.apply_guarded_update(&task.bvid, task.id, task.generation, model)
            .await;
        // 添加到下载历史
        let file_path = Some(dir.join(&final_filename));
        // 完成后的附加请求使用当前登录态。
        let task_cookies = self
            .settings_service
            .cookie_header()
            .await
            .unwrap_or_default();
        if let Err(e) = self
            .add_to_history(
                &task.bvid,
                file_path.as_deref(),
                uid.as_deref(),
                None,
                Some(&task_cookies),
                task.source.as_deref().unwrap_or("auto"),
            )
            .await
        {
            error!("添加历史记录失败 {}: {e}", task.bvid);
        }

        // 写一条带 bvid 的库日志（仅首次进入终态时），供抽屉“日志”区展示。
        // 手动下载任务不携带 uid，避免混入博主自动监测日志。
        if task.status != "completed" {
            let log_uid = if task.source.as_deref() == Some("manual") {
                None
            } else {
                uid.as_deref()
            };
            self.log_bvid(
                &task.bvid,
                log_uid,
                &format!("下载完成（{}）", task.task_type),
                "success",
            )
            .await;
        }

        // 确保封面已落盘（本地已有则跳过，避免与 add_to_history 内的下载重复请求）
        if let Err(e) = self.ensure_cover_local(&task.bvid, uid.as_deref()).await {
            warn!("自动下载封面失败 {}: {e}", task.bvid);
        }

        self.broadcast_progress(
            task,
            "completed",
            100,
            status.total_size,
            status.total_size,
            0,
            dedupe_message.as_deref(),
        )
        .await;

        CompleteOutcome::Finished { uid }
    }
}
