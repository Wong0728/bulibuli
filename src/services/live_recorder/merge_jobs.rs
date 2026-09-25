//! 合并任务的持久化与登记：`live_merge_jobs` 表的读写、收尾阶段的登记/关闭
//! 以及后台合并任务的执行。

use super::ffmpeg_merge::merge_segments_to_mp4_cancelable;
use super::ffmpeg_session::redact_diagnostics;
use super::segments::{
    cleanup_recording_segments, persist_closed_segments, remove_merged_segments,
};
use super::{MergeCancellationMap, MergeJobInfo, MergeJobMap};
use crate::models::live_recording;
use anyhow::Result;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseConnection, QueryResult, Set, Statement};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// 同一录制最多一条活跃合并任务（唯一索引 ux_live_merge_jobs_active_recording）。
pub(super) async fn find_active_merge_job(
    db: &DatabaseConnection,
    recording_id: i32,
) -> Result<Option<MergeJobInfo>> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT id, recording_id, status, progress, error_msg, source_segment_count,
                    cancel_requested, created_at, updated_at
               FROM live_merge_jobs
              WHERE recording_id = ? AND status IN ('queued', 'running', 'cancelling')
              ORDER BY updated_at DESC LIMIT 1"
                .to_owned(),
            [recording_id.into()],
        ))
        .await?;
    row.as_ref().map(merge_job_from_row).transpose()
}

/// finalize 的合并登记：复用 `request_stop` 已建的活跃行（同一录制有唯一索引，
/// 另起一行会冲突），没有才新建。DB 不可用时返回 None，不影响合并本身。
pub(super) async fn register_finalize_merge_job(
    db: &DatabaseConnection,
    merge_jobs: &MergeJobMap,
    recording_id: i32,
    segment_count: usize,
) -> Option<String> {
    let existing = find_active_merge_job(db, recording_id).await.ok().flatten();
    let now = chrono::Utc::now().to_rfc3339();
    let job = match existing {
        Some(mut job) => {
            job.status = "running".to_owned();
            job.progress = 10;
            job.source_segment_count = segment_count;
            job.updated_at = now;
            job
        }
        None => {
            if segment_count == 0 {
                return None;
            }
            MergeJobInfo {
                id: uuid::Uuid::new_v4().simple().to_string(),
                recording_id,
                status: "running".to_owned(),
                progress: 10,
                error: None,
                source_segment_count: segment_count,
                cancel_requested: false,
                created_at: now.clone(),
                updated_at: now,
            }
        }
    };
    let job_id = job.id.clone();
    if let Err(error) = persist_merge_job(db, &job).await {
        warn!(
            recording_id,
            "登记收尾合并任务失败，关机时无法续跑合并: {error}"
        );
        return None;
    }
    merge_jobs.lock().await.insert(job_id.clone(), job);
    Some(job_id)
}

/// 关闭 finalize 的合并任务行。必须在删除源分段之前调用：进程若在之后被杀，
/// 行已是终态，下次启动不会去重跑一次无分段的合并。
pub(super) async fn finish_finalize_merge_job(
    db: &DatabaseConnection,
    merge_jobs: &MergeJobMap,
    merge_cancellations: &MergeCancellationMap,
    job_id: Option<&str>,
    merge_result: &Result<PathBuf>,
    cancellation: Option<&CancellationToken>,
) {
    let Some(job_id) = job_id else {
        return;
    };
    let (status, error) = match merge_result {
        Ok(_) => ("completed", None),
        Err(error) => {
            let status = if cancellation.is_some_and(CancellationToken::is_cancelled) {
                "cancelled"
            } else {
                "failed"
            };
            (status, Some(error.to_string()))
        }
    };
    write_merge_job(merge_jobs, db, job_id, status, 100, error).await;
    merge_cancellations.lock().await.remove(job_id);
}

pub(super) async fn persist_merge_job(db: &DatabaseConnection, job: &MergeJobInfo) -> Result<()> {
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO live_merge_jobs
             (id, recording_id, status, progress, error_msg, source_segment_count,
              cancel_requested, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             status=excluded.status, progress=excluded.progress,
             error_msg=excluded.error_msg, source_segment_count=excluded.source_segment_count,
             cancel_requested=excluded.cancel_requested, updated_at=excluded.updated_at"
            .to_owned(),
        [
            job.id.clone().into(),
            job.recording_id.into(),
            job.status.clone().into(),
            (job.progress as i32).into(),
            job.error.clone().into(),
            (job.source_segment_count as i32).into(),
            (if job.cancel_requested { 1_i32 } else { 0_i32 }).into(),
            job.created_at.clone().into(),
            job.updated_at.clone().into(),
        ],
    ))
    .await?;
    Ok(())
}

pub(super) async fn prune_persisted_merge_jobs(db: &DatabaseConnection) -> Result<()> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "DELETE FROM live_merge_jobs
          WHERE status IN ('completed', 'failed', 'cancelled') AND updated_at < ?"
            .to_owned(),
        [cutoff.into()],
    ))
    .await?;
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        "DELETE FROM live_merge_jobs
          WHERE id IN (
              SELECT id FROM live_merge_jobs
               WHERE status IN ('completed', 'failed', 'cancelled')
               ORDER BY updated_at DESC LIMIT -1 OFFSET 200
          )"
        .to_owned(),
    ))
    .await?;
    Ok(())
}

/// 合并成功后的收尾顺序：先落库认领成品，落库成功才删分段。
/// 先删后落库一旦失败，成品就成了孤儿——记录仍显示产物缺失，而重试合并因
/// 分段已被删除恒报 no segments，用户再也救不回来。
pub(super) async fn persist_merge_result(
    db: &DatabaseConnection,
    recording_id: i32,
    segments: &[PathBuf],
    output: &Path,
) -> Result<()> {
    let update = live_recording::ActiveModel {
        id: Set(recording_id),
        status: Set("stopped".to_owned()),
        output_path: Set(Some(output.to_string_lossy().to_string())),
        error_msg: Set(None),
        is_recoverable: Set(false),
        updated_at: Set(chrono::Utc::now().to_rfc3339()),
        ..Default::default()
    };
    update.update(db).await?;
    persist_closed_segments(db, recording_id, segments).await;
    remove_merged_segments(segments).await;
    cleanup_recording_segments(db, recording_id).await;
    Ok(())
}

pub(super) async fn run_merge_job(
    inner: Arc<super::LiveRecorderInner>,
    job_id: String,
    recording_id: i32,
    ffmpeg_path: PathBuf,
    segments: Vec<PathBuf>,
    cancellation: CancellationToken,
) {
    update_merge_job(&inner, &job_id, "running", 10, None).await;
    let result = merge_segments_to_mp4_cancelable(&ffmpeg_path, &segments, &cancellation).await;
    match result {
        Ok(output) => {
            match persist_merge_result(&inner.db, recording_id, &segments, &output).await {
                Ok(()) => update_merge_job(&inner, &job_id, "completed", 100, None).await,
                Err(error) => {
                    update_merge_job(&inner, &job_id, "failed", 90, Some(error.to_string())).await;
                }
            }
        }
        Err(error) => {
            let status = if cancellation.is_cancelled() {
                "cancelled"
            } else {
                "failed"
            };
            update_merge_job(&inner, &job_id, status, 100, Some(error.to_string())).await;
        }
    }
    inner.merge_cancellations.lock().await.remove(&job_id);
}

pub(super) async fn update_merge_job(
    inner: &Arc<super::LiveRecorderInner>,
    job_id: &str,
    status: &str,
    progress: u8,
    error: Option<String>,
) {
    write_merge_job(
        &inner.merge_jobs,
        &inner.db,
        job_id,
        status,
        progress,
        error,
    )
    .await;
}

/// 录制 worker 只持有 map/db 克隆，不反向依赖 `LiveRecorderInner`。
pub(super) async fn write_merge_job(
    merge_jobs: &MergeJobMap,
    db: &DatabaseConnection,
    job_id: &str,
    status: &str,
    progress: u8,
    error: Option<String>,
) {
    let snapshot = if let Some(job) = merge_jobs.lock().await.get_mut(job_id) {
        job.status = status.to_owned();
        job.progress = progress;
        job.error = error.map(|error| redact_diagnostics(&error));
        job.updated_at = chrono::Utc::now().to_rfc3339();
        Some(job.clone())
    } else {
        None
    };
    if let Some(job) = snapshot {
        if let Err(error) = persist_merge_job(db, &job).await {
            warn!(job_id, "persist merge job failed: {error}");
        }
    }
}

pub(super) fn merge_job_from_row(row: &QueryResult) -> Result<MergeJobInfo> {
    Ok(MergeJobInfo {
        id: row.try_get("", "id")?,
        recording_id: row.try_get("", "recording_id")?,
        status: row.try_get("", "status")?,
        progress: row.try_get::<i32>("", "progress")?.clamp(0, 100) as u8,
        error: row.try_get("", "error_msg")?,
        source_segment_count: row.try_get::<i32>("", "source_segment_count")?.max(0) as usize,
        cancel_requested: row.try_get::<i32>("", "cancel_requested")? != 0,
        created_at: row.try_get("", "created_at")?,
        updated_at: row.try_get("", "updated_at")?,
    })
}
