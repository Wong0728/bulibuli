//! 合并后清理与纯视频 remux：merge_and_cleanup、源文件删除、m4s → mp4。

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tracing::{error, info, warn};

use super::{MergeCallback, MergeResult, VideoProcessor};

impl VideoProcessor {
    pub async fn merge_and_cleanup(
        &self,
        video_path: &Path,
        audio_path: &Path,
        output_path: &Path,
        container: &str,
        callback: Option<MergeCallback>,
    ) -> Result<MergeResult> {
        let v_path = video_path.to_path_buf();
        let a_path = audio_path.to_path_buf();
        let cb: Option<MergeCallback> = callback.map(|user_cb| {
            Box::new(move |result: MergeResult| {
                if result.success {
                    tokio::spawn(async move {
                        Self::cleanup_source_files(&v_path, &a_path).await;
                    });
                }
                user_cb(result);
            }) as MergeCallback
        });

        self.merge_audio_video(video_path, audio_path, output_path, container, cb, None)
            .await
    }

    async fn cleanup_source_files(video_path: &Path, audio_path: &Path) {
        for (path, file_type) in [(video_path, "视频"), (audio_path, "音频")] {
            if !path.exists() {
                continue;
            }
            for attempt in 0..5 {
                match tokio::fs::remove_file(path).await {
                    Ok(_) => {
                        info!("已删除{file_type}源文件: {}", path.display());
                        break;
                    }
                    Err(e) => {
                        if attempt < 4 {
                            warn!("删除{file_type}源文件失败（尝试 {}/5）: {e}", attempt + 1);
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        } else {
                            error!("删除{file_type}源文件最终失败: {e}");
                        }
                    }
                }
            }
        }
    }

    pub(super) fn extract_time(line: &str) -> Option<f64> {
        // 匹配 ffmpeg -stats 输出中的 time=00:05:23.45
        if let Some(idx) = line.find("time=") {
            let rest = &line[idx + 5..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit() && c != ':' && c != '.')
                .unwrap_or(rest.len());
            let time_str = &rest[..end];
            return Self::parse_duration(time_str).ok();
        }
        None
    }

    /// 单输入纯视频 remux（m4s → mp4，`-an` 丢弃音频轨），同步等待完成。
    /// 输出先写临时文件，成功后原子替换；失败/超时清理残留并终止子进程。
    pub async fn remux_video_only(&self, video_path: &Path, output_path: &Path) -> Result<()> {
        let (ffmpeg, _) = self
            .detect_ffmpeg("auto", self.custom_ffmpeg_path.as_deref())
            .await;
        let ffmpeg = ffmpeg.context("未找到 ffmpeg")?;
        if !video_path.exists() {
            return Err(anyhow!("视频文件不存在: {}", video_path.display()));
        }
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }

        let tmp_output = Self::temp_output_path(output_path);
        let mut cmd = Command::new(&ffmpeg);
        cmd.arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(video_path)
            .arg("-c")
            .arg("copy")
            .arg("-an")
            .arg("-f")
            .arg("mp4")
            .arg(&tmp_output)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

        let mut child = cmd.spawn().context("启动 ffmpeg 失败")?;
        // 并发读取 stderr，避免 ffmpeg 写满管道缓冲后阻塞
        let stderr = child.stderr.take();
        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = String::new();
            if let Some(mut pipe) = stderr {
                pipe.read_to_string(&mut buf).await.ok();
            }
            buf
        });

        let status = match tokio::time::timeout(Duration::from_secs(30 * 60), child.wait()).await {
            Ok(result) => result.context("等待 ffmpeg remux 进程失败")?,
            Err(_) => {
                if let Err(e) = child.kill().await {
                    warn!("终止超时 remux 进程失败: {e}");
                }
                child.wait().await.ok();
                tokio::fs::remove_file(&tmp_output).await.ok();
                return Err(anyhow!("ffmpeg remux 超时（30 分钟），已终止"));
            }
        };
        let stderr_text = stderr_task.await.unwrap_or_default();
        if !status.success() {
            tokio::fs::remove_file(&tmp_output).await.ok();
            return Err(anyhow!(
                "ffmpeg remux 失败: {}",
                Self::tail_on_char_boundary(&stderr_text, 500)
            ));
        }
        crate::services::file_safety::atomic_replace(&tmp_output, output_path)
            .await
            .map_err(|e| anyhow!("remux 输出原子替换失败: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::VideoProcessor;

    #[test]
    fn remux_progress_parser_handles_ffmpeg_lines_and_noise() {
        assert_eq!(
            VideoProcessor::extract_time("frame=12 time=00:01:02.25 bitrate=1k"),
            Some(62.25)
        );
        assert_eq!(VideoProcessor::extract_time("ffmpeg failed"), None);
        assert_eq!(VideoProcessor::extract_time("time=00:60:00"), None);
    }
}
