//! 状态与历史查询（impl LiveRecorder）：会话快照、事件流、历史记录与
//! 启动时的崩溃恢复扫描。

use super::ffmpeg_session::redact_diagnostics;
use super::segments::{count_residual_segments, find_recording_segments, sweep_merge_leftovers};
use super::startup::recovery_state;
use super::{LiveRecorder, RecordingInfo, RecordingStatus};
use crate::models::live_recording;
use anyhow::Result;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use std::path::Path;
use tracing::{info, warn};

impl LiveRecorder {
    /// 获取所有活跃录制会话的快照，不持有 sessions 锁执行文件或网络 I/O。
    pub async fn status_all(&self) -> Vec<RecordingInfo> {
        let handles = {
            let sessions = self.inner.sessions.lock().await;
            sessions
                .values()
                .map(|entry| match entry {
                    super::SessionEntry::Active(handle) => handle.snapshot.clone(),
                    super::SessionEntry::Starting { snapshot, .. } => snapshot.clone(),
                    super::SessionEntry::Stopping { snapshot, .. } => snapshot.clone(),
                })
                .collect::<Vec<_>>()
        };

        let mut result = Vec::with_capacity(handles.len());
        for snapshot in handles {
            result.push(snapshot.lock().await.clone());
        }
        result
    }

    pub async fn status(&self, room_id: i64) -> Option<RecordingInfo> {
        let snapshot = {
            let sessions = self.inner.sessions.lock().await;
            sessions.get(&room_id).map(|entry| match entry {
                super::SessionEntry::Active(handle) => handle.snapshot.clone(),
                super::SessionEntry::Starting { snapshot, .. } => snapshot.clone(),
                super::SessionEntry::Stopping { snapshot, .. } => snapshot.clone(),
            })
        }?;
        let info = snapshot.lock().await.clone();
        Some(info)
    }

    pub async fn events(
        &self,
        room_id: i64,
        after_seq: u64,
        limit: usize,
    ) -> Vec<super::ArchivedLiveEvent> {
        let recent = {
            let sessions = self.inner.sessions.lock().await;
            match sessions.get(&room_id) {
                Some(super::SessionEntry::Active(handle)) => Some(handle.recent.clone()),
                _ => None,
            }
        };
        let Some(recent) = recent else {
            return Vec::new();
        };
        let events = recent
            .lock()
            .await
            .iter()
            .filter(|event| event.seq > after_seq)
            .take(limit.min(100))
            .cloned()
            .collect();
        events
    }

    /// 已完成和失败的会话都会作为正式业务数据保留；调用方读取数据库记录，
    /// 而不是临时会话对象。
    pub async fn history(&self, limit: usize) -> Result<Vec<live_recording::Model>> {
        Ok(live_recording::Entity::find()
            .order_by_desc(live_recording::Column::StartedAt)
            .limit(limit.clamp(1, 100) as u64)
            .all(&self.inner.db)
            .await?)
    }

    pub async fn history_item(&self, recording_id: i32) -> Result<Option<live_recording::Model>> {
        Ok(live_recording::Entity::find_by_id(recording_id)
            .one(&self.inner.db)
            .await?)
    }

    pub async fn recovery_items(&self) -> Result<Vec<serde_json::Value>> {
        let rows = live_recording::Entity::find()
            .order_by_desc(live_recording::Column::StartedAt)
            .limit(100)
            .all(&self.inner.db)
            .await?;
        let mut result = Vec::new();
        for row in rows {
            let dir = self
                .inner
                .paths
                .download_dir
                .join("live")
                .join(row.room_id.to_string());
            let count = find_recording_segments(&dir, row.output_path.as_deref())
                .await
                .len();
            if row.is_recoverable || count > 0 {
                let has_output = row
                    .output_path
                    .as_deref()
                    .is_some_and(|path| Path::new(path).exists());
                result.push(serde_json::json!({
                    "recording_id": row.id, "room_id": row.room_id, "title": row.title,
                    "status": row.status, "segment_count": count,
                    "has_output": has_output,
                    "recovery_state": recovery_state(&row.status, has_output, count),
                    "is_recoverable": row.is_recoverable,
                    "error_msg": row.error_msg.as_deref().map(redact_diagnostics),
                }));
            }
        }
        Ok(result)
    }

    /// 将上次崩溃后仍处于运行态的会话标记为失败，并报告残留 FLV 分段供人工恢复，
    /// 避免把它们继续伪装成活动录制。
    pub async fn recover_incomplete_records(&self) -> Result<()> {
        let rows = live_recording::Entity::find()
            .filter(live_recording::Column::Status.is_in(["starting", "recording", "stopping"]))
            .all(&self.inner.db)
            .await?;
        let now = chrono::Utc::now().to_rfc3339();
        for row in rows {
            live_recording::ActiveModel {
                id: Set(row.id),
                status: Set(RecordingStatus::Failed.to_string()),
                ended_at: Set(Some(now.clone())),
                error_msg: Set(Some(
                    "previous process ended before recording finalized".to_owned(),
                )),
                updated_at: Set(now.clone()),
                ..Default::default()
            }
            .update(&self.inner.db)
            .await?;
        }

        let live_root = self.inner.paths.download_dir.join("live");
        let residual_count = count_residual_segments(&live_root);
        if residual_count > 0 {
            warn!(
                residual_count,
                root = %live_root.display(),
                "发现未合并的直播 FLV 分段，已保留文件供恢复"
            );
        }
        // 合并中途被杀留下的临时文件（含被 abort 的收尾合并）没人会再用，
        // 留到下次开机一并清掉；*.mp4.unverified 是待抢救产物，不在清理范围。
        let leftovers = sweep_merge_leftovers(&live_root);
        if leftovers > 0 {
            info!(
                leftovers,
                root = %live_root.display(),
                "已清理上次遗留的合并临时文件"
            );
        }
        Ok(())
    }
}
