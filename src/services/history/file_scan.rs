//! 本地文件扫描：轻量侧车状态检测与按 BV 号聚合全部下载产物。

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::services::file_safety::{strip_verbatim_prefix, validate_uid};

use super::{FileEntry, HistoryService, SidecarProbe, SidecarStatus};

impl HistoryService {
    /// 检测当前视频目录中的侧车文件存在性。
    ///
    /// 看板会高频调用此方法，因此只检查 history.file_path 的父目录，不递归下载根目录。
    pub async fn sidecar_status(
        &self,
        bvid: &str,
        uid: Option<&str>,
        video_path: Option<&str>,
    ) -> SidecarStatus {
        let probe = SidecarProbe {
            bvid,
            uid,
            video_path,
        };
        self.sidecar_status_batch(std::slice::from_ref(&probe))
            .await
            .into_iter()
            .next()
            .expect("单元素批量必返回一个结果")
    }

    /// 批量侧车探测：看板整页行先按目录聚合，每个唯一目录只 `read_dir` 一次，
    /// 替代旧版每行最多 8 次逐文件存在性探测（N+1 扇出导致列表页线性劣化）。
    /// 返回顺序与输入顺序一致。
    pub async fn sidecar_status_batch(&self, probes: &[SidecarProbe<'_>]) -> Vec<SidecarStatus> {
        let bases = probes
            .iter()
            .map(|probe| self.sidecar_base(probe))
            .collect::<Vec<_>>();
        let mut dir_names: HashMap<PathBuf, HashSet<String>> = bases
            .iter()
            .cloned()
            .map(|base| (base, HashSet::new()))
            .collect();
        for dir in dir_names.keys().cloned().collect::<Vec<_>>() {
            let names = dir_names.get_mut(&dir).expect("键来自 keys()，必存在");
            let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
                continue;
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Some(name) = entry.file_name().to_str() {
                    names.insert(name.to_string());
                }
            }
        }
        bases
            .iter()
            .zip(probes)
            .map(|(base, probe)| sidecar_from_names(probe, &dir_names[base]))
            .collect()
    }

    fn sidecar_base(&self, probe: &SidecarProbe<'_>) -> PathBuf {
        probe
            .video_path
            .and_then(|path| Path::new(path).parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| uid_download_dir(&self.paths.download_dir, probe.uid))
    }

    /// 把绝对路径转成相对 `data/downloads/` 的路径。
    /// 兼容历史数据里由 `canonicalize` 写入的 `\\?\` verbatim 前缀，去掉后再比较。
    pub fn to_relative_path(&self, abs: &str) -> Option<String> {
        let path = strip_verbatim_prefix(Path::new(abs));
        match path.strip_prefix(&self.paths.download_dir) {
            Ok(relative) => Some(relative.to_string_lossy().replace('\\', "/")),
            Err(_) => None,
        }
    }

    /// 扫描下载根目录内与指定 BV 号关联的全部产物。
    ///
    /// 该方法只在打开单视频详情或删除记录时调用。它使用显式目录栈且不跟随符号链接，
    /// 兼容 `{uid}/{title}`、`manual/{title}` 以及更深的自定义路径模板。
    pub async fn scan_files(
        &self,
        bvid: &str,
        _uid: Option<&str>,
        video_path: Option<&str>,
    ) -> Vec<FileEntry> {
        let current_video = video_path.map(PathBuf::from);
        let current_dir = current_video
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let mut files = Vec::new();
        let mut directories = vec![self.paths.download_dir.clone()];

        while let Some(directory) = directories.pop() {
            let Ok(mut entries) = tokio::fs::read_dir(&directory).await else {
                continue;
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let Ok(file_type) = entry.file_type().await else {
                    continue;
                };
                if file_type.is_symlink() {
                    continue;
                }
                let path = entry.path();
                if file_type.is_dir() {
                    directories.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                    continue;
                };
                if name.ends_with(".downloading") {
                    continue;
                }
                if let Some(file) = self
                    .classify_file(
                        bvid,
                        name,
                        &path,
                        current_video.as_deref(),
                        current_dir.as_deref(),
                    )
                    .await
                {
                    files.push(file);
                }
            }
        }

        files.sort_by(|left, right| {
            file_type_order(&left.file_type)
                .cmp(&file_type_order(&right.file_type))
                .then_with(|| right.is_current.cmp(&left.is_current))
                .then_with(|| right.modified_at.cmp(&left.modified_at))
                .then_with(|| left.path.cmp(&right.path))
        });
        files
    }

    /// 把接口返回的相对路径解析回下载根目录中的文件。
    ///
    /// 只接受普通路径组件，并再次校验文件确实位于下载根目录内。
    pub fn resolve_download_relative_path(&self, relative: &str) -> Option<PathBuf> {
        let relative = Path::new(relative);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return None;
        }
        let candidate = self.paths.download_dir.join(relative);
        candidate
            .starts_with(&self.paths.download_dir)
            .then_some(candidate)
    }

    async fn classify_file(
        &self,
        bvid: &str,
        name: &str,
        path: &Path,
        current_video: Option<&Path>,
        current_dir: Option<&Path>,
    ) -> Option<FileEntry> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_lowercase)?;
        let video_exts = ["mp4", "m4s", "flv", "mkv", "mov"];
        let audio_exts = ["m4a", "mp3", "aac", "wav", "flac"];
        let image_exts = ["jpg", "jpeg", "png", "webp", "bmp"];

        let (file_type, version) = if video_exts.contains(&extension.as_str())
            && (name.starts_with(bvid) || name.contains(&format!("_{bvid}")))
        {
            (
                if name.contains("_弹幕版") {
                    "danmaku_video"
                } else {
                    "video"
                },
                None,
            )
        } else if audio_exts.contains(&extension.as_str()) && name.starts_with(bvid) {
            ("audio", None)
        } else if name.starts_with(&format!("{bvid}_cover."))
            || (image_exts.contains(&extension.as_str()) && name == format!("{bvid}.{extension}"))
        {
            ("cover", None)
        } else if let Some(version) = sidecar_version(name, bvid, "comments", &["html", "txt"]) {
            ("comment", version)
        } else if let Some(version) =
            sidecar_version(name, bvid, "danmaku", &["xml", "json", "txt"])
        {
            ("danmaku", version)
        } else if matches!(extension.as_str(), "srt" | "ass" | "vtt") && name.starts_with(bvid) {
            ("subtitle", None)
        } else {
            return None;
        };

        let parent_is_current =
            current_dir.is_some_and(|directory| path.parent() == Some(directory));
        let is_current = if file_type == "video" {
            current_video.is_some_and(|current| path == current)
        } else {
            parent_is_current && version.is_none()
        };
        self.file_entry(file_type, name, path, Some(&extension), is_current, version)
            .await
    }

    async fn file_entry(
        &self,
        file_type: &str,
        name: &str,
        path: &Path,
        format: Option<&str>,
        is_current: bool,
        version: Option<String>,
    ) -> Option<FileEntry> {
        let metadata = tokio::fs::metadata(path).await.ok();
        let size = metadata
            .as_ref()
            .map(|value| i64::try_from(value.len()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        let modified_at = metadata
            .and_then(|value| value.modified().ok())
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .and_then(|value| i64::try_from(value.as_secs()).ok());
        let relative = self.to_relative_path(&path.to_string_lossy())?;
        Some(FileEntry {
            file_type: file_type.to_string(),
            name: name.to_string(),
            location: file_location(&relative),
            path: relative,
            size,
            format: format.map(str::to_string),
            is_current,
            version,
            modified_at,
        })
    }
}

/// 从目录文件名集合推导侧车存在性，与逐文件 `try_exists` 语义一致。
fn sidecar_from_names(probe: &SidecarProbe<'_>, names: &HashSet<String>) -> SidecarStatus {
    let has_any = |stem: &str, exts: &[&str]| {
        exts.iter()
            .any(|ext| names.contains(&format!("{stem}.{ext}")))
    };
    let video = match probe.video_path {
        Some(path) => Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| names.contains(name)),
        None => has_any(probe.bvid, &["m4s", "mp4", "flv"]),
    };
    SidecarStatus {
        video,
        danmaku: has_any(&format!("{}_danmaku", probe.bvid), &["xml", "json", "txt"]),
        comments: names.contains(&format!("{}_comments.html", probe.bvid))
            || names.contains(&format!("{}_comments.txt", probe.bvid)),
        subtitle: has_any(probe.bvid, &["srt", "ass", "vtt"]),
    }
}

fn uid_download_dir(download_dir: &Path, uid: Option<&str>) -> PathBuf {
    uid.and_then(|raw| validate_uid(raw).ok())
        .map(|validated| download_dir.join(validated.as_str()))
        .unwrap_or_else(|| download_dir.to_path_buf())
}

fn file_type_order(file_type: &str) -> u8 {
    match file_type {
        "video" | "danmaku_video" => 0,
        "audio" => 1,
        "cover" => 2,
        "danmaku" => 3,
        "comment" => 4,
        "subtitle" => 5,
        _ => 6,
    }
}

fn sidecar_version(
    name: &str,
    bvid: &str,
    family: &str,
    allowed_extensions: &[&str],
) -> Option<Option<String>> {
    let prefix = format!("{bvid}_{family}.");
    let suffix = name.strip_prefix(&prefix)?;
    let (stem, extension) = suffix.rsplit_once('.').unwrap_or(("", suffix));
    if !allowed_extensions.contains(&extension) && !allowed_extensions.contains(&suffix) {
        return None;
    }
    if allowed_extensions.contains(&suffix) {
        return Some(None);
    }
    is_archive_timestamp(stem).then(|| Some(stem.to_string()))
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

fn file_location(relative: &str) -> String {
    match Path::new(relative).components().next() {
        Some(Component::Normal(value)) if value == "manual" => "manual".to_string(),
        Some(Component::Normal(value)) => {
            let value = value.to_string_lossy();
            if value.chars().all(|character| character.is_ascii_digit()) {
                format!("auto:{value}")
            } else {
                format!("other:{value}")
            }
        }
        _ => "other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{file_location, sidecar_version, uid_download_dir};
    use std::path::Path;

    #[test]
    fn invalid_uid_never_changes_download_root() {
        let root = Path::new("downloads");
        assert_eq!(uid_download_dir(root, Some("12345")), root.join("12345"));
        assert_eq!(uid_download_dir(root, Some("../outside")), root);
        assert_eq!(uid_download_dir(root, Some(r"C:\outside")), root);
        assert_eq!(uid_download_dir(root, None), root);
    }

    #[test]
    fn parses_fixed_and_archived_sidecars() {
        let bvid = "BV1abcdefghij";
        assert_eq!(
            sidecar_version(
                &format!("{bvid}_comments.html"),
                bvid,
                "comments",
                &["html", "txt"]
            ),
            Some(None)
        );
        assert_eq!(
            sidecar_version(
                &format!("{bvid}_danmaku.20260730-123456-789.json"),
                bvid,
                "danmaku",
                &["xml", "json", "txt"]
            ),
            Some(Some("20260730-123456-789".to_string()))
        );
    }

    #[test]
    fn labels_manual_and_uid_locations() {
        assert_eq!(file_location("manual/title/file.mp4"), "manual");
        assert_eq!(
            file_location("1556651916/title/file.mp4"),
            "auto:1556651916"
        );
    }
}
