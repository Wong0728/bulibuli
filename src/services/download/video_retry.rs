//! 视频失败自动重试：对 failed 的 video 任务按指数退避重试（最多 3 次），
//! 超限后保持终态等待人工干预。博主监控场景一次 CDN 抖动不再需要人工介入。

use crate::models::download_task;
use chrono::{Duration, Local};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use tracing::{error, info, warn};

use super::DownloadManager;

/// 单个视频任务的最大自动重试次数（与音频重试一致）。
const MAX_VIDEO_RETRIES: u32 = 3;

impl DownloadManager {
    /// 检查并自动重试失败的视频任务（monitor_loop 每 30 秒调用一次）。
    pub(super) async fn check_video_retry(&self) {
        let failed_video = match download_task::Entity::find()
            .filter(download_task::Column::TaskType.eq("video"))
            .filter(download_task::Column::Status.eq("failed"))
            .all(&self.db)
            .await
        {
            Ok(tasks) => tasks,
            Err(e) => {
                error!("查询失败视频任务失败: {e}");
                return;
            }
        };

        for task in failed_video {
            let attempts = task.attempts.max(0) as u32;
            if attempts >= MAX_VIDEO_RETRIES {
                // 已达最大重试次数：保持终态，由用户手动重试。
                continue;
            }
            // 冷却检查（指数退避）
            let now = Local::now();
            if !task.next_retry_at.is_none_or(|retry_at| now >= retry_at) {
                continue;
            }

            let delay = Duration::seconds(30 * 2_i64.pow(attempts));
            info!(
                "[视频自动重试] {} 尝试第 {} 次重试，若再失败退避 {} 秒",
                task.bvid,
                attempts + 1,
                delay.num_seconds()
            );

            // 先落下次退避时间：本轮再失败时由冷却时间兜底，避免 30 秒节拍内反复打 URL。
            // retry_task 成功后任务离开 failed 状态，next_retry_at 不再被消费。
            let mut model: download_task::ActiveModel = task.clone().into();
            model.next_retry_at = Set(Some(now + delay));
            if let Err(e) = model.update(&self.db).await {
                warn!("[视频自动重试] 更新重试状态失败 {}: {e}", task.bvid);
                continue;
            }

            // 复用手动重试路径：重新解析 URL（避开坏 CDN）、failed→Retrying、重新入队。
            match self.retry_task(&task.bvid, "video").await {
                Ok(outcome) if outcome.ok => {
                    info!("[视频自动重试] {} 重试已投递", task.bvid);
                }
                Ok(outcome) => {
                    info!(
                        "[视频自动重试] {} 本次未能恢复：{}，将在退避后重试",
                        task.bvid, outcome.message
                    );
                }
                Err(e) => {
                    warn!("[视频自动重试] {} 投递失败: {e}，将在退避后重试", task.bvid);
                }
            }
        }
    }
}
