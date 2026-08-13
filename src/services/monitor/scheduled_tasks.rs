use crate::models::{blogger, history};
use crate::services::subtitle_burner::SubtitleBurner;
use anyhow::Result;
use chrono::{DateTime, Duration, Local, TimeZone};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, Statement,
};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use super::active_window;
use super::scheduled_helpers::*;
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
                let cookies = cookies.clone();
                let dc = dc.clone();
                let total_points = time_points.len();
                let next_target_at = time_points.get(idx + 1).and_then(|point| {
                    let hours = point.as_f64().unwrap_or(0.0);
                    Local
                        .timestamp_opt(h.pub_timestamp.unwrap_or(0) + (hours as i64) * 3600, 0)
                        .single()
                });
                tokio::spawn(async move {
                    let Ok(_permit) = service.sidecar_semaphore.clone().acquire_owned().await
                    else {
                        service
                            .scheduled_sidecar_in_progress
                            .lock()
                            .await
                            .remove(&h.id);
                        return;
                    };
                    if service.cancellation.is_cancelled() {
                        tracing::debug!("任务已取消，跳过执行: bvid={}", h.bvid);
                        service
                            .scheduled_sidecar_in_progress
                            .lock()
                            .await
                            .remove(&h.id);
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
                        .remove(&h.id);
                });
            }
        }
        Ok(())
    }

    async fn defer_sidecar_history(
        &self,
        history: &history::Model,
        next_check: DateTime<Local>,
    ) -> Result<()> {
        let mut model: history::ActiveModel = history.clone().into();
        model.next_sidecar_at = Set(Some(next_check));
        model.update(&self.db).await?;
        Ok(())
    }

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
            tokio::spawn(async move {
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
        }
        Ok(())
    }

    pub(super) async fn set_history_next_index(
        &self,
        uid: &str,
        bvid: &str,
        title: &str,
        next_index: i32,
        pub_timestamp: Option<i64>,
    ) -> Result<()> {
        if let Some(h) = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await?
        {
            let mut model: history::ActiveModel = h.into();
            model.source = Set("auto".to_string());
            model.next_download_index = Set(next_index);
            if let Some(ts) = pub_timestamp {
                if ts > 0 {
                    model.pub_timestamp = Set(Some(ts));
                }
            }
            model.update(&self.db).await?;
        } else {
            let pub_date = pub_timestamp.and_then(|ts| {
                DateTime::from_timestamp(ts, 0).map(|date| date.format("%Y-%m-%d").to_string())
            });
            history::ActiveModel {
                uid: Set(Some(uid.to_string())),
                bvid: Set(bvid.to_string()),
                source: Set("auto".to_string()),
                title: Set(Some(title.to_string())),
                pub_date: Set(pub_date),
                pub_timestamp: Set(pub_timestamp.filter(|timestamp| *timestamp > 0)),
                next_download_index: Set(next_index),
                state: Set(Some("pending".to_string())),
                ..Default::default()
            }
            .insert(&self.db)
            .await?;
        }
        Ok(())
    }

    pub(super) async fn schedule_next(&self, blogger: &blogger::Model) -> Result<()> {
        // 根据博主活跃度动态调整轮询间隔：
        // - 活跃（3 天内有投稿）：使用博主配置的原始间隔
        // - 半活跃（30 天内有投稿）：中等间隔 900 秒
        // - 不活跃（30 天以上无投稿）：较长间隔 1800 秒
        let (lo, hi) = self.compute_activity_interval(blogger).await;
        let interval = rand::random_range(lo..=hi);
        let mut next = Local::now() + Duration::seconds(interval as i64);
        let windows = blogger
            .active_windows
            .as_deref()
            .map(active_window::parse_windows)
            .unwrap_or_default();
        let deferred = !active_window::is_active(next, &windows);
        if deferred {
            next = active_window::next_window_start(next, &windows);
        }
        let mut model: blogger::ActiveModel = blogger.clone().into();
        model.next_check = Set(Some(next));
        model.update(&self.db).await?;
        let message = if deferred {
            format!(
                "博主检查完成，已顺延至活跃时段，下次检查时间: {}",
                next.format("%m-%d %H:%M:%S")
            )
        } else {
            format!(
                "博主检查完成，下次检查时间: {} (间隔 {} 秒)",
                next.format("%H:%M:%S"),
                interval
            )
        };
        self.add_log(Some(&blogger.uid), None, &message, "info")
            .await;
        Ok(())
    }

    /// 根据博主最近投稿时间计算轮询间隔区间。
    ///
    /// 活跃度分级：
    /// - 活跃（3 天内有投稿）：使用博主配置的 min_interval / max_interval
    /// - 半活跃（30 天内有投稿）：固定 900 秒（15 分钟）
    /// - 不活跃（30 天以上无投稿或无记录）：固定 1800 秒（30 分钟）
    async fn compute_activity_interval(&self, blogger: &blogger::Model) -> (i32, i32) {
        let uid = &blogger.uid;
        let now_ts = Local::now().timestamp();

        // 从 submission_checkpoints 表读取最近一次成功检查的发布时间
        let last_pub_ts: Option<i64> = self
            .db
            .query_one_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "SELECT last_pub_timestamp FROM submission_checkpoints WHERE uid = ?".to_string(),
                [uid.clone().into()],
            ))
            .await
            .ok()
            .flatten()
            .and_then(|row| {
                row.try_get::<Option<i64>>("", "last_pub_timestamp")
                    .ok()
                    .flatten()
            });

        let (lo, hi, tier) = match last_pub_ts {
            Some(ts) if now_ts - ts < 3 * 24 * 3600 => {
                // 活跃：3 天内有投稿，使用配置间隔
                let lo = if blogger.min_interval <= blogger.max_interval {
                    blogger.min_interval
                } else {
                    blogger.max_interval
                };
                let hi = if blogger.min_interval <= blogger.max_interval {
                    blogger.max_interval
                } else {
                    blogger.min_interval
                };
                (lo, hi, "active")
            }
            Some(ts) if now_ts - ts < 30 * 24 * 3600 => {
                // 半活跃：30 天内有投稿
                (900, 900, "semi-active")
            }
            _ => {
                // 不活跃：30 天以上无投稿或无检查记录
                (1800, 1800, "inactive")
            }
        };

        debug!(uid, tier, lo, hi, "Monitor 退避: 博主活跃度分级");
        (lo, hi)
    }

    pub(super) async fn defer_to_next_window(
        &self,
        blogger: &blogger::Model,
        windows: &[(u32, u32)],
    ) -> Result<()> {
        let next = active_window::next_window_start(Local::now(), windows);
        let mut model: blogger::ActiveModel = blogger.clone().into();
        model.next_check = Set(Some(next));
        model.update(&self.db).await?;
        self.add_log(
            Some(&blogger.uid),
            None,
            &format!(
                "当前处于静默时段，跳过检查，顺延至: {}",
                next.format("%m-%d %H:%M:%S")
            ),
            "info",
        )
        .await;
        Ok(())
    }
}
