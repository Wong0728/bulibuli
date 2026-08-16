//! 监控日志：写入（带 SQLITE_BUSY 重试与 WS 推送）、查询与超量清理。

use crate::models::log;
use anyhow::Result;
use chrono::Local;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;
use tracing::{error, info, warn};

use super::{MonitorService, LOG_CLEANUP_INTERVAL};

impl MonitorService {
    /// 日志查询（供 /api/logs/* 使用，API 层禁止直连数据库）。
    /// - `bvid=Some`：按 bvid 过滤（抽屉"日志"区）
    /// - `uid=Some`：按博主 UID 过滤。
    /// - 两者皆 None：全局日志（uid IS NULL）。
    ///
    /// 结果按创建时间倒序（最新在前）。
    pub async fn query_logs(
        &self,
        uid: Option<&str>,
        bvid: Option<&str>,
        limit: u64,
    ) -> Result<Vec<log::Model>> {
        let mut query = log::Entity::find();
        query = if let Some(bvid) = bvid {
            query.filter(log::Column::Bvid.eq(bvid))
        } else if let Some(uid) = uid {
            query.filter(log::Column::Uid.eq(uid))
        } else {
            query.filter(log::Column::Uid.is_null())
        };
        Ok(query
            .order_by_desc(log::Column::CreatedAt)
            .limit(limit)
            .all(&self.db)
            .await?)
    }

    pub async fn add_log(&self, uid: Option<&str>, bvid: Option<&str>, message: &str, level: &str) {
        // 先打印控制台日志
        let prefix = uid.map(|u| format!("[博主 {}] ", u)).unwrap_or_default();
        let bvid_prefix = bvid.map(|b| format!("[{}] ", b)).unwrap_or_default();
        let full_msg = format!("{}{}{}", prefix, bvid_prefix, message);
        match level {
            "error" => error!("{}", full_msg),
            "warning" | "warn" => warn!("{}", full_msg),
            "success" => info!("[✓] {}", full_msg),
            _ => info!("{}", full_msg),
        }

        let new_log = log::ActiveModel {
            level: Set(level.to_string()),
            message: Set(message.to_string()),
            uid: Set(uid.map(|s| s.to_string())),
            bvid: Set(bvid.map(|s| s.to_string())),
            created_at: Set(Some(Local::now())),
            ..Default::default()
        };
        // 写入失败重试 3 次：SQLite 在并发写或 WAL checkpoint 时可能短暂返回 SQLITE_BUSY。
        // 短暂数据库故障时重试，避免丢失诊断所需的监控日志。
        // 重试间退避 50/100/200ms，与 SQLite 典型 busy 时长匹配。
        let mut last_err: Option<sea_orm::DbErr> = None;
        for attempt in 0..3u32 {
            match new_log.clone().insert(&self.db).await {
                Ok(_) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    warn!("写入日志失败 (attempt {}/3): {e}", attempt + 1);
                    last_err = Some(e);
                    if attempt < 2 {
                        tokio::time::sleep(StdDuration::from_millis(50 * 2_u64.pow(attempt))).await;
                    }
                }
            }
        }
        if let Some(e) = last_err {
            error!("写入日志重试 3 次仍失败: {e}");
            return;
        }
        if let Err(error) = self.ws.broadcast_log(uid, message, level).await {
            warn!("推送监控日志失败: {error}");
        }
        // 每写入 N 条日志才触发一次清理，避免每条日志都执行 count+delete
        let count = self.log_counter.fetch_add(1, Ordering::SeqCst) + 1;
        if count.is_multiple_of(LOG_CLEANUP_INTERVAL) {
            if let Err(error) = self.cleanup_logs().await {
                warn!("清理监控日志失败: {error}");
            }
        }
    }

    async fn cleanup_logs(&self) -> Result<()> {
        let settings = self.settings_cached().await?;
        let limit = settings
            .get("storage")
            .and_then(|s| s.get("log_limit"))
            .and_then(|v| v.as_i64())
            .unwrap_or(self.config.log_limit);
        let count = log::Entity::find().count(&self.db).await?;
        if count > (limit as u64 * 12 / 10) {
            let to_delete: Vec<i32> = log::Entity::find()
                .select_only()
                .column(log::Column::Id)
                .order_by_desc(log::Column::CreatedAt)
                .offset(limit as u64)
                .limit(10000)
                .into_tuple()
                .all(&self.db)
                .await?;
            if !to_delete.is_empty() {
                log::Entity::delete_many()
                    .filter(log::Column::Id.is_in(to_delete))
                    .exec(&self.db)
                    .await?;
            }
        }
        Ok(())
    }
}
