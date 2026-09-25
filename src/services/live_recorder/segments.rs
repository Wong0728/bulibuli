//! 直播分段文件与录制目录的持久化辅助：分段路径、分段行落库、目录扫描
//! 与合并临时文件清理。

use anyhow::Result;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

pub(super) fn segment_path(live_dir: &Path, base_prefix: &str, index: u32) -> PathBuf {
    live_dir.join(format!("{base_prefix}_segment_{index:04}.flv"))
}

/// 合并失败时的路径回落：保留「分段前缀」让 retry_merge 还能找回分段，但指向
/// 不存在的合并目标而非分段本身——把 `.flv` 分段当成品路径落库会让烧录入口误认
/// 整场已录好，用户拿到的却只有单个分段。
pub(super) fn merge_fallback_output_path(
    segments: &[PathBuf],
    live_dir: &Path,
    base_prefix: &str,
) -> PathBuf {
    segments
        .first()
        .map(|segment| segment.with_extension("mp4"))
        .unwrap_or_else(|| live_dir.join(base_prefix))
}

/// 删除已合并的分段；失败只告警——删不掉不该推翻已经成功的合并。
pub(super) async fn remove_merged_segments(segments: &[PathBuf]) {
    for segment in segments {
        if let Err(error) = tokio::fs::remove_file(segment).await {
            debug!(path = %segment.display(), "删除已合并的直播分段失败: {error}");
        }
    }
}

pub(super) async fn persist_segment(
    db: &DatabaseConnection,
    recording_id: i32,
    segment_index: u32,
    path: &Path,
    status: &str,
) -> Result<()> {
    let file_size = tokio::fs::metadata(path)
        .await
        .map(|metadata| metadata.len() as i64)
        .unwrap_or(0);
    let now = chrono::Utc::now().to_rfc3339();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO live_recording_segments
             (recording_id, segment_index, path, file_size, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(recording_id, segment_index) DO UPDATE SET
             path=excluded.path, file_size=excluded.file_size,
             status=excluded.status, updated_at=excluded.updated_at"
            .to_owned(),
        [
            recording_id.into(),
            (segment_index as i32).into(),
            path.to_string_lossy().to_string().into(),
            file_size.into(),
            status.to_owned().into(),
            now.clone().into(),
            now.into(),
        ],
    ))
    .await?;
    Ok(())
}

pub(super) async fn persist_closed_segments(
    db: &DatabaseConnection,
    recording_id: i32,
    paths: &[PathBuf],
) {
    let ended_at = chrono::Utc::now().to_rfc3339();
    for (index, path) in paths.iter().enumerate() {
        let file_size = tokio::fs::metadata(path)
            .await
            .map(|metadata| metadata.len() as i64)
            .unwrap_or(0);
        let result = db
            .execute_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "UPDATE live_recording_segments
                 SET file_size=?, status='closed', ended_at=?, updated_at=?
                 WHERE recording_id=? AND segment_index=?"
                    .to_owned(),
                [
                    file_size.into(),
                    ended_at.clone().into(),
                    ended_at.clone().into(),
                    recording_id.into(),
                    (index as i32).into(),
                ],
            ))
            .await;
        if let Err(error) = result {
            warn!(
                recording_id,
                segment_index = index,
                "persist closed recording segment failed: {error}"
            );
        }
    }
}

/// 合并成功且分段文件已删除后清理分段行：`live_recording_segments` 表没有外键级联，
/// 也没有任何读取方（抢救路径靠 `find_recording_segments` 扫目录），不清理就是只增不减。
/// 只在合并成功路径调用——合并失败时保留分段行与分段文件，供 retry_merge 与启动续跑使用。
pub(super) async fn cleanup_recording_segments(db: &DatabaseConnection, recording_id: i32) {
    let result = db
        .execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "DELETE FROM live_recording_segments WHERE recording_id = ?".to_owned(),
            [recording_id.into()],
        ))
        .await;
    if let Err(error) = result {
        warn!(recording_id, "清理已合并的直播分段记录失败: {error}");
    }
}

/// 从输出路径推断分段前缀并扫描目录，收集仍存在的 FLV 分段（按文件名排序）。
pub(super) async fn find_recording_segments(
    directory: &Path,
    output_path: Option<&str>,
) -> Vec<PathBuf> {
    let Some(output_path) = output_path else {
        return Vec::new();
    };
    let Some(file_name) = Path::new(output_path)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return Vec::new();
    };
    let Some(prefix) = file_name.split("_segment_").next() else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return result;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with(&format!("{prefix}_segment_")) && name.ends_with(".flv")
            });
        if matches {
            result.push(path);
        }
    }
    result.sort();
    result
}

pub(super) async fn total_file_size(paths: &[PathBuf]) -> u64 {
    let mut total = 0;
    for path in paths {
        total += tokio::fs::metadata(path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
    }
    total
}

/// 合并临时文件：FFmpeg 的中间输出、分段清单，以及互动归档中止时残留的中间文件。
fn is_merge_leftover_name(name: &str) -> bool {
    name.ends_with(".mp4.partial")
        || name.ends_with(super::interaction_archive::ARCHIVE_STAGING_SUFFIX)
        || (name.starts_with('.') && name.contains(".concat-") && name.ends_with(".txt"))
}

/// 删除录制目录里上次运行遗留的合并临时文件，返回删除数量。
/// 只在启动恢复阶段调用：此时不可能有合并在写这些文件。
pub(super) fn sweep_merge_leftovers(root: &Path) -> usize {
    if !root.is_dir() {
        return 0;
    }
    let mut removed = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !is_merge_leftover_name(name) {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) => warn!(
                    path = %path.display(),
                    "清理合并临时文件失败: {error}"
                ),
            }
        }
    }
    removed
}

pub(super) fn count_residual_segments(root: &Path) -> usize {
    if !root.is_dir() {
        return 0;
    }
    let mut count = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("_segment_") && name.ends_with(".flv"))
            {
                count += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_leftover_names_are_recognized() {
        assert!(is_merge_leftover_name("room_segment_0001.mp4.partial"));
        assert!(is_merge_leftover_name(
            ".room_segment_0001.concat-9f3a1b.txt"
        ));
        assert!(is_merge_leftover_name(
            "深夜点歌台_20260925_013200_ab12cd34_danmaku.xml.archive-part"
        ));
        assert!(!is_merge_leftover_name("room_segment_0001.flv"));
        assert!(!is_merge_leftover_name("room_segment_0001.mp4"));
        assert!(!is_merge_leftover_name("room_segment_0001.mp4.unverified"));
        assert!(!is_merge_leftover_name("concat-list.txt"));
        assert!(!is_merge_leftover_name(".room.concat-1.log"));
    }

    #[test]
    fn sweep_removes_only_merge_temporaries() {
        let root = tempfile::tempdir().expect("temp directory");
        let room = root.path().join("8178490");
        std::fs::create_dir_all(&room).expect("create room dir");
        for name in [
            "room_segment_0001.mp4.partial",
            ".room_segment_0001.concat-ab12.txt",
            "room_segment_0001.flv",
            "room_segment_0001.mp4",
            "room_segment_0001.mp4.unverified",
        ] {
            std::fs::write(room.join(name), b"x").expect("write fixture");
        }
        std::fs::write(root.path().join("room_segment_0002.mp4.partial"), b"x")
            .expect("write nested leftover");
        let staging = room.join(format!(
            "room_danmaku.xml{}",
            super::super::interaction_archive::ARCHIVE_STAGING_SUFFIX
        ));
        std::fs::write(&staging, b"x").expect("write aborted archive staging");

        assert_eq!(sweep_merge_leftovers(root.path()), 4);
        assert!(!room.join("room_segment_0001.mp4.partial").exists());
        assert!(!room.join(".room_segment_0001.concat-ab12.txt").exists());
        assert!(!root.path().join("room_segment_0002.mp4.partial").exists());
        assert!(
            !staging.exists(),
            "收尾被 abort 留下的归档中间文件也要被扫掉"
        );
        assert!(room.join("room_segment_0001.flv").exists());
        assert!(room.join("room_segment_0001.mp4").exists());
        assert!(room.join("room_segment_0001.mp4.unverified").exists());
        assert_eq!(sweep_merge_leftovers(root.path()), 0);
        assert_eq!(sweep_merge_leftovers(&root.path().join("不存在")), 0);
    }

    #[tokio::test]
    async fn merge_failure_fallback_does_not_promise_a_finished_output() {
        let dir = tempfile::tempdir().expect("temp directory");
        let first = dir.path().join("room_segment_0001.flv");
        let second = dir.path().join("room_segment_0002.flv");
        std::fs::write(&first, b"segment").expect("write segment");
        std::fs::write(&second, b"segment").expect("write segment");

        let fallback =
            merge_fallback_output_path(&[first.clone(), second.clone()], dir.path(), "room");
        // 回落到不存在的合并目标：记录不再声称自己握着整场成品。
        assert_eq!(fallback, dir.path().join("room_segment_0001.mp4"));
        assert!(!fallback.exists(), "合并失败不得落库一个被当成成品的路径");
        // 但分段前缀必须仍可被找回，抢救能力不能被一并砍掉。
        assert_eq!(
            find_recording_segments(dir.path(), fallback.to_str()).await,
            vec![first.clone(), second.clone()]
        );
        // 没有分段时沿用目录前缀回落。
        assert_eq!(
            merge_fallback_output_path(&[], dir.path(), "room"),
            dir.path().join("room")
        );
    }
}
