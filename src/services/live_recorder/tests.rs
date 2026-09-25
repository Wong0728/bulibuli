//! live_recorder 公共辅助与合并任务持久化的单元测试：磁盘闸门、分段行清理、
//! 异常退出状态机、停止原因稳定值、文件名模板与 `live_merge_jobs` 登记口径。
//! （分段清扫相关测试见 `segments::tests`，FFmpeg 合并/探测测试见
//! `ffmpeg_merge::tests` 与 `ffmpeg_probe::tests`。）

use super::merge_jobs::{
    find_active_merge_job, finish_finalize_merge_job, persist_merge_job, persist_merge_result,
    register_finalize_merge_job,
};
use super::segments::{cleanup_recording_segments, find_recording_segments, persist_segment};
use super::startup::{disk_gate, recovery_state, render_file_template, DiskGate};
use super::state::{unexpected_exit_action, UnexpectedExitAction};
use super::*;
use crate::models::live_recording;
use anyhow::anyhow;
use chrono::TimeZone;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseConnection, EntityTrait, Set, Statement};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

type JobMap = Arc<Mutex<HashMap<String, MergeJobInfo>>>;
type CancellationMap = Arc<Mutex<HashMap<String, CancellationToken>>>;

fn empty_job_maps() -> (JobMap, CancellationMap) {
    (
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(Mutex::new(HashMap::new())),
    )
}

#[test]
fn disk_gate_separates_probe_failure_from_genuine_shortage() {
    let threshold = 10 * 1024 * 1024 * 1024;
    // 探测失败绝不能等价为 0 字节可用：否则一次 IO 错误就停掉正常场次
    assert_eq!(disk_gate(None, threshold), DiskGate::ProbeFailed);
    assert_eq!(
        disk_gate(Some(threshold - 1), threshold),
        DiskGate::BelowThreshold
    );
    assert_eq!(disk_gate(Some(threshold), threshold), DiskGate::Ok);
    assert_eq!(disk_gate(Some(0), 0), DiskGate::Ok);
}

#[tokio::test]
async fn merged_recording_segment_rows_are_removed_and_others_kept() {
    let db = migrated_merge_job_db().await;
    let dir = tempfile::tempdir().expect("temp directory");
    let merged = insert_recording(&db, dir.path().join("merged.flv").to_str().expect("utf8")).await;
    let pending =
        insert_recording(&db, dir.path().join("pending.flv").to_str().expect("utf8")).await;
    for index in 0..2 {
        persist_segment(
            &db,
            merged,
            index,
            &dir.path().join(format!("merged_segment_{index:04}.flv")),
            "closed",
        )
        .await
        .expect("写入分段行");
    }
    persist_segment(
        &db,
        pending,
        0,
        &dir.path().join("pending_segment_0000.flv"),
        "open",
    )
    .await
    .expect("写入分段行");

    cleanup_recording_segments(&db, merged).await;

    assert_eq!(
        count_segment_rows(&db, merged).await,
        0,
        "合并成功的分段行应清理"
    );
    assert_eq!(
        count_segment_rows(&db, pending).await,
        1,
        "未合并录制的分段行不能受影响"
    );
}

async fn count_segment_rows(db: &DatabaseConnection, recording_id: i32) -> i64 {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT COUNT(*) AS total FROM live_recording_segments WHERE recording_id = ?"
                .to_owned(),
            [recording_id.into()],
        ))
        .await
        .expect("统计分段行")
        .expect("统计结果");
    row.try_get("", "total").expect("total 列")
}

#[test]
fn unexpected_exit_state_machine_requires_offline_confirmation_for_completion() {
    assert_eq!(
        unexpected_exit_action(Some(true), 0),
        UnexpectedExitAction::CompleteAfterOfflineConfirmation
    );
    assert_eq!(
        unexpected_exit_action(Some(false), 0),
        UnexpectedExitAction::Recover
    );
    assert_eq!(
        unexpected_exit_action(None, 0),
        UnexpectedExitAction::Recover
    );
}

#[test]
fn unexpected_exit_state_machine_marks_exhausted_live_or_unknown_exit_recoverable() {
    assert_eq!(
        unexpected_exit_action(Some(false), MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS),
        UnexpectedExitAction::FailRecoverable
    );
    assert_eq!(
        unexpected_exit_action(None, MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS),
        UnexpectedExitAction::FailRecoverable
    );
}

#[test]
fn recording_stop_reasons_are_stable_database_values() {
    assert_eq!(STOP_REASON_MANUAL, "manual_stop");
    assert_eq!(
        STOP_REASON_OFFLINE_END,
        "stream_ended_after_offline_confirmation"
    );
    assert_eq!(
        STOP_REASON_UNRECOVERABLE_EXIT,
        "ffmpeg_exit_while_live_or_unconfirmed"
    );
}

#[tokio::test]
async fn crash_between_file_replace_and_db_commit_remains_recoverable() {
    use crate::services::file_safety::atomic_replace;

    let directory = tempfile::tempdir().expect("temp directory");
    let output = directory.path().join("room_segment_0001.mp4");
    let temp = directory.path().join(".room_segment_0001.mp4.downloading");
    let segment = directory.path().join("room_segment_0001.flv");
    tokio::fs::write(&temp, b"merged")
        .await
        .expect("temp output");

    // 模拟文件替换完成后、持久化状态从 finalizing 变为 completed 前进程立即停止。
    atomic_replace(&temp, &output)
        .await
        .expect("atomic replace");
    tokio::fs::write(&segment, b"source")
        .await
        .expect("source segment");
    let segments = find_recording_segments(
        directory.path(),
        Some(output.to_str().expect("utf8 output path")),
    )
    .await;

    assert!(output.exists());
    assert_eq!(segments, vec![segment]);
    assert_eq!(
        recovery_state("finalizing", true, segments.len()),
        "segments_pending"
    );
    assert_eq!(
        recovery_state("completed", false, segments.len()),
        "output_missing_recoverable"
    );
    assert_eq!(recovery_state("completed", true, 0), "complete");
}

#[test]
fn file_template_renders_known_tokens() {
    let now = chrono::Local
        .with_ymd_and_hms(2026, 8, 10, 21, 14, 5)
        .unwrap();
    let rendered = render_file_template("{room_id}_{title}_{date}", 8178490, "深夜点歌台", now);
    assert_eq!(rendered, "8178490_深夜点歌台_20260810");
    let with_time = render_file_template("{room_id}_{time}", 732, "标题", now);
    assert_eq!(with_time, "732_211405");
}

#[test]
fn file_template_falls_back_for_empty_result() {
    let now = chrono::Local::now();
    assert_eq!(render_file_template("   ", 123, "标题", now), "live_123");
    assert_eq!(
        render_file_template("{missing}", 456, "标题", now),
        "{missing}"
    );
}

#[tokio::test]
async fn finalize_registers_merge_job_row_for_every_stop_branch() {
    // 同步停止（stop()/关机）没有 request_stop 预建行，finalize 必须自己登记，
    // 否则进程被杀后没人续跑这场合并。
    let db = migrated_merge_job_db().await;
    let (jobs, _cancellations) = empty_job_maps();
    let job_id = register_finalize_merge_job(&db, &jobs, 7, 3)
        .await
        .expect("收尾合并应登记任务行");
    assert_eq!(persisted_status(&db, &job_id).await, "running");
    assert_eq!(
        find_active_merge_job(&db, 7)
            .await
            .expect("find active")
            .expect("关机 abort 后应留在可续跑状态")
            .id,
        job_id
    );
}

#[tokio::test]
async fn finalize_reuses_active_row_instead_of_colliding_on_unique_index() {
    let db = migrated_merge_job_db().await;
    let (jobs, _cancellations) = empty_job_maps();
    let now = chrono::Utc::now().to_rfc3339();
    let pre = MergeJobInfo {
        id: "pre-job".to_owned(),
        recording_id: 9,
        status: "queued".to_owned(),
        progress: 0,
        error: None,
        source_segment_count: 0,
        cancel_requested: false,
        created_at: now.clone(),
        updated_at: now,
    };
    persist_merge_job(&db, &pre)
        .await
        .expect("persist queued job");
    jobs.lock().await.insert(pre.id.clone(), pre.clone());

    let job_id = register_finalize_merge_job(&db, &jobs, 9, 2)
        .await
        .expect("复用已登记的行");
    assert_eq!(job_id, "pre-job");
    assert_eq!(persisted_status(&db, "pre-job").await, "running");
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id FROM live_merge_jobs WHERE recording_id = 9".to_owned(),
        ))
        .await
        .expect("count rows");
    assert_eq!(rows.len(), 1, "同一录制只允许一条合并任务行");
}

#[tokio::test]
async fn completed_merge_closes_row_before_segments_are_removed() {
    let db = migrated_merge_job_db().await;
    let (jobs, cancellations) = empty_job_maps();
    let job_id = register_finalize_merge_job(&db, &jobs, 11, 2)
        .await
        .expect("登记合并任务");
    let ok: Result<PathBuf, anyhow::Error> = Ok(PathBuf::from("room.mp4"));
    finish_finalize_merge_job(&db, &jobs, &cancellations, Some(&job_id), &ok, None).await;
    assert_eq!(persisted_status(&db, &job_id).await, "completed");
    assert!(
        find_active_merge_job(&db, 11)
            .await
            .expect("find active")
            .is_none(),
        "已完成的合并不得在下次启动被重跑"
    );
}

#[tokio::test]
async fn failed_and_cancelled_merges_leave_terminal_rows() {
    let db = migrated_merge_job_db().await;
    let (jobs, cancellations) = empty_job_maps();
    let failed = register_finalize_merge_job(&db, &jobs, 12, 2)
        .await
        .expect("登记合并任务");
    let error: Result<PathBuf, anyhow::Error> = Err(anyhow!("输入分段无效"));
    finish_finalize_merge_job(&db, &jobs, &cancellations, Some(&failed), &error, None).await;
    assert_eq!(persisted_status(&db, &failed).await, "failed");

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let cancelled = register_finalize_merge_job(&db, &jobs, 13, 2)
        .await
        .expect("登记合并任务");
    cancellations
        .lock()
        .await
        .insert(cancelled.clone(), cancellation.clone());
    finish_finalize_merge_job(
        &db,
        &jobs,
        &cancellations,
        Some(&cancelled),
        &error,
        Some(&cancellation),
    )
    .await;
    assert_eq!(persisted_status(&db, &cancelled).await, "cancelled");
    assert!(
        !cancellations.lock().await.contains_key(&cancelled),
        "收尾结束后必须释放取消令牌，避免 map 无限增长"
    );
}

#[tokio::test]
async fn empty_segment_list_registers_no_job() {
    let db = migrated_merge_job_db().await;
    let (jobs, _cancellations) = empty_job_maps();
    assert!(register_finalize_merge_job(&db, &jobs, 14, 0)
        .await
        .is_none());
    assert!(jobs.lock().await.is_empty());
}

#[tokio::test]
async fn merge_result_persisted_before_segments_are_removed() {
    let db = migrated_merge_job_db().await;
    let dir = tempfile::tempdir().expect("temp directory");
    let segments = vec![
        dir.path().join("room_segment_0001.flv"),
        dir.path().join("room_segment_0002.flv"),
    ];
    for segment in &segments {
        std::fs::write(segment, b"segment").expect("write segment");
    }
    let output = dir.path().join("room_segment_0001.mp4");
    std::fs::write(&output, b"merged").expect("write merged output");
    let recording_id = insert_recording(&db, segments[0].to_str().expect("utf8")).await;

    persist_merge_result(&db, recording_id, &segments, &output)
        .await
        .expect("合并结果落库");
    let row = live_recording::Entity::find_by_id(recording_id)
        .one(&db)
        .await
        .expect("查询记录")
        .expect("记录存在");
    assert_eq!(row.status, "stopped");
    assert_eq!(
        row.output_path.as_deref(),
        Some(output.to_str().expect("utf8"))
    );
    assert!(
        segments.iter().all(|segment| !segment.exists()),
        "落库成功后分段才应被清理"
    );
}

#[tokio::test]
async fn failed_merge_persist_keeps_segments_for_retry() {
    let db = migrated_merge_job_db().await;
    let dir = tempfile::tempdir().expect("temp directory");
    let segments = vec![
        dir.path().join("room_segment_0001.flv"),
        dir.path().join("room_segment_0002.flv"),
    ];
    for segment in &segments {
        std::fs::write(segment, b"segment").expect("write segment");
    }
    let output = dir.path().join("room_segment_0001.mp4");
    std::fs::write(&output, b"merged").expect("write merged output");
    let recording_id = insert_recording(&db, segments[0].to_str().expect("utf8")).await;
    // 模拟落库失败（记录已被删除 / 写入被拒）。
    live_recording::Entity::delete_by_id(recording_id)
        .exec(&db)
        .await
        .expect("删除记录");

    let error = persist_merge_result(&db, recording_id, &segments, &output)
        .await
        .expect_err("记录不存在时落库必须失败");
    assert!(
        segments.iter().all(|segment| segment.exists()),
        "落库失败不得删除分段：否则成品成孤儿且重试恒报 no segments，实际: {error}"
    );
    assert_eq!(
        find_recording_segments(dir.path(), Some(segments[0].to_str().expect("utf8"))).await,
        segments,
        "分段必须还能被 retry_merge 找回"
    );
}

async fn migrated_merge_job_db() -> DatabaseConnection {
    let db = sea_orm::Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    <crate::migration::Migrator as sea_orm_migration::MigratorTrait>::up(&db, None)
        .await
        .expect("migrations");
    db
}

async fn persisted_status(db: &DatabaseConnection, job_id: &str) -> String {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT status FROM live_merge_jobs WHERE id = ?".to_owned(),
            [job_id.to_owned().into()],
        ))
        .await
        .expect("query job")
        .expect("job row");
    row.try_get("", "status").expect("status column")
}

async fn insert_recording(db: &DatabaseConnection, output_path: &str) -> i32 {
    let now = chrono::Utc::now().to_rfc3339();
    let row = live_recording::ActiveModel {
        room_id: Set(8178490),
        title: Set("深夜点歌台".to_owned()),
        status: Set("failed".to_owned()),
        output_path: Set(Some(output_path.to_owned())),
        started_at: Set(now.clone()),
        created_at: Set(now.clone()),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("插入录制记录");
    row.id
}
