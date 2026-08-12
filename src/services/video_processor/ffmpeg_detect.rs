//! ffmpeg 可执行文件探测与可用性校验（env / system / embedded / custom / auto）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use super::VideoProcessor;

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
                // auto: embedded > custom > env > system；完整包优先使用包内版本。
                if let Some(p) = self.embedded_ffmpeg() {
                    return (Some(p), "embedded".to_string());
                }
                if let Some(p) = custom_path {
                    let pb = PathBuf::from(p);
                    if pb.is_file() {
                        return (Some(pb), "custom".to_string());
                    }
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

    fn ffmpeg_from_env() -> Option<PathBuf> {
        for name in [
            "FFMPEG_PATH",
            "FFMPEG",
            "FF_PATH",
            "FFMPEG_HOME",
            "FFMPEG_DIR",
        ] {
            if let Ok(val) = std::env::var(name) {
                let p = PathBuf::from(&val);
                if p.is_file() {
                    return Some(p);
                }
                let exe_name = if cfg!(target_os = "windows") {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                };
                let candidate = p.join(exe_name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
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
