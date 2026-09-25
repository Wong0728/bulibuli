//! 产物文件识别：判断某任务词根（单P为 bvid，多P为 `{bvid}_p{page}`）的
//! 最终视频/音频产物是否已下载完成，供入队去重与恢复路径复用。

use std::path::{Path, PathBuf};

use super::DownloadManager;

/// 产物文件名严格匹配：仅接受 `{stem}.{ext}` 精确命中或 `{title}_{stem}.{ext}`
/// 标题前缀命中（大小写归一）。要求 stem 与扩展名之间是硬边界，排除前缀/包含误匹配：
/// 裸 `starts_with(stem)` 曾让单P stem 命中多P产物 `{bvid}_p2.mp4`、
/// `{bvid}_p2` 命中 `_p20`，导致缺文件的分P被判已存在而不重下。
pub(super) fn product_file_matches(name: &str, stem: &str, extensions: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    let lower_stem = stem.to_ascii_lowercase();
    extensions.iter().any(|ext| {
        let suffix = format!(".{ext}");
        lower.ends_with(&suffix)
            && (lower.starts_with(&format!("{lower_stem}."))
                || lower.ends_with(&format!("_{lower_stem}{suffix}")))
    })
}

impl DownloadManager {
    /// 判断指定词根的最终产物是否已存在。`stem` 单P为 bvid，多P为 `{bvid}_p{page}`。
    /// 合并产物命名为 `{title}_{stem}.mp4`，匹配规则见 [`product_file_matches`]：
    /// 仅接受精确命中与标题前缀命中，边界判定与 storage.rs 的去重扫描对齐。
    pub(super) async fn find_completed_product(
        dir: &Path,
        stem: &str,
        task_type: &str,
    ) -> Option<PathBuf> {
        let extensions: &[&str] = if task_type == "video" {
            &["mp4", "mkv", "flv", "mov"]
        } else if task_type == "audio" {
            &["m4a", "mp3", "aac", "wav", "flac"]
        } else {
            return None;
        };
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return None;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Some(name) = entry.file_name().into_string().ok() else {
                continue;
            };
            if product_file_matches(&name, stem, extensions) {
                return Some(entry.path());
            }
        }
        None
    }

    pub(super) async fn completed_product_exists(dir: &Path, stem: &str, task_type: &str) -> bool {
        Self::find_completed_product(dir, stem, task_type)
            .await
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::product_file_matches;

    const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "flv", "mov"];

    #[test]
    fn product_match_accepts_exact_and_title_prefixed_hits() {
        // 精确命中：{stem}.{ext}
        assert!(product_file_matches(
            "BV1xx411c7mD.mp4",
            "BV1xx411c7mD",
            VIDEO_EXTS
        ));
        // 标题前缀命中：{title}_{stem}.{ext}
        assert!(product_file_matches(
            "标题_BV1xx411c7mD_p2.mp4",
            "BV1xx411c7mD_p2",
            VIDEO_EXTS
        ));
        // 大小写归一（扩展名大小写不敏感）
        assert!(product_file_matches(
            "BV1xx411c7mD.MP4",
            "BV1xx411c7mD",
            VIDEO_EXTS
        ));
    }

    #[test]
    fn product_match_rejects_prefix_and_page_boundary_collisions() {
        // 单P stem 不得命中其他分P产物
        assert!(!product_file_matches(
            "BV1xx411c7mD_p2.mp4",
            "BV1xx411c7mD",
            VIDEO_EXTS
        ));
        // 分P词根不得命中更大页码（_p20）
        assert!(!product_file_matches(
            "标题_BV1xx411c7mD_p20.mp4",
            "BV1xx411c7mD_p2",
            VIDEO_EXTS
        ));
        assert!(!product_file_matches(
            "BV1xx411c7mD_p20.m4a",
            "BV1xx411c7mD_p2",
            &["m4a"]
        ));
        // 扩展名不匹配
        assert!(!product_file_matches(
            "BV1xx411c7mD.m4s",
            "BV1xx411c7mD",
            VIDEO_EXTS
        ));
    }
}
