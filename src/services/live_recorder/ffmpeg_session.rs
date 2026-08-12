//! FFmpeg 直播录制会话：进程启动、监督、停止和分段合并。

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const MAX_DIAGNOSTIC_BYTES: usize = 1024 * 1024;
const MIN_MERGED_DURATION_RATIO: f64 = 0.90;
const MERGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 合并预检的额外安全余量：除输出体积（约等于输入分段总和）外保留的空间。
const MERGE_FREE_SPACE_MARGIN_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug)]
struct MediaProbe {
    duration_secs: f64,
    has_video: bool,
    has_audio: bool,
}

/// 单个 FFmpeg 录制进程。
pub struct FfmpegSession {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    output_path: PathBuf,
    room_id: i64,
    diagnostic_task: Option<JoinHandle<Result<Vec<u8>>>>,
}

impl FfmpegSession {
    /// 启动 FFmpeg 录制进程。
    pub fn start(
        ffmpeg_path: &Path,
        stream_url: &str,
        output_path: PathBuf,
        room_id: i64,
        user_agent: &str,
        referer: &str,
    ) -> Result<Self> {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建录制输出目录失败: {}", parent.display()))?;
        }

        let mut cmd = Command::new(ffmpeg_path);
        cmd.args([
            "-y",
            "-user_agent",
            user_agent,
            "-referer",
            referer,
            "-rw_timeout",
            "30000000",
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_on_network_error",
            "1",
            "-reconnect_delay_max",
            "10",
            "-i",
            stream_url,
            "-c",
            "copy",
            "-flvflags",
            "add_keyframe_index",
        ])
        .arg(&output_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        // FFmpeg 长时间录制会持续写 stderr；必须有后台 reader，且 reader 不能在达到
        // 缓存上限后停止，否则管道仍会重新填满并阻塞 FFmpeg。
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("启动 FFmpeg 录制进程失败 (room_id={room_id})"))?;
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let diagnostic_task = tokio::spawn(read_diagnostic(stderr));

        info!(
            room_id,
            output = %output_path.display(),
            "FFmpeg 录制会话已启动"
        );

        Ok(Self {
            child: Some(child),
            stdin,
            output_path,
            room_id,
            diagnostic_task: Some(diagnostic_task),
        })
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    /// 非阻塞地检查进程状态。
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        child.try_wait().context("检查 FFmpeg 进程状态失败")
    }

    /// 读取当前已收集的诊断输出。
    pub async fn diagnostics(&mut self) -> String {
        let Some(task) = self.diagnostic_task.take() else {
            return String::new();
        };
        match task.await {
            Ok(Ok(bytes)) => redact_diagnostics(&String::from_utf8_lossy(&bytes)),
            Ok(Err(error)) => format!("读取 FFmpeg 诊断输出失败: {error}"),
            Err(error) => format!("FFmpeg 诊断任务异常退出: {error}"),
        }
    }

    /// 停止录制，最多等待 `wait_timeout`；超时后强制终止并回收子进程。
    pub async fn stop_with_timeout(&mut self, wait_timeout: Duration) -> Result<()> {
        let Some(child) = self.child.as_mut() else {
            let _ = self.diagnostics().await;
            return Ok(());
        };

        // `q` 是 FFmpeg 的可移植优雅退出命令。写入后关闭 stdin，便于复用器在
        // Windows 和 Unix 上完成文件尾写入。
        if let Some(mut stdin) = self.stdin.take() {
            if let Err(error) = stdin.write_all(b"q\n").await {
                debug!(room_id = self.room_id, "发送 FFmpeg 退出命令失败: {error}");
            } else if let Err(error) = stdin.flush().await {
                debug!(room_id = self.room_id, "刷新 FFmpeg stdin 失败: {error}");
            }
        }

        let wait_result = timeout(wait_timeout, child.wait()).await;
        let mut stop_error = None;
        match wait_result {
            Err(_) => {
                warn!(room_id = self.room_id, "FFmpeg 停止超时，强制终止进程");
                stop_error = Some(anyhow!("FFmpeg 停止超时，已强制终止"));
                if let Err(error) = child.kill().await {
                    stop_error = Some(anyhow!("强制终止 FFmpeg 失败: {error}"));
                } else if let Err(error) = child.wait().await {
                    stop_error = Some(anyhow!("回收被强制终止的 FFmpeg 失败: {error}"));
                }
            }
            Ok(Err(error)) => {
                stop_error = Some(anyhow!("等待 FFmpeg 停止失败: {error}"));
            }
            Ok(Ok(_)) => {}
        }

        self.child = None;
        self.stdin = None;
        let diagnostics = self.diagnostics().await;
        if let Some(error) = stop_error {
            if !diagnostics.is_empty() {
                return Err(error.context(diagnostics));
            }
            return Err(error);
        }

        info!(room_id = self.room_id, "FFmpeg 录制会话已停止");
        Ok(())
    }
}

impl Drop for FfmpegSession {
    fn drop(&mut self) {
        if self.child.is_some() {
            debug!(
                room_id = self.room_id,
                "FFmpeg 会话 drop，子进程将由 kill_on_drop 清理"
            );
        }
        if let Some(task) = self.diagnostic_task.take() {
            task.abort();
        }
    }
}

/// 按顺序合并 FLV 分段并封装为 MP4。
///
/// 本函数不会删除任何输入分段；调用方只有在确认输出文件有效后才能清理输入。
pub async fn merge_segments_to_mp4(ffmpeg_path: &Path, segments: &[PathBuf]) -> Result<PathBuf> {
    merge_segments_to_mp4_inner(ffmpeg_path, segments, None).await
}

/// 合并分段，同时允许后台任务终止 FFmpeg 子进程。
/// 取消合并时保留源分段文件。
pub async fn merge_segments_to_mp4_cancelable(
    ffmpeg_path: &Path,
    segments: &[PathBuf],
    cancellation: &CancellationToken,
) -> Result<PathBuf> {
    merge_segments_to_mp4_inner(ffmpeg_path, segments, Some(cancellation)).await
}

async fn merge_segments_to_mp4_inner(
    ffmpeg_path: &Path,
    segments: &[PathBuf],
    cancellation: Option<&CancellationToken>,
) -> Result<PathBuf> {
    if segments.is_empty() {
        return Err(anyhow!("没有可合并的直播分段"));
    }

    let first = segments
        .first()
        .expect("segments checked above")
        .to_path_buf();
    // 合并输出会占用与输入分段总量相当的磁盘空间；启动前预检，避免长时间
    // 写入后才因磁盘写满产出损坏的 MP4。源分段在合并成功前不会被删除。
    let total_segment_bytes: u64 = segments
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum();
    if total_segment_bytes > 0 {
        let target_dir = first
            .parent()
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(|| Path::new("."));
        if let Ok(available) = fs2::available_space(target_dir) {
            let required = total_segment_bytes + MERGE_FREE_SPACE_MARGIN_BYTES;
            if available < required {
                return Err(anyhow!(
                    "磁盘空间不足：合并约需额外 {} MB，当前可用 {} MB，已取消本次合并（源分段仍保留）",
                    required / (1024 * 1024),
                    available / (1024 * 1024)
                ));
            }
        }
    }
    let output = first.with_extension("mp4");
    let partial_output = first.with_extension("mp4.partial");
    let list_path = first.with_file_name(format!(
        ".{}.concat-{}.txt",
        first.file_stem().and_then(|v| v.to_str()).unwrap_or("live"),
        uuid::Uuid::new_v4().simple()
    ));
    let list_body = segments
        .iter()
        .map(|path| format!("file '{}'", escape_concat_path(path)))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(&list_path, format!("{list_body}\n"))
        .await
        .with_context(|| format!("创建 FFmpeg 分段清单失败: {}", list_path.display()))?;

    let result =
        run_merge_command_timed(ffmpeg_path, &list_path, &partial_output, cancellation).await;
    if let Err(error) = tokio::fs::remove_file(&list_path).await {
        debug!(path = %list_path.display(), "删除临时分段清单失败: {error}");
    }

    let output_size = tokio::fs::metadata(&partial_output)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if output_size == 0 {
        let _ = tokio::fs::remove_file(&partial_output).await;
    }
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        let _ = tokio::fs::remove_file(&partial_output).await;
        return Err(anyhow!("FFmpeg merge cancelled"));
    }
    match result {
        Ok(()) if output_size > 0 => {
            if let Err(error) = verify_merged_output(ffmpeg_path, &partial_output, segments).await {
                let _ = tokio::fs::remove_file(&partial_output).await;
                return Err(error);
            }
            tokio::fs::rename(&partial_output, &output)
                .await
                .with_context(|| {
                    format!("rename verified merge output failed: {}", output.display())
                })?;
            info!(output = %output.display(), segments = segments.len(), "直播分段合并完成");
            Ok(output)
        }
        Ok(()) => Err(anyhow!("FFmpeg 合并完成但输出 MP4 为空")),
        Err(error) => {
            let _ = tokio::fs::remove_file(&partial_output).await;
            Err(error)
        }
    }
}

/// 非空 MP4 不能单独证明拼接成功；必须先探测容器，调用方才能删除原始 FLV 分段。
async fn verify_merged_output(
    ffmpeg_path: &Path,
    output: &Path,
    segments: &[PathBuf],
) -> Result<()> {
    let output_probe = probe_media(ffmpeg_path, output).await?;
    if !output_probe.has_video || !output_probe.has_audio {
        return Err(anyhow!(
            "合并输出缺少{}轨道",
            if !output_probe.has_video {
                "视频"
            } else {
                "音频"
            }
        ));
    }
    let mut input_duration = 0.0;
    for segment in segments {
        let probe = probe_media(ffmpeg_path, segment)
            .await
            .with_context(|| format!("校验合并输入分段失败: {}", segment.display()))?;
        if probe.duration_secs <= 0.0 {
            return Err(anyhow!("输入分段时长无效: {}", segment.display()));
        }
        input_duration += probe.duration_secs;
    }
    if output_probe.duration_secs < input_duration * MIN_MERGED_DURATION_RATIO {
        return Err(anyhow!(
            "合并输出时长不足: 输出 {:.3}s，输入合计 {:.3}s",
            output_probe.duration_secs,
            input_duration
        ));
    }
    Ok(())
}

async fn probe_media(ffmpeg_path: &Path, input: &Path) -> Result<MediaProbe> {
    let probe = ffprobe_path(ffmpeg_path);
    let result = Command::new(&probe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type",
            "-of",
            "json",
        ])
        .arg(input)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await;
    match result {
        Ok(result) if result.status.success() => parse_probe_json(&result.stdout),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            probe_media_with_ffmpeg(ffmpeg_path, input).await
        }
        Ok(result) => Err(anyhow!(
            "ffprobe 校验失败: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )),
        Err(error) => {
            Err(error).with_context(|| format!("启动 ffprobe 校验失败: {}", probe.display()))
        }
    }
}

fn parse_probe_json(stdout: &[u8]) -> Result<MediaProbe> {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).context("解析 ffprobe 校验结果失败")?;
    let streams = value
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("ffprobe 未返回 streams"))?;
    let has_video = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
    });
    let has_audio = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("audio")
    });
    let duration_secs = value
        .pointer("/format/duration")
        .and_then(serde_json::Value::as_str)
        .and_then(|duration| duration.parse::<f64>().ok())
        .filter(|duration| duration.is_finite() && *duration >= 0.0)
        .ok_or_else(|| anyhow!("ffprobe 未返回有效时长"))?;
    Ok(MediaProbe {
        duration_secs,
        has_video,
        has_audio,
    })
}

async fn probe_media_with_ffmpeg(ffmpeg_path: &Path, input: &Path) -> Result<MediaProbe> {
    let result = Command::new(ffmpeg_path)
        .args(["-hide_banner", "-i"])
        .arg(input)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("使用 FFmpeg 读取媒体信息失败: {}", ffmpeg_path.display()))?;
    let text = String::from_utf8_lossy(&result.stderr);
    let duration_secs = text
        .lines()
        .find_map(|line| line.split("Duration: ").nth(1))
        .and_then(|value| value.split(',').next())
        .and_then(parse_timestamp)
        .ok_or_else(|| anyhow!("FFmpeg 未返回有效媒体时长"))?;
    let has_video = text.lines().any(|line| line.contains(" Video:"));
    let has_audio = text.lines().any(|line| line.contains(" Audio:"));
    if !result.status.success() && !has_video && !has_audio {
        return Err(anyhow!("FFmpeg 媒体探测失败: {}", text.trim()));
    }
    Ok(MediaProbe {
        duration_secs,
        has_video,
        has_audio,
    })
}

fn parse_timestamp(value: &str) -> Option<f64> {
    let mut parts = value.trim().split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn ffprobe_path(ffmpeg_path: &Path) -> PathBuf {
    let probe_name = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    if let Some(candidate) = ffmpeg_path
        .parent()
        .map(|parent| parent.join(probe_name))
        .filter(|path| path.is_file())
    {
        return candidate;
    }
    for variable in [
        "FFPROBE_PATH",
        "FFPROBE",
        "FFMPEG_PATH",
        "FFMPEG_HOME",
        "FFMPEG_DIR",
    ] {
        if let Ok(value) = std::env::var(variable) {
            let configured = PathBuf::from(value);
            let candidate = if configured.is_file() {
                configured
            } else {
                configured.join(probe_name)
            };
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    which::which(probe_name).unwrap_or_else(|_| PathBuf::from(probe_name))
}

async fn run_merge_command_timed(
    ffmpeg_path: &Path,
    list_path: &Path,
    output: &Path,
    cancellation: Option<&CancellationToken>,
) -> Result<()> {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args([
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        &list_path.to_string_lossy(),
        "-c",
        "copy",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
    ])
    .arg(output)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);

    let mut child = cmd.spawn().context("failed to start FFmpeg merge")?;
    let stdout_task = tokio::spawn(read_diagnostic(child.stdout.take()));
    let stderr_task = tokio::spawn(read_diagnostic(child.stderr.take()));
    let status = if let Some(cancellation) = cancellation {
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(anyhow!("FFmpeg merge cancelled"));
            }
            result = timeout(MERGE_TIMEOUT, child.wait()) => {
                match result {
                    Ok(result) => result.context("failed to wait for FFmpeg merge")?,
                    Err(_) => {
                        warn!(timeout_secs = MERGE_TIMEOUT.as_secs(), "FFmpeg merge timed out");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        let _ = stdout_task.await;
                        let _ = stderr_task.await;
                        return Err(anyhow!("FFmpeg merge timed out"));
                    }
                }
            }
        }
    } else {
        match timeout(MERGE_TIMEOUT, child.wait()).await {
            Ok(result) => result.context("failed to wait for FFmpeg merge")?,
            Err(_) => {
                warn!(
                    timeout_secs = MERGE_TIMEOUT.as_secs(),
                    "FFmpeg merge timed out"
                );
                let _ = child.kill().await;
                let _ = child.wait().await;
                let _ = stdout_task.await;
                let _ = stderr_task.await;
                return Err(anyhow!("FFmpeg merge timed out"));
            }
        }
    };
    let _stdout = stdout_task
        .await
        .context("failed to read FFmpeg merge stdout")??;
    let stderr = stderr_task
        .await
        .context("failed to read FFmpeg merge stderr")??;
    if !status.success() {
        let diagnostics = redact_diagnostics(&String::from_utf8_lossy(&stderr));
        error!(stderr = %diagnostics, "FFmpeg merge failed");
        return Err(anyhow!(
            "FFmpeg merge failed: {}",
            diagnostics
                .lines()
                .last()
                .unwrap_or("FFmpeg returned failure")
        ));
    }
    Ok(())
}

#[allow(dead_code)]
async fn run_merge_command(ffmpeg_path: &Path, list_path: &Path, output: &Path) -> Result<()> {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args([
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        &list_path.to_string_lossy(),
        "-c",
        "copy",
        "-movflags",
        "+faststart",
    ])
    .arg(output)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);

    let mut child = cmd.spawn().context("启动 FFmpeg 分段合并进程失败")?;
    let stdout_task = tokio::spawn(read_diagnostic(child.stdout.take()));
    let stderr_task = tokio::spawn(read_diagnostic(child.stderr.take()));
    let status = child.wait().await.context("等待 FFmpeg 分段合并进程失败")?;
    let _stdout = stdout_task
        .await
        .context("读取 FFmpeg 合并 stdout 任务失败")??;
    let stderr = stderr_task
        .await
        .context("读取 FFmpeg 合并 stderr 任务失败")??;

    if !status.success() {
        let diagnostics = String::from_utf8_lossy(&stderr).trim().to_string();
        error!(stderr = %diagnostics, "FFmpeg 分段合并失败");
        return Err(anyhow!(
            "直播分段合并失败: {}",
            diagnostics.lines().last().unwrap_or("FFmpeg 返回失败状态")
        ));
    }
    Ok(())
}

pub(crate) fn redact_diagnostics(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find("http://").or_else(|| rest.find("https://")) {
        output.push_str(&rest[..index]);
        let url = &rest[index..];
        let end = url
            .find(|character: char| {
                character.is_whitespace() || character == ')' || character == ']'
            })
            .unwrap_or(url.len());
        let token = &url[..end];
        if let Some(query) = token.find('?') {
            output.push_str(&token[..query]);
            output.push_str("?<redacted>");
        } else {
            output.push_str("<redacted-url>");
        }
        rest = &url[end..];
    }
    output.push_str(rest);
    for marker in [
        "access_key=",
        "auth_key=",
        "token=",
        "sign=",
        "w_rid=",
        "wts=",
    ] {
        let mut cursor = 0;
        while let Some(relative) = output[cursor..].find(marker) {
            let start = cursor + relative + marker.len();
            let end = output[start..]
                .find(|character: char| character.is_whitespace() || character == '&')
                .map(|index| start + index)
                .unwrap_or(output.len());
            output.replace_range(start..end, "<redacted>");
            cursor = start + "<redacted>".len();
        }
    }
    if output.split_whitespace().any(|part| {
        part.starts_with('/')
            || part.starts_with("\\\\")
            || (part.len() >= 3
                && part.as_bytes()[1] == b':'
                && (part.as_bytes()[2] == b'\\' || part.as_bytes()[2] == b'/'))
    }) {
        return "diagnostic redacted".to_owned();
    }
    output.trim().to_owned()
}

fn escape_concat_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

/// 持续消费进程输出，同时只保留最后 1 MiB 诊断文本。
async fn read_diagnostic<R>(reader: Option<R>) -> Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(mut reader) = reader else {
        return Ok(Vec::new());
    };
    use tokio::io::AsyncReadExt;

    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .context("读取 FFmpeg 诊断输出失败")?;
        if read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..read]);
        if output.len() > MAX_DIAGNOSTIC_BYTES {
            let excess = output.len() - MAX_DIAGNOSTIC_BYTES;
            output.drain(..excess);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tempfile::tempdir;

    fn spawn_test_child(script: &str) -> Child {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd.exe");
            command.args(["/C", script]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            command
        };

        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn test process")
    }

    fn session_from_child(mut child: Child) -> FfmpegSession {
        let diagnostic_task = tokio::spawn(read_diagnostic(child.stderr.take()));
        let stdin = child.stdin.take();
        FfmpegSession {
            child: Some(child),
            stdin,
            output_path: PathBuf::from("test.flv"),
            room_id: 1,
            diagnostic_task: Some(diagnostic_task),
        }
    }

    #[tokio::test]
    async fn diagnostic_reader_continues_draining_after_limit() {
        #[cfg(windows)]
        let script = "for /L %i in (1,1,60000) do @echo stderr-line 1>&2";
        #[cfg(not(windows))]
        let script = "for i in $(seq 1 60000); do echo stderr-line >&2; done";

        let mut session = session_from_child(spawn_test_child(script));
        let diagnostics = session.diagnostics().await;
        assert!(!diagnostics.is_empty());
        assert!(diagnostics.len() <= MAX_DIAGNOSTIC_BYTES);
    }

    #[tokio::test]
    async fn unexpected_child_exit_is_observable() {
        #[cfg(windows)]
        let script = "exit /B 7";
        #[cfg(not(windows))]
        let script = "exit 7";

        let mut session = session_from_child(spawn_test_child(script));
        let status = loop {
            if let Some(status) = session.try_wait().expect("try wait") {
                break status;
            }
            tokio::task::yield_now().await;
        };
        assert!(!status.success());
        assert!(session.diagnostics().await.is_empty());
    }

    #[tokio::test]
    async fn graceful_stop_writes_q_to_child_stdin() {
        #[cfg(windows)]
        let script = "setlocal EnableDelayedExpansion & set /p line= & if \"!line!\"==\"q\" (exit /B 0) else (exit /B 2)";
        #[cfg(not(windows))]
        let script = "read line; [ \"$line\" = q ]";

        let mut session = session_from_child(spawn_test_child(script));
        session
            .stop_with_timeout(Duration::from_secs(3))
            .await
            .expect("graceful child stop");
    }

    #[tokio::test]
    async fn stop_timeout_force_kills_child() {
        #[cfg(windows)]
        let script = "ping 127.0.0.1 -n 30 > nul";
        #[cfg(not(windows))]
        let script = "trap '' INT; sleep 30";

        let mut session = session_from_child(spawn_test_child(script));
        let result = session.stop_with_timeout(Duration::from_millis(50)).await;
        assert!(result.is_err());
        assert!(session.child.is_none());
    }

    #[test]
    fn concat_path_escapes_single_quotes() {
        let path = PathBuf::from("C:\\recordings\\主播'segment.flv");
        assert_eq!(
            escape_concat_path(&path),
            "C:\\recordings\\主播'\\''segment.flv"
        );
    }

    fn required_ffmpeg_bin() -> PathBuf {
        let bin = std::env::var_os("FFMPEG_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(if cfg!(windows) {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                })
            });
        std::process::Command::new(&bin)
            .arg("-version")
            .output()
            .unwrap_or_else(|error| {
                panic!(
                "真实媒体集成测试要求可用 ffmpeg；请设置 FFMPEG_BIN 或将 ffmpeg 加入 PATH: {error}"
            )
            });
        let probe = ffprobe_path(&bin);
        std::process::Command::new(&probe)
            .arg("-version")
            .output()
            .unwrap_or_else(|error| {
                panic!("真实媒体集成测试要求可用 ffprobe；请将其置于 ffmpeg 同目录或 PATH: {error}")
            });
        bin
    }

    async fn generate_fixture(ffmpeg: &Path, output: &Path) {
        let result = Command::new(ffmpeg)
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=25",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=44100",
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-f",
                "flv",
            ])
            .arg(output)
            .output()
            .await
            .expect("run ffmpeg fixture generation");
        assert!(
            result.status.success(),
            "fixture generation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[tokio::test]
    async fn merge_real_flv_segments_preserves_duration_and_tracks() {
        let ffmpeg = required_ffmpeg_bin();
        let temp = tempdir().expect("temporary directory");
        let media_dir = temp.path().join("直播 ' 媒体夹具");
        tokio::fs::create_dir_all(&media_dir)
            .await
            .expect("media directory");
        let first = media_dir.join("第一段.flv");
        let second = media_dir.join("第二段.flv");
        generate_fixture(&ffmpeg, &first).await;
        generate_fixture(&ffmpeg, &second).await;

        let output = merge_segments_to_mp4(&ffmpeg, &[first.clone(), second.clone()])
            .await
            .expect("merge real FLV fixtures");
        let probe = probe_media(&ffmpeg, &output)
            .await
            .expect("probe merged output");
        assert!(probe.has_video && probe.has_audio);
        assert!(
            probe.duration_secs >= 1.8,
            "merged duration: {}",
            probe.duration_secs
        );
        assert!(
            first.exists() && second.exists(),
            "merger must not remove source segments"
        );
    }

    #[tokio::test]
    async fn invalid_merge_keeps_source_segment() {
        let ffmpeg = required_ffmpeg_bin();
        let temp = tempdir().expect("temporary directory");
        let source = temp.path().join("损坏片段.flv");
        tokio::fs::write(&source, b"not a media stream")
            .await
            .expect("write source");
        assert!(
            merge_segments_to_mp4(&ffmpeg, std::slice::from_ref(&source))
                .await
                .is_err()
        );
        assert!(
            source.exists(),
            "failed validation must retain recovery input"
        );
    }

    #[test]
    fn diagnostics_redact_urls_and_signed_query_values() {
        let value = redact_diagnostics(
            "GET https://cdn.example/live.flv?token=secret&sign=signature w_rid=rid",
        );
        assert!(!value.contains("secret"));
        assert!(!value.contains("signature"));
        assert!(!value.contains("=rid"));
    }
}
