//! 合并任务管理（impl LiveRecorder）：查询、重试、取消与启动续跑。

use super::merge_jobs::{
    find_active_merge_job as find_active_merge_job_in_db, merge_job_from_row, persist_merge_job,
    prune_persisted_merge_jobs, run_merge_job, update_merge_job,
};
use super::segments::find_recording_segments;
use super::{LiveRecorder, MergeJobInfo, FINALIZE_SHUTDOWN_BUDGET};
use crate::models::live_recording;
use anyhow::{anyhow, Context, Result};
use sea_orm::{ConnectionTrait, EntityTrait, Statement};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use tracing::warn;

impl LiveRecorder {
    pub async fn merge_jobs(&self) -> Vec<MergeJobInfo> {
        match self
            .inner
            .db
            .query_all_raw(Statement::from_string(
                self.inner.db.get_database_backend(),
                "SELECT id, recording_id, status, progress, error_msg, source_segment_count,
                        cancel_requested, created_at, updated_at
                   FROM live_merge_jobs
                  ORDER BY updated_at DESC
                  LIMIT 200"
                    .to_owned(),
            ))
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| match merge_job_from_row(&row) {
                    Ok(job) => Some(job),
                    Err(error) => {
                        warn!("read persisted merge job failed: {error}");
                        None
                    }
                })
                .collect(),
            Err(error) => {
                warn!("read persisted merge jobs failed: {error}");
                Vec::new()
            }
        }
    }

    pub async fn merge_job(&self, job_id: &str) -> Option<MergeJobInfo> {
        if let Some(job) = self.inner.merge_jobs.lock().await.get(job_id).cloned() {
            return Some(job);
        }
        let row = self
            .inner
            .db
            .query_one_raw(Statement::from_sql_and_values(
                self.inner.db.get_database_backend(),
                "SELECT id, recording_id, status, progress, error_msg, source_segment_count,
                        cancel_requested, created_at, updated_at
                   FROM live_merge_jobs WHERE id = ?"
                    .to_owned(),
                [job_id.to_owned().into()],
            ))
            .await
            .ok()??;
        merge_job_from_row(&row).ok()
    }

    /// 在后台重建失败或可恢复的录制任务。
    /// 只有 FFmpeg 和 ffprobe 都成功后，源 FLV 分段才会删除。
    pub async fn retry_merge(&self, recording_id: i32) -> Result<MergeJobInfo> {
        let row = live_recording::Entity::find_by_id(recording_id)
            .one(&self.inner.db)
            .await?
            .ok_or_else(|| anyhow!("recording not found"))?;
        let segment_dir = self
            .inner
            .paths
            .download_dir
            .join("live")
            .join(row.room_id.to_string());
        let segments = find_recording_segments(&segment_dir, row.output_path.as_deref()).await;
        if segments.is_empty() {
            return Err(anyhow!("no recoverable recording segments found"));
        }
        let ffmpeg_path = self
            .inner
            .video_processor
            .detect_ffmpeg_for_live()
            .await
            .context("直播分段合并需要支持网络流的完整版 FFmpeg")?;
        if let Some(job) = self.find_active_merge_job(recording_id).await? {
            return Ok(job);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let job = MergeJobInfo {
            id: uuid::Uuid::new_v4().simple().to_string(),
            recording_id,
            status: "queued".to_owned(),
            progress: 0,
            error: None,
            source_segment_count: segments.len(),
            cancel_requested: false,
            created_at: now.clone(),
            updated_at: now,
        };
        self.start_merge_job(job, ffmpeg_path, segments).await
    }

    pub(super) async fn find_active_merge_job(
        &self,
        recording_id: i32,
    ) -> Result<Option<MergeJobInfo>> {
        find_active_merge_job_in_db(&self.inner.db, recording_id).await
    }

    pub(super) async fn start_merge_job(
        &self,
        job: MergeJobInfo,
        ffmpeg_path: PathBuf,
        segments: Vec<PathBuf>,
    ) -> Result<MergeJobInfo> {
        let cancellation = CancellationToken::new();
        self.inner
            .merge_jobs
            .lock()
            .await
            .insert(job.id.clone(), job.clone());
        if let Err(error) = persist_merge_job(&self.inner.db, &job).await {
            self.inner.merge_jobs.lock().await.remove(&job.id);
            if let Some(existing) = self.find_active_merge_job(job.recording_id).await? {
                return Ok(existing);
            }
            return Err(error);
        }
        self.inner
            .merge_cancellations
            .lock()
            .await
            .insert(job.id.clone(), cancellation.clone());
        let inner = self.inner.clone();
        let task_cancellation = cancellation.clone();
        let accepted = self.inner.background_tasks.spawn_with_budget(
            "live_merge_job",
            run_merge_job(
                inner,
                job.id.clone(),
                job.recording_id,
                ffmpeg_path,
                segments,
                task_cancellation,
            ),
            FINALIZE_SHUTDOWN_BUDGET,
        );
        if !accepted {
            cancellation.cancel();
            self.inner.merge_cancellations.lock().await.remove(&job.id);
            update_merge_job(
                &self.inner,
                &job.id,
                "failed",
                100,
                Some("应用正在关闭，合并任务未启动".to_owned()),
            )
            .await;
            return Err(anyhow!("应用正在关闭，无法启动直播合并任务"));
        }
        Ok(job)
    }

    pub async fn cancel_merge(&self, job_id: &str) -> Result<MergeJobInfo> {
        if !self.inner.merge_jobs.lock().await.contains_key(job_id) {
            let job = self
                .merge_job(job_id)
                .await
                .ok_or_else(|| anyhow!("merge job not found"))?;
            self.inner
                .merge_jobs
                .lock()
                .await
                .insert(job_id.to_owned(), job);
        }
        let mut jobs = self.inner.merge_jobs.lock().await;
        let job = jobs
            .get_mut(job_id)
            .ok_or_else(|| anyhow!("merge job not found"))?;
        if matches!(job.status.as_str(), "completed" | "failed" | "cancelled") {
            return Ok(job.clone());
        }
        job.cancel_requested = true;
        job.status = "cancelling".to_owned();
        job.updated_at = chrono::Utc::now().to_rfc3339();
        let result = job.clone();
        drop(jobs);
        if let Err(error) = persist_merge_job(&self.inner.db, &result).await {
            warn!(job_id, "persist merge cancellation failed: {error}");
        }
        if let Some(cancellation) = self.inner.merge_cancellations.lock().await.get(job_id) {
            cancellation.cancel();
        }
        Ok(result)
    }

    pub async fn restore_merge_jobs(&self) -> Result<()> {
        prune_persisted_merge_jobs(&self.inner.db).await?;
        let rows = self
            .inner
            .db
            .query_all_raw(Statement::from_string(
                self.inner.db.get_database_backend(),
                "SELECT id, recording_id, status, progress, error_msg, source_segment_count,
                        cancel_requested, created_at, updated_at
                   FROM live_merge_jobs
                  WHERE status IN ('queued', 'running', 'cancelling')
                  ORDER BY updated_at ASC"
                    .to_owned(),
            ))
            .await?;
        for row in rows {
            let mut job = merge_job_from_row(&row)?;
            let Some(recording) = live_recording::Entity::find_by_id(job.recording_id)
                .one(&self.inner.db)
                .await?
            else {
                job.status = "failed".to_owned();
                job.progress = 100;
                job.error = Some("recording not found after restart".to_owned());
                job.updated_at = chrono::Utc::now().to_rfc3339();
                persist_merge_job(&self.inner.db, &job).await?;
                continue;
            };
            let directory = self
                .inner
                .paths
                .download_dir
                .join("live")
                .join(recording.room_id.to_string());
            let segments =
                find_recording_segments(&directory, recording.output_path.as_deref()).await;
            let ffmpeg_path = match self.inner.video_processor.detect_ffmpeg_for_live().await {
                Ok(path) => path,
                Err(error) => {
                    job.status = "failed".to_owned();
                    job.progress = 100;
                    job.error = Some(error.to_string());
                    job.updated_at = chrono::Utc::now().to_rfc3339();
                    persist_merge_job(&self.inner.db, &job).await?;
                    continue;
                }
            };
            if segments.is_empty() {
                job.status = "failed".to_owned();
                job.progress = 100;
                job.error = Some("no recoverable recording segments found".to_owned());
                job.updated_at = chrono::Utc::now().to_rfc3339();
                persist_merge_job(&self.inner.db, &job).await?;
                continue;
            }
            self.inner
                .merge_jobs
                .lock()
                .await
                .insert(job.id.clone(), job.clone());
            let cancellation = CancellationToken::new();
            if job.status == "cancelling" || job.cancel_requested {
                cancellation.cancel();
            }
            self.inner
                .merge_cancellations
                .lock()
                .await
                .insert(job.id.clone(), cancellation.clone());
            let task_cancellation = cancellation.clone();
            let accepted = self.inner.background_tasks.spawn_with_budget(
                "live_merge_job",
                run_merge_job(
                    self.inner.clone(),
                    job.id.clone(),
                    recording.id,
                    ffmpeg_path,
                    segments,
                    task_cancellation,
                ),
                FINALIZE_SHUTDOWN_BUDGET,
            );
            if !accepted {
                cancellation.cancel();
                self.inner.merge_cancellations.lock().await.remove(&job.id);
                update_merge_job(
                    &self.inner,
                    &job.id,
                    "failed",
                    100,
                    Some("应用正在关闭，恢复的合并任务未启动".to_owned()),
                )
                .await;
            }
        }
        Ok(())
    }
}
