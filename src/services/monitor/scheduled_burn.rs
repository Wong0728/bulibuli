//! 自动烧录调度：扫描已完成且未烧录弹幕/字幕的历史记录，
//! 在博主活跃时段内排队执行 ffmpeg 烧录并登记重试状态。

use crate::models::{blogger, history};
use crate::services::subtitle_burner::SubtitleBurner;
use anyhow::Result;
use chrono::{Duration, Local};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Set,
};
use std::collections::HashMap;
use tracing::{info, warn};

use super::active_window;
use super::scheduled_helpers::{
    auto_burn_retry_at, missing_burn_materials, update_auto_burn_state,
};
use super::MonitorService;

impl MonitorService {
    pub(super) async fn check_auto_burn(&self) -> Result<()> {
        let now = Local::now();
        let histories = history::Entity::find()
            .filter(history::Column::State.eq("completed"))
            .filter(history::Column::FilePath.is_not_null())
            .filter(
                Condition::any()
                    .add(history::Column::BurnedDanmaku.ne(true))
                    .add(history::Column::BurnedSubtitle.ne(true)),
            )
            .filter(
                Condition::any()
                    .add(history::Column::AutoBurnNextRetryAt.is_null())
                    .add(history::Column::AutoBurnNextRetryAt.lte(now)),
            )
            .order_by_asc(history::Column::AutoBurnNextRetryAt)
            .limit(20)
            .all(&self.db)
            .await?;
        if histories.is_empty() {
            return Ok(());
        }

        let settings = self.settings_cached().await?;
        // 旧版配置可能缺 mode 字段，回退 auto 保持与历史行为兼容。
        let ffmpeg_mode = settings
            .get("ffmpeg")
            .and_then(|f| f.get("mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("auto")
            .to_string();
        let custom_ffmpeg = settings
            .get("ffmpeg")
            .and_then(|f| f.get("custom_path"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        // 烧录参数从 settings.burn 读取；旧版配置无 burn 字段时回退默认值（行为不变）。
        let burn_config = settings
            .get("burn")
            .map(|b| {
                serde_json::from_value::<crate::services::settings::BurnSettings>(b.clone())
                    .map(|s| s.to_burn_config())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let burner = SubtitleBurner::with_burn_config(
            self.video_processor.clone(),
            ffmpeg_mode,
            custom_ffmpeg,
            burn_config,
        );

        let uids = histories
            .iter()
            .filter_map(|history| history.uid.clone())
            .collect::<Vec<_>>();
        let bloggers = if uids.is_empty() {
            Vec::new()
        } else {
            blogger::Entity::find()
                .filter(blogger::Column::Uid.is_in(uids))
                .all(&self.db)
                .await?
        };
        let blogger_by_uid = bloggers
            .into_iter()
            .map(|blogger| (blogger.uid.clone(), blogger))
            .collect::<HashMap<_, _>>();

        for h in histories {
            let Some(uid) = h.uid.as_deref() else {
                continue;
            };
            let Some(blogger) = blogger_by_uid.get(uid) else {
                continue;
            };
            let windows = blogger
                .active_windows
                .as_deref()
                .map(active_window::parse_windows)
                .unwrap_or_default();
            if !blogger.is_running || !active_window::is_active(Local::now(), &windows) {
                continue;
            }

            let want_danmaku =
                blogger.burn_danmaku.unwrap_or(false) && !h.burned_danmaku.unwrap_or(false);
            let want_subtitle =
                blogger.burn_subtitle.unwrap_or(false) && !h.burned_subtitle.unwrap_or(false);
            if !want_danmaku && !want_subtitle {
                continue;
            }
            let source = if want_danmaku && want_subtitle {
                "both"
            } else if want_danmaku {
                "danmaku"
            } else {
                "subtitle"
            };

            let Some(path_str) = h.file_path.as_deref() else {
                continue;
            };
            let video_path = std::path::PathBuf::from(path_str);
            if !video_path.exists() {
                continue;
            }
            let missing_materials =
                missing_burn_materials(&video_path, &h.bvid, want_danmaku, want_subtitle).await;
            if !missing_materials.is_empty() {
                let retry_at = Local::now() + Duration::hours(6);
                let mut waiting: history::ActiveModel = h.clone().into();
                waiting.auto_burn_status = Set(Some("waiting_material".to_string()));
                waiting.auto_burn_next_retry_at = Set(Some(retry_at));
                waiting.update(&self.db).await?;
                info!(
                    "[自动烧录] {} 等待素材: {}",
                    h.bvid,
                    missing_materials.join("、")
                );
                continue;
            }

            {
                let mut guard = self.auto_burn_in_progress.lock().await;
                if !guard.insert(h.id) {
                    continue;
                }
            }

            let attempts = h.auto_burn_attempts.saturating_add(1);
            let mut queued: history::ActiveModel = h.clone().into();
            queued.auto_burn_status = Set(Some("queued".to_string()));
            queued.auto_burn_attempts = Set(attempts);
            queued.auto_burn_next_retry_at = Set(None);
            queued.update(&self.db).await?;

            let db = self.db.clone();
            let bvid = h.bvid.clone();
            let history_id = h.id;
            let source = source.to_string();
            let burner = burner.clone();
            let in_progress = self.auto_burn_in_progress.clone();
            let burn_semaphore = self.burn_semaphore.clone();
            let history_service = self.history_service.clone();
            let cancellation = self.cancellation.clone();
            let background_tasks = self.background_tasks.clone();
            let accepted = background_tasks.spawn("auto_burn", async move {
                let Ok(_permit) = burn_semaphore.acquire_owned().await else {
                    let mut guard = in_progress.lock().await;
                    guard.remove(&history_id);
                    return;
                };
                if cancellation.is_cancelled() {
                    tracing::debug!("任务已取消，跳过执行: bvid={}", bvid);
                    let mut guard = in_progress.lock().await;
                    guard.remove(&history_id);
                    return;
                }
                if let Err(error) = update_auto_burn_state(&db, history_id, "running", None).await {
                    warn!("[自动烧录] 更新运行状态失败 {}: {error}", bvid);
                }
                info!("[自动烧录] 开始为 {} 烧录 source={}", bvid, source);
                let result = match source.as_str() {
                    "danmaku" => burner.burn_danmaku(&video_path).await,
                    "subtitle" => burner.burn_subtitle(&video_path).await,
                    _ => burner.burn_mixed(&video_path).await,
                };
                match result {
                    Ok((success, output, message)) => {
                        if success {
                            info!("[自动烧录] {} 成功: {}", bvid, message);
                            if let Err(e) = history_service
                                .mark_burned_by_id(history_id, &source, output.as_deref())
                                .await
                            {
                                warn!("[自动烧录] 更新历史记录失败 {}: {e}", bvid);
                            }
                        } else {
                            warn!("[自动烧录] {} 失败: {}", bvid, message);
                            let retry_at = auto_burn_retry_at(attempts);
                            if let Err(error) =
                                update_auto_burn_state(&db, history_id, "failed", Some(retry_at))
                                    .await
                            {
                                warn!("[自动烧录] 持久化失败状态失败 {}: {error}", bvid);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("[自动烧录] {} 出错: {e}", bvid);
                        let retry_at = auto_burn_retry_at(attempts);
                        if let Err(error) =
                            update_auto_burn_state(&db, history_id, "failed", Some(retry_at)).await
                        {
                            warn!("[自动烧录] 持久化异常状态失败 {}: {error}", bvid);
                        }
                    }
                }
                let mut guard = in_progress.lock().await;
                guard.remove(&history_id);
            });
            if !accepted {
                // queued 状态本身可在下次启动时重新扫描；清掉内存占位，避免
                // 进程未立即退出时后续检查永远跳过该记录。
                self.auto_burn_in_progress.lock().await.remove(&h.id);
                tracing::debug!(bvid = %h.bvid, "自动烧录任务在 shutdown 后被拒绝");
            }
        }
        Ok(())
    }
}
