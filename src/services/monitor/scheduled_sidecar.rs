//! 计划弹幕/评论下载调度：按 `download_time_points` 时间点表分批补下载，
//! 结合博主活跃时段顺延，并对失败做指数退避重试。

use crate::models::{blogger, history};
use anyhow::Result;
use chrono::{Duration, Local, TimeZone};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Set,
};
use std::collections::HashMap;
use tracing::{info, warn};

use super::active_window;
use super::scheduled_helpers::sidecar_retry_at;
use super::MonitorService;

impl MonitorService {
    pub(super) async fn check_scheduled_danmaku(&self) -> Result<()> {
        let settings = self.settings_cached().await?;
        let dc = settings.get("danmaku_comment").cloned().unwrap_or_default();
        if !dc["enable_smart_download"].as_bool().unwrap_or(true) {
            return Ok(());
        }
        let time_points = dc["download_time_points"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if time_points.is_empty() {
            return Ok(());
        }

        let now = Local::now();
        let histories = history::Entity::find()
            .filter(history::Column::Source.eq("auto"))
            .filter(history::Column::PubTimestamp.gt(0))
            .filter(history::Column::NextDownloadIndex.lt(time_points.len() as i32))
            // 被付费/下架拦截（pay_blocked）或已下架（removed）的视频不再排弹幕计划：
            // 弹幕 API 虽可能仍可用，但产物缺失，且拦截状态可能随时解除，
            // 状态恢复（重新入队下载）后调度会随 source=auto 记录自然恢复。
            .filter(
                Condition::any()
                    .add(history::Column::State.is_null())
                    .add(history::Column::State.is_not_in(["pay_blocked", "removed"])),
            )
            .filter(
                Condition::any()
                    .add(history::Column::NextSidecarAt.is_null())
                    .add(history::Column::NextSidecarAt.lte(now)),
            )
            .order_by_asc(history::Column::NextSidecarAt)
            .limit(50)
            .all(&self.db)
            .await?;

        if histories.is_empty() {
            return Ok(());
        }

        info!(
            "检查计划的弹幕/评论下载任务: {} 个视频需要检查",
            histories.len()
        );
        let cookies = self.get_cookies_for_blogger("").await;
        if cookies.trim().is_empty() {
            warn!("跳过计划弹幕下载：Cookies 未配置，无法获取弹幕/评论数据");
            return Ok(());
        }
        let now_ts = now.timestamp();
        let uids = histories
            .iter()
            .filter_map(|history| history.uid.clone())
            .collect::<Vec<_>>();
        let owners = if uids.is_empty() {
            Vec::new()
        } else {
            blogger::Entity::find()
                .filter(blogger::Column::Uid.is_in(uids))
                .all(&self.db)
                .await?
        };
        let owner_by_uid = owners
            .into_iter()
            .map(|owner| (owner.uid.clone(), owner))
            .collect::<HashMap<_, _>>();

        for h in histories {
            let Some(uid) = h.uid.as_deref() else {
                self.defer_sidecar_history(&h, now + Duration::hours(1))
                    .await?;
                continue;
            };
            let Some(owner) = owner_by_uid.get(uid) else {
                self.defer_sidecar_history(&h, now + Duration::hours(1))
                    .await?;
                continue;
            };
            let windows = owner
                .active_windows
                .as_deref()
                .map(active_window::parse_windows)
                .unwrap_or_default();
            if !owner.is_running {
                self.defer_sidecar_history(&h, now + Duration::hours(1))
                    .await?;
                continue;
            }
            if !active_window::is_active(now, &windows) {
                let next_window = active_window::next_window_start(now, &windows);
                self.defer_sidecar_history(&h, next_window).await?;
                continue;
            }
            let idx = h.next_download_index as usize;
            if idx >= time_points.len() {
                continue;
            }
            let target_hours = time_points[idx].as_f64().unwrap_or(0.0);
            let target_ts = h.pub_timestamp.unwrap_or(0) + (target_hours as i64) * 3600;
            let target_at = Local.timestamp_opt(target_ts, 0).single();
            if now_ts < target_ts {
                if h.next_sidecar_at != target_at {
                    let mut model: history::ActiveModel = h.into();
                    model.next_sidecar_at = Set(target_at);
                    model.update(&self.db).await?;
                }
                continue;
            }
            if now_ts >= target_ts {
                {
                    let mut in_progress = self.scheduled_sidecar_in_progress.lock().await;
                    if !in_progress.insert(h.id) {
                        continue;
                    }
                }
                let hours = (now_ts - h.pub_timestamp.unwrap_or(0)) as f64 / 3600.0;
                let title = h.title.clone().unwrap_or_else(|| h.bvid.clone());
                let service = self.clone();
                let background_tasks = self.background_tasks.clone();
                let history_id = h.id;
                let bvid = h.bvid.clone();
                let cookies = cookies.clone();
                let dc = dc.clone();
                let total_points = time_points.len();
                let next_target_at = time_points.get(idx + 1).and_then(|point| {
                    let hours = point.as_f64().unwrap_or(0.0);
                    Local
                        .timestamp_opt(h.pub_timestamp.unwrap_or(0) + (hours as i64) * 3600, 0)
                        .single()
                });
                let accepted = background_tasks.spawn("scheduled_sidecar", async move {
                    let Ok(_permit) = service.sidecar_semaphore.clone().acquire_owned().await
                    else {
                        service
                            .scheduled_sidecar_in_progress
                            .lock()
                            .await
                            .remove(&history_id);
                        return;
                    };
                    if service.cancellation.is_cancelled() {
                        tracing::debug!("任务已取消，跳过执行: bvid={}", h.bvid);
                        service
                            .scheduled_sidecar_in_progress
                            .lock()
                            .await
                            .remove(&history_id);
                        return;
                    }
                    service
                        .add_log(
                            h.uid.as_deref(),
                            Some(&h.bvid),
                            &format!(
                                "执行计划弹幕下载: {} (已发布 {:.1} 小时，第 {} 次)",
                                title,
                                hours,
                                idx + 1
                            ),
                            "info",
                        )
                        .await;
                    match service
                        .do_download_danmaku(
                            h.uid.as_deref().unwrap_or(""),
                            &h.bvid,
                            &title,
                            &cookies,
                            &dc,
                            h.page,
                        )
                        .await
                    {
                        Ok(()) => {
                            let mut model: history::ActiveModel = h.clone().into();
                            model.next_download_index = Set(idx as i32 + 1);
                            model.sidecar_attempts = Set(0);
                            model.next_sidecar_at = Set(next_target_at);
                            if let Err(error) = model.update(&service.db).await {
                                warn!("更新计划侧车进度失败 {}: {error}", h.bvid);
                            } else if idx + 1 >= total_points {
                                service
                                    .add_log(
                                        h.uid.as_deref(),
                                        Some(&h.bvid),
                                        &format!("视频 {} 已完成所有计划的弹幕/评论下载", title),
                                        "success",
                                    )
                                    .await;
                            }
                        }
                        Err(error) => {
                            let attempts = h.sidecar_attempts.saturating_add(1);
                            let mut model: history::ActiveModel = h.clone().into();
                            model.sidecar_attempts = Set(attempts);
                            model.next_sidecar_at = Set(Some(sidecar_retry_at(attempts)));
                            if let Err(update_error) = model.update(&service.db).await {
                                warn!("持久化计划侧车重试状态失败 {}: {update_error}", h.bvid);
                            }
                            warn!("计划侧车下载失败 {}: {error}", h.bvid);
                        }
                    }
                    service
                        .scheduled_sidecar_in_progress
                        .lock()
                        .await
                        .remove(&history_id);
                });
                if !accepted {
                    // shutdown 与登记使用同一把锁；若这里被拒绝，不能留下一个
                    // 永久占位的 in_progress id。进程正在退出时无需再写 DB，
                    // 下次启动会按原计划重新扫描。
                    self.scheduled_sidecar_in_progress
                        .lock()
                        .await
                        .remove(&history_id);
                    tracing::debug!(bvid = %bvid, "计划侧车任务在 shutdown 后被拒绝");
                }
            }
        }
        Ok(())
    }

    async fn defer_sidecar_history(
        &self,
        history: &history::Model,
        next_check: chrono::DateTime<Local>,
    ) -> Result<()> {
        let mut model: history::ActiveModel = history.clone().into();
        model.next_sidecar_at = Set(Some(next_check));
        model.update(&self.db).await?;
        Ok(())
    }
}
