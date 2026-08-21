//! ffmpeg 可执行文件探测与可用性校验（env / system / embedded / custom / auto）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use super::VideoProcessor;

const LIVE_FFMPEG_UNAVAILABLE: &str =
    "未找到支持直播录制的 FFmpeg（需支持 http 协议、FLV 封装与 concat 拼接）：请安装完整版 FFmpeg 或在设置中指定路径";

fn windows_exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

impl VideoProcessor {
    pub async fn detect_ffmpeg(
        &self,
        mode: &str,
        custom_path: Option<&str>,
    ) -> (Option<PathBuf>, String) {
        match mode {
            "env" => {
                if let Some(p) = Self::ffmpeg_from_env() {
                    return (Some(p), "env".to_string());
                }
            }
            "system" => {
                if let Ok(p) = which::which("ffmpeg") {
                    return (Some(p), "system".to_string());
                }
            }
            "embedded" => {
                if let Some(p) = self.embedded_ffmpeg() {
                    return (Some(p), "embedded".to_string());
                }
            }
            "custom" => {
                if let Some(p) = custom_path {
                    let pb = PathBuf::from(p);
                    if pb.is_file() {
                        return (Some(pb), "custom".to_string());
                    }
                }
            }
            _ => {
                // auto: custom > embedded > env > system。用户显式指定的完整版优先，
                // 否则用包内版本（发布包自带、合并行为可控），最后才落回环境变量与 PATH。
                if let Some(p) = custom_path {
                    let pb = PathBuf::from(p);
                    if pb.is_file() {
                        return (Some(pb), "custom".to_string());
                    }
                }
                if let Some(p) = self.embedded_ffmpeg() {
                    return (Some(p), "embedded".to_string());
                }
                if let Some(p) = Self::ffmpeg_from_env() {
                    return (Some(p), "env".to_string());
                }
                if let Ok(p) = which::which("ffmpeg") {
                    return (Some(p), "system".to_string());
                }
            }
        }
        (None, "unknown".to_string())
    }

    /// 直播录制需要支持网络流（http 协议）、FLV 封装与 concat 拼接的 FFmpeg。
    /// 精简构建（如 `--disable-network`，仅 file/pipe 协议）不能用于直播：进程会因
    /// 不认识 `-user_agent` 等选项立即退出。按 auto 顺序探测候选，结果缓存；
    /// 全部不合格时返回明确错误。
    pub async fn detect_ffmpeg_for_live(&self) -> anyhow::Result<PathBuf> {
        if let Some(cached) = self.live_ffmpeg_cache.lock().await.clone() {
            return cached.ok_or_else(|| anyhow::anyhow!(LIVE_FFMPEG_UNAVAILABLE));
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(value) = self.custom_ffmpeg_path.clone() {
            if let Some(path) = Self::ffmpeg_path_from_value(&value, windows_exe_name()) {
                candidates.push(path);
            }
        }
        if let Some(path) = self.embedded_ffmpeg() {
            candidates.push(path);
        }
        if let Some(path) = Self::ffmpeg_from_env() {
            candidates.push(path);
        }
        if let Ok(path) = which::which("ffmpeg") {
            candidates.push(path);
        }
        let mut capable = None;
        for candidate in &candidates {
            if Self::supports_live_capture(candidate).await {
                capable = Some(candidate.clone());
                break;
            }
        }
        *self.live_ffmpeg_cache.lock().await = Some(capable.clone());
        capable.ok_or_else(|| anyhow::anyhow!(LIVE_FFMPEG_UNAVAILABLE))
    }

    /// 依次校验 http 协议、FLV muxer/demuxer 与 concat demuxer 是否存在。
    async fn supports_live_capture(path: &Path) -> bool {
        for (arg, needle) in [
            ("-protocols", "http"),
            ("-muxers", "flv"),
            ("-demuxers", "concat"),
        ] {
            let Ok(output) = Command::new(path)
                .args(["-hide_banner", arg])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
            else {
                return false;
            };
            if !output.status.success() {
                return false;
            }
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            // 能力清单按列输出（如 " flv FLV (Flash Video)"），按整词匹配避免误报。
            if !text.split_whitespace().any(|word| word == needle) {
                return false;
            }
        }
        true
    }

    fn ffmpeg_from_env() -> Option<PathBuf> {
        for name in [
            "FFMPEG_PATH",
            "FFMPEG",
            "FF_PATH",
            "FFMPEG_HOME",
            "FFMPEG_DIR",
        ] {
            if let Ok(val) = std::env::var(name) {
                let exe_name = if cfg!(target_os = "windows") {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                };
                if let Some(path) = Self::ffmpeg_path_from_value(&val, exe_name) {
                    return Some(path);
                }
            }
        }
        None
    }

    fn ffmpeg_path_from_value(value: &str, exe_name: &str) -> Option<PathBuf> {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
        let candidate = path.join(exe_name);
        candidate.is_file().then_some(candidate)
    }

    fn embedded_ffmpeg(&self) -> Option<PathBuf> {
        // 按平台选择可执行文件名，Linux/Termux 下无 .exe 后缀
        let binary_name = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        let p = self.paths.app_root.join("resources").join(binary_name);
        if p.is_file() {
            return Some(p);
        }
        if let Ok(exe) = std::env::current_exe() {
            let alt = exe
                .parent()?
                .join("_internal")
                .join("resources")
                .join(binary_name);
            if alt.exists() {
                return Some(alt);
            }
        }
        None
    }

    pub async fn check_ffmpeg(&self, path: &Path) -> (bool, Option<String>) {
        let mut command = Command::new(path);
        command
            .arg("-version")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let output = match command.spawn() {
            Ok(child) => tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
                .await
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "ffmpeg -version 超时")
                })
                .and_then(|result| result),
            Err(e) => Err(e),
        };
        match output {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let version = text
                    .lines()
                    .next()
                    .and_then(|line| {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        parts
                            .iter()
                            .position(|&p| p == "version")
                            .and_then(|i| parts.get(i + 1))
                            .copied()
                    })
                    .map(|s| s.to_string());
                (true, version)
            }
            _ => (false, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::VideoProcessor;

    #[test]
    fn ffmpeg_env_value_accepts_file_or_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let binary = dir.path().join("ffmpeg");
        std::fs::write(&binary, b"stub").expect("binary");
        assert_eq!(
            VideoProcessor::ffmpeg_path_from_value(binary.to_str().unwrap(), "ffmpeg"),
            Some(binary.clone())
        );
        assert_eq!(
            VideoProcessor::ffmpeg_path_from_value(dir.path().to_str().unwrap(), "ffmpeg"),
            Some(binary)
        );
        assert!(VideoProcessor::ffmpeg_path_from_value("missing", "ffmpeg").is_none());
    }

    #[tokio::test]
    async fn auto_prefers_custom_path_over_embedded() {
        let dir = tempfile::tempdir().expect("temp dir");
        let binary_name = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        std::fs::create_dir_all(dir.path().join("resources")).expect("resources dir");
        std::fs::write(dir.path().join("resources").join(binary_name), b"embedded")
            .expect("embedded ffmpeg");
        let custom = dir.path().join("full").join(binary_name);
        std::fs::create_dir_all(custom.parent().unwrap()).expect("custom dir");
        std::fs::write(&custom, b"custom").expect("custom ffmpeg");
        let paths = crate::config::AppPaths {
            app_root: dir.path().to_path_buf(),
            data_dir: dir.path().to_path_buf(),
            database_dir: dir.path().to_path_buf(),
            download_dir: dir.path().to_path_buf(),
        };
        let processor = VideoProcessor::new(std::sync::Arc::new(paths));
        let (path, source) = processor.detect_ffmpeg("auto", custom.to_str()).await;
        assert_eq!(source, "custom");
        assert_eq!(path.as_deref(), Some(custom.as_path()));
    }
}
