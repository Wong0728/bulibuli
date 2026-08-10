use anyhow::{Context, Result};
use chrono::Local;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidecarArchiveMode {
    Overwrite,
    KeepLatestN,
    KeepAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SidecarArchivePolicy {
    mode: SidecarArchiveMode,
    limit: usize,
}

impl SidecarArchivePolicy {
    pub(crate) fn new(mode: &str, limit: i64) -> Self {
        let mode = match mode {
            "keep_latest_n" => SidecarArchiveMode::KeepLatestN,
            "keep_all" => SidecarArchiveMode::KeepAll,
            _ => SidecarArchiveMode::Overwrite,
        };
        Self {
            mode,
            limit: limit.clamp(1, 50) as usize,
        }
    }

    fn should_archive(self) -> bool {
        self.mode != SidecarArchiveMode::Overwrite
    }
}

pub(crate) async fn archive_sidecar_files(
    save_dir: &Path,
    bvid: &str,
    family: &str,
    fixed_paths: &[PathBuf],
    policy: SidecarArchivePolicy,
) -> Result<()> {
    if !policy.should_archive() {
        return Ok(());
    }

    let timestamp = Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    let mut archived = Vec::with_capacity(fixed_paths.len());
    for fixed_path in fixed_paths {
        let extension = fixed_path
            .extension()
            .and_then(|value| value.to_str())
            .context("侧车文件缺少有效扩展名")?;
        let archive_path = save_dir.join(format!("{bvid}_{family}.{timestamp}.{extension}"));
        if let Err(error) = tokio::fs::copy(fixed_path, &archive_path).await {
            for path in archived {
                if let Err(cleanup_error) = tokio::fs::remove_file(path).await {
                    tracing::warn!(%cleanup_error, "清理未完成的弹幕归档失败");
                }
            }
            return Err(error).with_context(|| {
                format!(
                    "归档侧车文件失败: {} -> {}",
                    fixed_path.display(),
                    archive_path.display()
                )
            });
        }
        archived.push(archive_path);
    }

    if policy.mode == SidecarArchiveMode::KeepLatestN {
        prune_archive_groups(save_dir, bvid, family, fixed_paths, policy.limit).await?;
    }
    Ok(())
}

async fn prune_archive_groups(
    save_dir: &Path,
    bvid: &str,
    family: &str,
    fixed_paths: &[PathBuf],
    limit: usize,
) -> Result<()> {
    let allowed_extensions = fixed_paths
        .iter()
        .filter_map(|path| path.extension())
        .filter_map(|extension| extension.to_str())
        .map(str::to_string)
        .collect::<HashSet<_>>();
    let prefix = format!("{bvid}_{family}.");
    let mut groups = BTreeMap::<String, Vec<PathBuf>>::new();
    let mut entries = tokio::fs::read_dir(save_dir)
        .await
        .with_context(|| format!("读取侧车归档目录失败: {}", save_dir.display()))?;
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some((timestamp, extension)) = suffix.rsplit_once('.') else {
            continue;
        };
        if is_archive_timestamp(timestamp) && allowed_extensions.contains(extension) {
            groups
                .entry(timestamp.to_string())
                .or_default()
                .push(entry.path());
        }
    }

    let remove_count = groups.len().saturating_sub(limit);
    for (_, paths) in groups.into_iter().take(remove_count) {
        for path in paths {
            tokio::fs::remove_file(&path)
                .await
                .with_context(|| format!("删除过期侧车归档失败: {}", path.display()))?;
        }
    }
    Ok(())
}

fn is_archive_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 19
        && bytes[8] == b'-'
        && bytes[15] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 15) || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_archive_timestamp_shape() {
        assert!(is_archive_timestamp("20260730-123456-789"));
        assert!(!is_archive_timestamp("20260730-1234"));
        assert!(!is_archive_timestamp("20260730_123456_789"));
    }

    #[tokio::test]
    async fn overwrite_keeps_only_fixed_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let bvid = "BV1abcdefghij";
        let fixed = temp.path().join(format!("{bvid}_comments.html"));
        tokio::fs::write(&fixed, b"latest")
            .await
            .expect("fixed file");

        archive_sidecar_files(
            temp.path(),
            bvid,
            "comments",
            std::slice::from_ref(&fixed),
            SidecarArchivePolicy::new("overwrite", 3),
        )
        .await
        .expect("overwrite policy");

        let mut entries = tokio::fs::read_dir(temp.path()).await.expect("read dir");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("next entry") {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names, vec![format!("{bvid}_comments.html")]);
    }

    #[tokio::test]
    async fn keep_all_creates_timestamped_archive_for_each_format() {
        let temp = tempfile::tempdir().expect("temp dir");
        let bvid = "BV1abcdefghij";
        let fixed_paths = ["xml", "json", "txt"]
            .into_iter()
            .map(|extension| temp.path().join(format!("{bvid}_danmaku.{extension}")))
            .collect::<Vec<_>>();
        for path in &fixed_paths {
            tokio::fs::write(path, b"latest").await.expect("fixed file");
        }

        archive_sidecar_files(
            temp.path(),
            bvid,
            "danmaku",
            &fixed_paths,
            SidecarArchivePolicy::new("keep_all", 3),
        )
        .await
        .expect("keep all");

        let mut entries = tokio::fs::read_dir(temp.path()).await.expect("read dir");
        let mut archive_count = 0;
        while let Some(entry) = entries.next_entry().await.expect("next entry") {
            let name = entry.file_name().to_string_lossy().to_string();
            if name
                .strip_prefix(&format!("{bvid}_danmaku."))
                .and_then(|suffix| suffix.rsplit_once('.'))
                .is_some_and(|(timestamp, _)| is_archive_timestamp(timestamp))
            {
                archive_count += 1;
            }
        }
        assert_eq!(archive_count, 3);
    }

    #[tokio::test]
    async fn keep_latest_n_prunes_complete_timestamp_groups() {
        let temp = tempfile::tempdir().expect("temp dir");
        let bvid = "BV1abcdefghij";
        let fixed_paths = ["xml", "json", "txt"]
            .into_iter()
            .map(|extension| temp.path().join(format!("{bvid}_danmaku.{extension}")))
            .collect::<Vec<_>>();
        for path in &fixed_paths {
            tokio::fs::write(path, b"latest").await.expect("fixed file");
        }
        for timestamp in [
            "20260730-120000-000",
            "20260730-120001-000",
            "20260730-120002-000",
        ] {
            for extension in ["xml", "json", "txt"] {
                tokio::fs::write(
                    temp.path()
                        .join(format!("{bvid}_danmaku.{timestamp}.{extension}")),
                    b"archive",
                )
                .await
                .expect("archive file");
            }
        }

        prune_archive_groups(temp.path(), bvid, "danmaku", &fixed_paths, 2)
            .await
            .expect("prune");

        for extension in ["xml", "json", "txt"] {
            assert!(!temp
                .path()
                .join(format!("{bvid}_danmaku.20260730-120000-000.{extension}"))
                .exists());
            assert!(temp
                .path()
                .join(format!("{bvid}_danmaku.20260730-120001-000.{extension}"))
                .exists());
        }
        assert!(fixed_paths.iter().all(|path| path.exists()));
    }
}
