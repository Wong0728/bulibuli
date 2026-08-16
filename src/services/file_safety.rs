use crate::error::{AppError, AppResult};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

const MAX_FILENAME_CHARS: usize = 180;
const MAX_UID_DIGITS: usize = 20;
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 已通过目录安全校验的 B 站 UID。
///
/// UID 会参与路径拼接，因此不能在各个调用点直接使用客户端传入的字符串。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedUid(String);

impl ValidatedUid {
    pub(crate) fn parse(raw: &str) -> AppResult<Self> {
        let value = raw.trim();
        if value.is_empty()
            || value.len() > MAX_UID_DIGITS
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || value == "0"
        {
            return Err(AppError::BadRequest(
                "UID 必须是 1 到 20 位正整数".to_string(),
            ));
        }
        Ok(Self(value.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn validate_uid(raw: &str) -> AppResult<ValidatedUid> {
    ValidatedUid::parse(raw)
}

/// 去掉 Windows `canonicalize` 产生的 verbatim 前缀（`\\?\C:\...` / `\\?\UNC\server\...`）。
/// 该前缀只对 Win32 API 有意义，写入数据库或与普通路径做前缀比较会失配，
/// 因此路径在离开安全校验、流向存储/展示之前统一去掉该前缀。
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    path.to_path_buf()
}

pub fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| match character {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            other => other,
        })
        .take(MAX_FILENAME_CHARS)
        .collect();
    let sanitized = sanitized.trim_matches([' ', '.']);
    let stem = sanitized
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if sanitized.is_empty() || WINDOWS_RESERVED_NAMES.contains(&stem.as_str()) {
        "untitled".to_string()
    } else {
        sanitized.to_string()
    }
}

/// 删除当前布局中与 BVID 对应的弹幕、评论和字幕附件。
pub async fn remove_bvid_sidecars(
    base: &Path,
    bvid: &str,
) -> Vec<(PathBuf, Option<std::io::Error>)> {
    let mut candidates = vec![
        base.join(format!("{bvid}.ass")),
        base.join(format!("{bvid}.srt")),
        base.join(format!("{bvid}.vtt")),
    ];
    if let Ok(mut entries) = tokio::fs::read_dir(base).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if is_known_bvid_sidecar(&name, bvid) {
                candidates.push(entry.path());
            }
        }
    }
    let mut results = Vec::new();
    for path in candidates {
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            let error = tokio::fs::remove_file(&path).await.err();
            results.push((path, error));
        }
    }
    results
}

fn is_known_bvid_sidecar(name: &str, bvid: &str) -> bool {
    for (family, extensions) in [
        ("danmaku", &["xml", "json", "txt"][..]),
        ("comments", &["html", "txt"][..]),
    ] {
        for extension in extensions {
            if name == format!("{bvid}_{family}.{extension}") {
                return true;
            }
        }
        let prefix = format!("{bvid}_{family}.");
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some((timestamp, extension)) = suffix.rsplit_once('.') else {
            continue;
        };
        if is_sidecar_archive_timestamp(timestamp) && extensions.contains(&extension) {
            return true;
        }
    }
    false
}

fn is_sidecar_archive_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 19
        && bytes[8] == b'-'
        && bytes[15] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 15) || byte.is_ascii_digit())
}

pub fn render_path_template(
    root: &Path,
    template: &str,
    variables: &HashMap<&str, String>,
) -> AppResult<PathBuf> {
    let template_path = Path::new(template);
    if template_path.is_absolute() {
        return Err(AppError::BadRequest(
            "下载路径模板不能是绝对路径".to_string(),
        ));
    }
    let mut result = root.to_path_buf();
    for component in template_path.components() {
        let Component::Normal(raw) = component else {
            return Err(AppError::BadRequest(
                "下载路径模板不能包含路径穿越".to_string(),
            ));
        };
        let mut segment = raw.to_string_lossy().to_string();
        for (key, value) in variables {
            segment = segment.replace(&format!("{{{key}}}"), value);
        }
        if segment.contains('{') || segment.contains('}') {
            return Err(AppError::BadRequest(format!(
                "下载路径模板包含未知变量: {segment}"
            )));
        }
        result.push(sanitize_filename(&segment));
    }
    ensure_within_root(root, &result)?;
    Ok(result)
}

pub fn ensure_within_root(root: &Path, candidate: &Path) -> AppResult<()> {
    let normalized_root = normalize_without_io(root)?;
    let normalized_candidate = normalize_without_io(candidate)?;
    if !normalized_candidate.starts_with(&normalized_root) {
        return Err(AppError::BadRequest("目标路径超出下载根目录".to_string()));
    }
    Ok(())
}

/// Windows：将文件 DACL 收紧为仅当前用户、SYSTEM 与 Administrators 可访问。
/// 用于配对码等敏感文件——便携目录解压到共享位置时，继承 ACL 可能放宽到其他本机用户。
/// 通过 icacls 实现（系统自带），失败时由调用方决定是否仅告警。
#[cfg(windows)]
pub fn restrict_windows_file_acl(path: &Path) -> AppResult<()> {
    // 移除继承的 ACE，仅保留当前用户完全控制（SYSTEM/管理员按需显式授予）。
    let username = std::env::var("USERNAME").unwrap_or_default();
    if username.is_empty() {
        return Err(AppError::Internal("无法确定当前用户名".to_string()));
    };
    let grant_current = format!("{username}:(F)");
    let status = std::process::Command::new("icacls")
        .arg(path)
        .args([
            "/inheritance:r",
            "/grant:r",
            &grant_current,
            "/grant:r",
            "SYSTEM:(F)",
            "/grant:r",
            "Administrators:(F)",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| AppError::Internal(format!("执行 icacls 失败: {error}")))?;
    if !status.success() {
        return Err(AppError::Internal("icacls 收紧文件 ACL 失败".to_string()));
    }
    Ok(())
}

/// 在词法路径校验之外，再检查现有路径或最近的现有父目录是否仍位于根目录内。
///
/// 这一步用于阻止下载目录中的符号链接把新文件引导到根目录之外。
pub(crate) async fn ensure_existing_within_root(root: &Path, candidate: &Path) -> AppResult<()> {
    ensure_within_root(root, candidate)?;

    let canonical_root = tokio::fs::canonicalize(root).await?;
    let mut probe = candidate.to_path_buf();
    while !tokio::fs::try_exists(&probe).await.unwrap_or(false) {
        if !probe.pop() {
            return Err(AppError::BadRequest(
                "目标路径无法定位到下载根目录".to_string(),
            ));
        }
    }
    let canonical_probe = tokio::fs::canonicalize(&probe).await?;
    if !canonical_probe.starts_with(&canonical_root) {
        return Err(AppError::BadRequest("目标路径超出下载根目录".to_string()));
    }
    Ok(())
}

fn normalize_without_io(path: &Path) -> AppResult<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(AppError::BadRequest("路径包含非法穿越".to_string()));
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

pub async fn atomic_replace(temp: &Path, target: &Path) -> AppResult<()> {
    let parent = target
        .parent()
        .ok_or_else(|| AppError::BadRequest("目标文件没有父目录".to_string()))?;
    if temp.parent() != Some(parent) {
        return Err(AppError::BadRequest(
            "临时文件必须与目标文件位于同一目录".to_string(),
        ));
    }
    let backup = parent.join(format!(
        ".{}.{}.backup",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("target"),
        uuid::Uuid::new_v4()
    ));
    let had_target = tokio::fs::try_exists(target).await?;
    if had_target {
        tokio::fs::rename(target, &backup).await?;
    }
    match tokio::fs::rename(temp, target).await {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(target, std::fs::Permissions::from_mode(0o644)).await?;
            }
            if had_target {
                if let Err(error) = tokio::fs::remove_file(&backup).await {
                    tracing::warn!(path = %backup.display(), %error, "清理原子替换备份失败");
                }
            }
            Ok(())
        }
        Err(error) => {
            if had_target {
                if let Err(rollback_error) = tokio::fs::rename(&backup, target).await {
                    tracing::error!(
                        backup = %backup.display(),
                        target = %target.display(),
                        %rollback_error,
                        "原子替换回滚失败"
                    );
                }
            }
            Err(AppError::Io(error))
        }
    }
}

/// 流式分块计算文件 SHA-256，避免整文件读入内存（数 GB 视频会打爆内存峰值）。
/// 供 verify worker / history 回填 / 下载去重共用。
pub async fn stream_file_sha256(path: &Path) -> AppResult<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    const CHUNK_SIZE: usize = 512 * 1024;
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub async fn ensure_disk_space(path: &Path, expected_bytes: Option<u64>) -> AppResult<()> {
    const RESERVE_BYTES: u64 = 512 * 1024 * 1024;
    let path = path.to_path_buf();
    let available = tokio::task::spawn_blocking(move || fs2::available_space(path))
        .await
        .map_err(|error| AppError::Internal(format!("磁盘空间检查任务失败: {error}")))??;
    let required = expected_bytes
        .map(|bytes| bytes.saturating_add(bytes / 10))
        .unwrap_or(0)
        .saturating_add(RESERVE_BYTES);
    if available < required {
        return Err(AppError::Conflict(format!(
            "磁盘空间不足：需要至少 {required} 字节，当前可用 {available} 字节"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_sanitizer_handles_windows_names() {
        assert_eq!(sanitize_filename("CON.txt"), "untitled");
        assert_eq!(sanitize_filename(" ../a:b?.mp4 "), "_a_b_.mp4");
        assert_eq!(sanitize_filename("\u{0000}"), "untitled");
    }

    #[test]
    fn verbatim_prefix_is_stripped() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\D:\data\video.mp4")),
            PathBuf::from(r"D:\data\video.mp4")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\a.mp4")),
            PathBuf::from(r"\\server\share\a.mp4")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"D:\data\video.mp4")),
            PathBuf::from(r"D:\data\video.mp4")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("downloads/a.mp4")),
            PathBuf::from("downloads/a.mp4")
        );
    }

    #[test]
    fn template_rejects_traversal_and_unknown_variables() {
        let root = Path::new("downloads");
        let mut variables = HashMap::new();
        variables.insert("title", "hello".to_string());
        assert!(render_path_template(root, "../{title}", &variables).is_err());
        assert!(render_path_template(root, "{unknown}", &variables).is_err());
        assert_eq!(
            render_path_template(root, "{title}", &variables).expect("valid template"),
            PathBuf::from("downloads").join("hello")
        );
    }

    #[test]
    fn uid_validation_rejects_path_like_values() {
        assert_eq!(ValidatedUid::parse("12345").unwrap().as_str(), "12345");
        assert_eq!(ValidatedUid::parse(" 12345 ").unwrap().as_str(), "12345");
        for value in [
            "",
            "0",
            "123456789012345678901",
            "../123",
            r"..\123",
            r"C:\123",
            r"\\server\share",
            "１２３",
        ] {
            assert!(
                ValidatedUid::parse(value).is_err(),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_validation_rejects_absolute_unc_parent_and_mixed_separators() {
        let root = Path::new(r"C:\app\downloads");
        for candidate in [
            Path::new(r"C:\app\downloads\..\outside"),
            Path::new(r"C:/app/downloads/../outside"),
            Path::new(r"C:\outside\file.xml"),
            Path::new(r"\\server\share\file.xml"),
        ] {
            assert!(
                ensure_within_root(root, candidate).is_err(),
                "accepted {candidate:?}"
            );
        }
        assert!(ensure_within_root(root, Path::new(r"C:/app/downloads/user/file.xml")).is_ok());
    }

    #[tokio::test]
    async fn stream_sha256_matches_full_read_digest() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("blob.bin");
        let payload = b"streamed sha256";
        tokio::fs::write(&path, payload).await.expect("write blob");
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let expected = format!("{:x}", hasher.finalize());
        assert_eq!(
            stream_file_sha256(&path).await.expect("stream sha256"),
            expected
        );
    }

    #[tokio::test]
    async fn atomic_replace_replaces_existing_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("video.mp4");
        let temp = dir.path().join("video.tmp");
        tokio::fs::write(&target, b"old").await.expect("write old");
        tokio::fs::write(&temp, b"new").await.expect("write new");
        atomic_replace(&temp, &target).await.expect("replace");
        assert_eq!(tokio::fs::read(&target).await.expect("read target"), b"new");
    }

    #[tokio::test]
    async fn removes_only_known_bvid_sidecars() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bvid = "BV1abcdefghij";
        let danmaku = dir.path().join(format!("{bvid}_danmaku.xml"));
        let comments = dir.path().join(format!("{bvid}_comments.html"));
        let archive = dir
            .path()
            .join(format!("{bvid}_danmaku.20260730-123456-789.xml"));
        let similarly_named = dir.path().join(format!("{bvid}_danmaku.notes.xml"));
        let unrelated = dir.path().join("unrelated.xml");
        tokio::fs::write(&danmaku, b"dm").await.expect("danmaku");
        tokio::fs::write(&comments, b"comments")
            .await
            .expect("comments");
        tokio::fs::write(&unrelated, b"keep")
            .await
            .expect("unrelated");
        tokio::fs::write(&archive, b"archive")
            .await
            .expect("archive");
        tokio::fs::write(&similarly_named, b"keep")
            .await
            .expect("similarly named");

        let results = remove_bvid_sidecars(dir.path(), bvid).await;
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, error)| error.is_none()));
        assert!(!danmaku.exists());
        assert!(!comments.exists());
        assert!(!archive.exists());
        assert!(similarly_named.exists());
        assert!(unrelated.exists());
    }
}
