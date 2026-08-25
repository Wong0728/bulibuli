//! 音视频合并：启动 ffmpeg 合并任务、监控进度、原子替换输出。

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use super::{
    MergeCallback, MergeProgress, MergeResult, MergeTaskInfo, ProgressCallback, VideoProcessor,
};

impl VideoProcessor {
    pub async fn merge_audio_video(
        &self,
        video_path: &Path,
        audio_path: &Path,
        output_path: &Path,
        container: &str,
        on_complete: Option<MergeCallback>,
        on_progress: Option<ProgressCallback>,
    ) -> Result<MergeResult> {
        let (ffmpeg, _) = self
            .detect_ffmpeg("auto", self.custom_ffmpeg_path.as_deref())
            .await;
        let ffmpeg = ffmpeg.context("未找到 ffmpeg")?;

        if !video_path.exists() {
            return Ok(MergeResult {
                success: false,
                task_id: String::new(),
                output_path: None,
                message: format!("视频文件不存在: {}", video_path.display()),
            });
        }
        if !audio_path.exists() {
            return Ok(MergeResult {
                success: false,
                task_id: String::new(),
                output_path: None,
                message: format!("音频文件不存在: {}", audio_path.display()),
            });
        }

        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }

        // 合并并发闸门：等待槽位期间任务尚未登记，调用方只会感知为启动变慢。
        let permit = tokio::time::timeout(
            Duration::from_secs(5 * 60),
            Arc::clone(&self.merge_gate).acquire_owned(),
        )
        .await
        .map_err(|_| anyhow!("等待合并槽位超时（已有多个合并任务在执行）"))?
        .map_err(|_| anyhow!("合并服务已关闭"))?;

        let task_id = format!(
            "merge_{}",
            uuid::Uuid::new_v4()
                .to_string()
                .replace('-', "")
                .chars()
                .take(12)
                .collect::<String>()
        );
        let duration = Self::probe_duration(&ffmpeg, video_path)
            .await
            .unwrap_or(0.0);

        // 先写同目录临时文件，成功后原子替换，失败/超时不残留半成品
        let tmp_output = Self::temp_output_path(output_path);
        let mut cmd = Command::new(&ffmpeg);
        cmd.arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("info")
            .arg("-stats")
            .arg("-i")
            .arg(video_path)
            .arg("-i")
            .arg(audio_path)
            .arg("-c")
            .arg("copy")
            .arg("-f")
            .arg(container) // 容器格式：m4a 音频用 mp4，flac/ec3 用 mkv
            .arg(&tmp_output)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

        let child = cmd.spawn().context("启动 ffmpeg 失败")?;

        let task = MergeTaskInfo {
            task_id: task_id.clone(),
            video_path: video_path.to_path_buf(),
            audio_path: audio_path.to_path_buf(),
            output_path: output_path.to_path_buf(),
            status: "running".to_string(),
            progress_percent: 0,
            current_time: 0.0,
            duration,
            start_time: Some(chrono::Local::now()),
            message: "合并任务已启动".to_string(),
        };
        tracing::debug!(
            task_id = %task.task_id,
            video = %task.video_path.display(),
            audio = %task.audio_path.display(),
            output = %task.output_path.display(),
            duration = task.duration,
            start_time = ?task.start_time,
            "创建 FFmpeg 合并任务"
        );
        self.tasks.lock().await.insert(task_id.clone(), task);

        info!(
            "开始合并音视频 [{task_id}]: {} + {} -> {}",
            video_path.display(),
            audio_path.display(),
            output_path.display()
        );

        let tasks = self.tasks.clone();
        let v_path = video_path.to_path_buf();
        let a_path = audio_path.to_path_buf();
        let out_path = output_path.to_path_buf();
        let tid = task_id.clone();
        let background_tasks = self.background_tasks.clone();

        let accepted = background_tasks.spawn("video_merge", async move {
            // 持有合并闸门许可直至 ffmpeg 进程结束（含 panic 路径，随任务 drop 释放）
            let _merge_gate_permit = permit;
            // 包裹 monitor_merge_task：若子任务 panic，确保释放 tasks 映射并记录 error
            use futures::FutureExt;
            use std::panic::AssertUnwindSafe;
            let fut = Self::monitor_merge_task(
                &ffmpeg,
                child,
                &tid,
                duration,
                &tasks,
                &v_path,
                &a_path,
                &out_path,
                on_complete,
                on_progress,
            );
            if let Err(panic_payload) = AssertUnwindSafe(fut).catch_unwind().await {
                error!("合并任务 [{tid}] panic: {panic_payload:?}");
                tasks.lock().await.remove(&tid);
                // panic 路径同样释放调用方的幂等键（on_complete 已随 fut 被 drop，
                // 无法再调用——只能记录并依赖调用方的兜底超时；至少移除 tasks 条目
                // 避免状态卡死）。
                let payload = panic_payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".to_string());
                error!("合并任务 [{tid}] 因 panic 终止，幂等键由调用方兜底: {payload}");
            }
        });
        if !accepted {
            self.tasks.lock().await.remove(&task_id);
            return Err(anyhow!("应用正在关闭，音视频合并任务未启动"));
        }

        Ok(MergeResult {
            success: true,
            task_id: task_id.clone(),
            output_path: Some(output_path.to_path_buf()),
            message: "合并任务已启动".to_string(),
        })
    }

    /// 时长探测：优先使用 ffmpeg 同目录的 ffprobe（Windows portable 已捆绑，
    /// stdout 输出精度更高）；缺失或失败时回退解析 `ffmpeg -i` 的 stderr。
    async fn probe_duration(ffmpeg: &Path, video_path: &Path) -> Option<f64> {
        let ffprobe = ffmpeg.with_file_name(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        if ffprobe.is_file() {
            if let Some(duration) = Self::probe_duration_ffprobe(&ffprobe, video_path).await {
                return Some(duration);
            }
        }
        Self::probe_duration_ffmpeg(ffmpeg, video_path).await
    }

    async fn probe_duration_ffprobe(ffprobe: &Path, video_path: &Path) -> Option<f64> {
        let mut command = Command::new(ffprobe);
        command
            .arg("-v")
            .arg("error")
            .arg("-show_entries")
            .arg("format=duration")
            .arg("-of")
            .arg("default=noprint_wrappers=1:nokey=1")
            .arg(video_path)
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().ok()?;
        let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .ok()?
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .ok()
    }

    async fn probe_duration_ffmpeg(ffmpeg: &Path, video_path: &Path) -> Option<f64> {
        let mut command = Command::new(ffmpeg);
        command
            .arg("-i")
            .arg(video_path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().ok()?;
        let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .ok()?
            .ok()?;
        let text = String::from_utf8_lossy(&output.stderr);
        for line in text.lines() {
            if line.contains("Duration:") {
                if let Some(start) = line.find("Duration: ") {
                    let rest = &line[start + 10..];
                    if let Some(end) = rest.find(',') {
                        let duration_str = &rest[..end];
                        if let Ok(d) = Self::parse_duration(duration_str) {
                            return Some(d);
                        }
                    }
                }
            }
        }
        None
    }

    pub(super) fn parse_duration(s: &str) -> Result<f64> {
        let parts: Vec<&str> = s.trim().split(':').collect();
        if parts.len() != 3 {
            return Err(anyhow!("无效时长格式: {s}"));
        }
        let hours: f64 = parts[0].parse()?;
        let minutes: f64 = parts[1].parse()?;
        let seconds: f64 = parts[2].parse()?;
        if hours < 0.0 || !(0.0..60.0).contains(&minutes) || !(0.0..60.0).contains(&seconds) {
            return Err(anyhow!("无效时长范围: {s}"));
        }
        Ok(hours * 3600.0 + minutes * 60.0 + seconds)
    }

    #[allow(clippy::too_many_arguments)]
    async fn monitor_merge_task(
        _ffmpeg: &Path,
        mut child: Child,
        task_id: &str,
        duration: f64,
        tasks: &Arc<Mutex<HashMap<String, MergeTaskInfo>>>,
        _video_path: &Path,
        _audio_path: &Path,
        output_path: &Path,
        on_complete: Option<MergeCallback>,
        on_progress: Option<ProgressCallback>,
    ) {
        let Some(stderr) = child.stderr.take() else {
            error!("合并任务 [{task_id}] 无法读取 ffmpeg stderr");
            // 早退路径同样必须释放调用方的幂等键（on_complete），
            // 否则该 bvid 的后续合并会被永久跳过。
            if let Some(cb) = on_complete {
                cb(MergeResult {
                    success: false,
                    task_id: task_id.to_string(),
                    output_path: None,
                    message: "无法读取 ffmpeg stderr".to_string(),
                });
            }
            return;
        };
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();

        let mut stderr_buf = String::new();
        let mut last_progress = Instant::now();
        let timeout = tokio::time::sleep(Duration::from_secs(6 * 60 * 60));
        tokio::pin!(timeout);
        let mut timed_out = false;

        loop {
            let line = tokio::select! {
                result = lines.next_line() => match result {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        warn!("读取 ffmpeg 输出失败 [{task_id}]: {e}");
                        break;
                    }
                },
                _ = &mut timeout => {
                    timed_out = true;
                    error!("ffmpeg 合并任务超时 [{task_id}]，正在终止子进程");
                    break;
                }
            };
            stderr_buf.push_str(&line);
            stderr_buf.push('\n');

            if let Some(time) = Self::extract_time(&line) {
                let percent = if duration > 0.0 {
                    ((time / duration) * 100.0).min(99.0) as i32
                } else {
                    0
                };

                {
                    let mut lock = tasks.lock().await;
                    if let Some(t) = lock.get_mut(task_id) {
                        t.status = "running".to_string();
                        t.progress_percent = percent;
                        t.current_time = time;
                        t.message = format!("合并中 {}%", percent);
                    }
                }

                if last_progress.elapsed() >= Duration::from_millis(500) {
                    if let Some(cb) = &on_progress {
                        let update = MergeProgress {
                            task_id: task_id.to_string(),
                            status: "running".to_string(),
                            progress_percent: percent,
                            current_time: time,
                            duration,
                            message: format!("合并中 {}%", percent),
                        };
                        tracing::debug!(
                            task_id = %update.task_id,
                            status = %update.status,
                            progress = update.progress_percent,
                            current_time = update.current_time,
                            duration = update.duration,
                            message = %update.message,
                            "FFmpeg 合并进度"
                        );
                        cb(update);
                    }
                    last_progress = Instant::now();
                }
            }
        }

        if timed_out {
            if let Err(e) = child.kill().await {
                warn!("终止超时 ffmpeg 任务失败 [{task_id}]: {e}");
            }
        }
        let status = child.wait().await;
        let mut success = matches!(status, Ok(s) if s.success());

        // ffmpeg 实际写入的是临时文件：成功后原子替换到最终路径，失败/超时清理残留
        let tmp_output = Self::temp_output_path(output_path);
        let mut replace_error: Option<String> = None;
        if success {
            if let Err(e) =
                crate::services::file_safety::atomic_replace(&tmp_output, output_path).await
            {
                success = false;
                replace_error = Some(format!("合并输出原子替换失败: {e}"));
            }
        }
        if !success {
            if let Ok(true) = tokio::fs::try_exists(&tmp_output).await {
                if let Err(e) = tokio::fs::remove_file(&tmp_output).await {
                    warn!("清理合并临时文件失败 [{task_id}]: {e}");
                }
            }
        }

        let result = if success {
            info!("合并完成 [{task_id}]: {}", output_path.display());
            {
                let mut lock = tasks.lock().await;
                if let Some(t) = lock.get_mut(task_id) {
                    t.status = "completed".to_string();
                    t.progress_percent = 100;
                    t.message = "合并完成".to_string();
                }
            }
            MergeResult {
                success: true,
                task_id: task_id.to_string(),
                output_path: Some(output_path.to_path_buf()),
                message: "合并完成".to_string(),
            }
        } else {
            let err = if timed_out {
                "ffmpeg 处理超过 6 小时，已终止".to_string()
            } else if let Some(replace_error) = replace_error {
                replace_error
            } else {
                stderr_buf.split_whitespace().collect::<Vec<_>>().join(" ")
            };
            let err = Self::tail_on_char_boundary(&err, 500).to_string();
            warn!("ffmpeg 合并失败 [{task_id}]: {err}");
            {
                let mut lock = tasks.lock().await;
                if let Some(t) = lock.get_mut(task_id) {
                    t.status = "failed".to_string();
                    t.message = format!("合并失败: {err}");
                }
            }
            MergeResult {
                success: false,
                task_id: task_id.to_string(),
                output_path: None,
                message: format!("合并失败: {err}"),
            }
        };

        if let Some(cb) = on_complete {
            tracing::debug!(task_id = %result.task_id, "FFmpeg 合并任务完成");
            cb(result);
        }

        // 任务终结后从内存映射中移除，避免长期运行导致内存泄漏
        // 仍保留 5 秒便于前端轮询获取最终状态
        let tasks_clone = tasks.clone();
        let tid = task_id.to_string();
        tokio::time::sleep(Duration::from_secs(5)).await;
        tasks_clone.lock().await.remove(&tid);
    }

    /// 合并输出的同目录临时文件路径（atomic_replace 要求与目标同目录）
    pub(super) fn temp_output_path(output: &Path) -> PathBuf {
        let mut name = output
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| std::ffi::OsString::from("output.mp4"));
        name.push(".tmp");
        output.with_file_name(name)
    }

    /// 取字符串末尾至多 max_bytes 字节，起点对齐 UTF-8 字符边界，
    /// 避免在多字节字符（如中文文件名）中间切片导致 panic
    pub(super) fn tail_on_char_boundary(s: &str, max_bytes: usize) -> &str {
        let mut start = s.len().saturating_sub(max_bytes);
        while !s.is_char_boundary(start) {
            start += 1;
        }
        &s[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::VideoProcessor;

    #[test]
    fn merge_helpers_reject_invalid_clock_ranges() {
        assert_eq!(
            VideoProcessor::parse_duration("00:59:59.9").unwrap(),
            3599.9
        );
        assert!(VideoProcessor::parse_duration("00:60:00").is_err());
        assert!(VideoProcessor::parse_duration("00:00:60").is_err());
        assert!(VideoProcessor::parse_duration("-1:00:00").is_err());
    }
}
