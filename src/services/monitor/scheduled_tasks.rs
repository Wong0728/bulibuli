//! 博主轮询调度与侧车下载进度登记：按博主活跃度计算下次检查时间、
//! 顺延至活跃时段，并为侧车补下载登记 next_download_index 进度。
//! 计划弹幕下载调度见 `scheduled_sidecar`，自动烧录调度见 `scheduled_burn`。

use crate::models::{blogger, history};
use anyhow::Result;
use chrono::{Duration, Local};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, Statement,
};
use tracing::debug;

use super::active_window;
use super::MonitorService;

fn activity_interval(
    last_pub_ts: Option<i64>,
    now_ts: i64,
    min_interval: i32,
    max_interval: i32,
) -> (i32, i32, &'static str) {
    match last_pub_ts {
        Some(ts) if now_ts - ts < 3 * 24 * 3600 => (
            min_interval.min(max_interval),
            min_interval.max(max_interval),
            "active",
        ),
        Some(ts) if now_ts - ts < 30 * 24 * 3600 => (900, 900, "semi-active"),
        _ => (1800, 1800, "inactive"),
    }
}

impl MonitorService {
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
            let pub_date = pub_timestamp.and_then(crate::services::time_format::pub_date);
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
        // 只推进 next_check 一列：轮初快照在扫描期间可能已经被用户改过
        // （停止监控、改间隔、改活跃时段、改配置版本），整行回写会把这些改动
        // 连同 version 一起退回旧值，用户的手动改动被静默吃掉。
        blogger::ActiveModel {
            id: Set(blogger.id),
            next_check: Set(Some(next)),
            ..Default::default()
        }
        .update(&self.db)
        .await?;
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

        let (lo, hi, tier) = activity_interval(
            last_pub_ts,
            now_ts,
            blogger.min_interval,
            blogger.max_interval,
        );

        debug!(uid, tier, lo, hi, "Monitor 退避: 博主活跃度分级");
        (lo, hi)
    }

    pub(super) async fn defer_to_next_window(
        &self,
        blogger: &blogger::Model,
        windows: &[(u32, u32)],
    ) -> Result<()> {
        let next = active_window::next_window_start(Local::now(), windows);
        // 同 schedule_next：顺延只推进 next_check，不整行回写轮初快照
        blogger::ActiveModel {
            id: Set(blogger.id),
            next_check: Set(Some(next)),
            ..Default::default()
        }
        .update(&self.db)
        .await?;
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

#[cfg(test)]
mod tests {
    use super::activity_interval;

    #[test]
    fn activity_interval_clamps_tiers_and_reversed_configuration() {
        let now = 1_000_000;
        assert_eq!(
            activity_interval(Some(now - 60), now, 120, 60),
            (60, 120, "active")
        );
        assert_eq!(
            activity_interval(Some(now - 4 * 24 * 3600), now, 60, 120),
            (900, 900, "semi-active")
        );
        assert_eq!(
            activity_interval(None, now, 60, 120),
            (1800, 1800, "inactive")
        );
    }
}
