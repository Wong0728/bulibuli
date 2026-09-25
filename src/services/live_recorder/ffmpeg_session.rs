//! FFmpeg 直播录制会话：进程启动、监督、停止和分段合并。

use crate::services::spawn_util::child_lifetime;
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
/// 校验失败时保留的合并产物后缀：不匹配 `*.mp4`，也不被启动清扫删除。
const UNVERIFIED_SUFFIX: &str = ".unverified";
/// 探测挂起（损坏文件/异常 IO）会让 finalize 永久停留在 Finalizing：
/// ffprobe 与回退的 ffmpeg 探测都按此超时收口，量级与合并超时对齐。
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
struct MediaProbe {
    duration_secs: f64,
    has_video: bool,
    has_audio: bool,
}

/// 分段有效性判定：时长为 0 不等于坏文件。
#[derive(Debug, PartialEq)]
enum SegmentProbe {
    /// 时长可信，计入输入总时长。
    Timestamped(f64),
    /// 时长不可信（强杀的 FLV 会把 onMetaData.duration 占位写成 0），
    /// 但音视频轨道与体积有效。
    Untimestamped,
    /// 既无时长也无可用轨道或体积为空：真实坏输入。
    Invalid(&'static str),
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

        let mut child = child_lifetime::spawn(&mut cmd)
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
        // spawn_blocking：fs2 是同步系统调用，Windows 网络盘可能阻塞 runtime 线程
        let target_dir = first
            .parent()
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        if let Ok(Ok(available)) =
            tokio::task::spawn_blocking(move || fs2::available_space(&target_dir)).await
        {
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
                // 校验失败不删产物：整场录像只有一次合并机会，删掉就等于用户什么都没了。
                // 改名为 *.mp4.unverified 保留（既不被启动清扫，也不会被误认成成品）。
                let salvage = unverified_output_path(&partial_output);
                let kept = match tokio::fs::rename(&partial_output, &salvage).await {
                    Ok(()) => salvage,
                    Err(rename_error) => {
                        debug!(
                            error = %rename_error,
                            "重命名待抢救合并产物失败，按原路径保留"
                        );
                        partial_output
                    }
                };
                let name = kept
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("")
                    .to_owned();
                // 单条消息里带上原因与保留路径：调用方只取 Display(to_string)，
                // 用 context 包装会把原因挤到链外、用户只剩一句"已保留"。
                return Err(anyhow!(
                    "{error:#}；已保留未通过校验的合并产物 {name}（与分段同目录，可自行播放抢救）"
                ));
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

/// `xxx.mp4.partial` -> `xxx.mp4.unverified`
fn unverified_output_path(partial_output: &Path) -> PathBuf {
    let name = partial_output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("merge-output.mp4.partial");
    let base = name.strip_suffix(".partial").unwrap_or(name);
    partial_output.with_file_name(format!("{base}{UNVERIFIED_SUFFIX}"))
}

/// 非空 MP4 不能单独证明拼接成功；必须先探测容器，调用方才能删除原始 FLV 分段。
async fn verify_merged_output(
    ffmpeg_path: &Path,
    output: &Path,
    segments: &[PathBuf],
) -> Result<()> {
    let output_probe = probe_media(ffmpeg_path, output).await?;
    if !output_probe.has_video {
        return Err(anyhow!("合并输出缺少视频轨道"));
    }
    let mut input_duration = 0.0;
    let mut input_has_audio = false;
    let mut untimestamped = Vec::new();
    for segment in segments {
        let probe = probe_media(ffmpeg_path, segment)
            .await
            .with_context(|| format!("校验合并输入分段失败: {}", segment.display()))?;
        input_has_audio |= probe.has_audio;
        let size = tokio::fs::metadata(segment)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        match classify_segment_probe(&probe, size) {
            SegmentProbe::Timestamped(duration) => input_duration += duration,
            SegmentProbe::Untimestamped => untimestamped.push(segment.clone()),
            SegmentProbe::Invalid(reason) => {
                return Err(anyhow!("输入分段无效（{reason}）: {}", segment.display()))
            }
        }
    }
    // 纯视频直播流（房间无音频轨）合并后本就无声：只有输入带音频时才要求输出含
    // 音频，否则这类房间每一场都会被判成合并失败。
    if input_has_audio && !output_probe.has_audio {
        return Err(anyhow!("合并输出缺少音频轨道"));
    }
    if !untimestamped.is_empty() {
        warn!(
            count = untimestamped.len(),
            total = segments.len(),
            "部分分段时间戳不可信（探测时长为 0，常见于被强杀的 FLV），已按轨道与体积校验后放行，且不计入输出时长比对"
        );
    }
    if input_duration > 0.0
        && output_probe.duration_secs < input_duration * MIN_MERGED_DURATION_RATIO
    {
        return Err(anyhow!(
            "合并输出时长不足: 输出 {:.3}s，输入可确认合计 {:.3}s（{} 个分段时间戳不可信未计入）",
            output_probe.duration_secs,
            input_duration,
            untimestamped.len()
        ));
    }
    Ok(())
}

/// 分段判定：时长优先；时长缺失/为 0 时退化为「有音视频轨道 + 非空体积」。
fn classify_segment_probe(probe: &MediaProbe, size_bytes: u64) -> SegmentProbe {
    if probe.duration_secs.is_finite() && probe.duration_secs > 0.0 {
        return SegmentProbe::Timestamped(probe.duration_secs);
    }
    if size_bytes == 0 {
        return SegmentProbe::Invalid("文件为空");
    }
    if !probe.has_video && !probe.has_audio {
        return SegmentProbe::Invalid("无音视频轨道");
    }
    SegmentProbe::Untimestamped
}

async fn probe_media(ffmpeg_path: &Path, input: &Path) -> Result<MediaProbe> {
    let probe = ffprobe_path(ffmpeg_path);
    let mut cmd = Command::new(&probe);
    cmd.args([
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
    .kill_on_drop(true);
    let result = match tokio::time::timeout(PROBE_TIMEOUT, probe_output(&mut cmd)).await {
        Ok(result) => result,
        Err(_) => {
            return Err(anyhow!(
                "ffprobe 校验超时（{} 秒）",
                PROBE_TIMEOUT.as_secs()
            ));
        }
    };
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

/// 与 `Command::output()` 等价的进程输出收集（stdin 由调用方置为 null），
/// 但先 spawn 以便把探测进程也纳入 Job Object / pdeathsig 回收。
async fn probe_output(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    let child = child_lifetime::spawn(cmd)?;
    child.wait_with_output().await
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
        // 缺失/为 0 只表示「时长不可信」，由调用方按轨道与体积判定有效性；
        // 强杀留下的 FLV 分段就是 duration=0 但内容可播。
        .unwrap_or(0.0);
    Ok(MediaProbe {
        duration_secs,
        has_video,
        has_audio,
    })
}

async fn probe_media_with_ffmpeg(ffmpeg_path: &Path, input: &Path) -> Result<MediaProbe> {
    let mut cmd = Command::new(ffmpeg_path);
    cmd.args(["-hide_banner", "-i"])
        .arg(input)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // 没有 ffprobe 时回退到 ffmpeg 探测，同样必须可超时：挂住的 ffmpeg 会让
    // 收尾永久停在 Finalizing，且这条路径上没有任何其它超时兜底。
    let result = match timeout(PROBE_TIMEOUT, probe_output(&mut cmd)).await {
        Ok(result) => result,
        Err(_) => {
            return Err(anyhow!(
                "FFmpeg 媒体探测超时（{} 秒）",
                PROBE_TIMEOUT.as_secs()
            ));
        }
    }
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

    let mut child = child_lifetime::spawn(&mut cmd).context("failed to start FFmpeg merge")?;
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
    // 路径 token 逐个替换而不是整体丢弃：FFmpeg stderr 几乎必然包含输入/输出
    // 绝对路径，整体替换会让真实错误（如 "Unrecognized option"）完全不可见。
    let mut rebuilt = String::with_capacity(output.len());
    for (index, part) in output.split_whitespace().enumerate() {
        if index > 0 {
            rebuilt.push(' ');
        }
        if looks_like_absolute_path(part) {
            rebuilt.push_str("<path>");
        } else {
            rebuilt.push_str(part);
        }
    }
    rebuilt.trim().to_owned()
}

fn looks_like_absolute_path(part: &str) -> bool {
    part.starts_with('/')
        || part.starts_with("\\\\")
        || (part.len() >= 3
            && part.as_bytes()[1] == b':'
            && (part.as_bytes()[2] == b'\\' || part.as_bytes()[2] == b'/'))
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
    fn zero_duration_segment_with_tracks_is_untimestamped_not_invalid() {
        // 实测：FLV 分段被强杀后 onMetaData.duration 停在占位 0，ffprobe 仍列出音视频轨。
        let probe = MediaProbe {
            duration_secs: 0.0,
            has_video: true,
            has_audio: true,
        };
        assert_eq!(
            classify_segment_probe(&probe, 4096),
            SegmentProbe::Untimestamped
        );
        let audio_only = MediaProbe {
            duration_secs: 0.0,
            has_video: false,
            has_audio: true,
        };
        assert_eq!(
            classify_segment_probe(&audio_only, 4096),
            SegmentProbe::Untimestamped
        );
        assert_eq!(
            classify_segment_probe(&audio_only, 0),
            SegmentProbe::Invalid("文件为空")
        );
        let streamless = MediaProbe {
            duration_secs: 0.0,
            has_video: false,
            has_audio: false,
        };
        assert_eq!(
            classify_segment_probe(&streamless, 4096),
            SegmentProbe::Invalid("无音视频轨道")
        );
        let timed = MediaProbe {
            duration_secs: 12.5,
            has_video: true,
            has_audio: true,
        };
        assert_eq!(
            classify_segment_probe(&timed, 4096),
            SegmentProbe::Timestamped(12.5)
        );
    }

    #[test]
    fn probe_json_treats_missing_duration_as_unknown() {
        let without_duration =
            br#"{"streams":[{"codec_type":"video"},{"codec_type":"audio"}],"format":{}}"#;
        let probe = parse_probe_json(without_duration).expect("streams 有效即应通过");
        assert_eq!(probe.duration_secs, 0.0);
        assert!(probe.has_video && probe.has_audio);

        let zero = br#"{"streams":[{"codec_type":"video"}],"format":{"duration":"0.000000"}}"#;
        assert_eq!(parse_probe_json(zero).expect("零时长").duration_secs, 0.0);

        let real = br#"{"streams":[{"codec_type":"audio"}],"format":{"duration":"7.5"}}"#;
        assert_eq!(parse_probe_json(real).expect("真实时长").duration_secs, 7.5);
    }

    #[test]
    fn unverified_output_keeps_mp4_name_and_drops_partial() {
        let partial = PathBuf::from("/data/live/room_1/主播 第一段.mp4.partial");
        assert_eq!(
            unverified_output_path(&partial),
            PathBuf::from("/data/live/room_1/主播 第一段.mp4.unverified")
        );
    }

    #[tokio::test]
    async fn merge_of_segment_killed_mid_write_still_produces_output() {
        let ffmpeg = required_ffmpeg_bin();
        let temp = tempdir().expect("temporary directory");
        let healthy = temp.path().join("room_segment_0001.flv");
        generate_fixture(&ffmpeg, &healthy).await;
        let killed = spawn_killed_flv(&ffmpeg, temp.path().join("room_segment_0002.flv")).await;

        let output = merge_segments_to_mp4(&ffmpeg, &[healthy.clone(), killed.clone()])
            .await
            .expect("被强杀的分段只能按未知时长处理，不得毁掉整场合并");
        let probe = probe_media(&ffmpeg, &output)
            .await
            .expect("probe merged output");
        assert!(probe.has_video && probe.has_audio);
        // 输出必须比单个健康分段更大，否则说明强杀分段被静默丢弃。
        let healthy_size = tokio::fs::metadata(&healthy).await.expect("meta").len();
        let killed_size = tokio::fs::metadata(&killed).await.expect("meta").len();
        let output_size = tokio::fs::metadata(&output).await.expect("meta").len();
        assert!(
            output_size > healthy_size + killed_size / 2,
            "merged output too small: {output_size} vs killed {killed_size}"
        );
        assert!(
            killed_size > 1024,
            "强杀分段应保留可用内容，实际 {killed_size} 字节"
        );
    }

    /// 录制中途强杀 FFmpeg：留下的 FLV 分段头部 metadata 停在占位值，
    /// 与生产上 taskkill 的场次一致。
    async fn spawn_killed_flv(ffmpeg: &Path, output: PathBuf) -> PathBuf {
        let mut command = Command::new(ffmpeg);
        command
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x64:rate=25",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=1000:sample_rate=44100",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-flvflags",
                "add_keyframe_index",
                "-f",
                "flv",
            ])
            .arg(&output)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child =
            child_lifetime::spawn(&mut command).expect("start ffmpeg for kill simulation");
        for _ in 0..100 {
            if tokio::fs::metadata(&output)
                .await
                .map(|metadata| metadata.len())
                .unwrap_or(0)
                > 256 * 1024
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        child.kill().await.expect("force kill ffmpeg");
        child.wait().await.expect("reap killed child");
        output
    }

    /// 只含一种媒体轨道的 FLV 分段：`video` 决定保留视频轨还是音频轨。
    async fn generate_single_track_fixture(ffmpeg: &Path, output: &Path, video: bool) {
        let mut command = Command::new(ffmpeg);
        command.args(["-y", "-v", "error", "-f", "lavfi", "-i"]);
        if video {
            command
                .arg("testsrc=size=160x90:rate=25")
                .args(["-t", "1", "-c:v", "libx264", "-pix_fmt", "yuv420p"]);
        } else {
            command
                .arg("sine=frequency=1000:sample_rate=44100")
                .args(["-t", "1", "-c:a", "aac"]);
        }
        let result = command
            .args(["-f", "flv"])
            .arg(output)
            .output()
            .await
            .expect("run ffmpeg single-track fixture generation");
        assert!(
            result.status.success(),
            "fixture generation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[tokio::test]
    async fn merge_video_only_segments_produces_complete_output() {
        // 无音频轨的直播间（纯视频流）真实存在：合并必须走仅视频轨的 remux，
        // 不得因为输出没有音轨就被校验判成失败、把整场留给分段。
        let ffmpeg = required_ffmpeg_bin();
        let temp = tempdir().expect("temporary directory");
        let first = temp.path().join("room_segment_0001.flv");
        let second = temp.path().join("room_segment_0002.flv");
        generate_single_track_fixture(&ffmpeg, &first, true).await;
        generate_single_track_fixture(&ffmpeg, &second, true).await;

        let output = merge_segments_to_mp4(&ffmpeg, &[first.clone(), second.clone()])
            .await
            .expect("纯视频分段应能合并为成品");
        let probe = probe_media(&ffmpeg, &output)
            .await
            .expect("probe merged output");
        assert!(probe.has_video, "合并输出必须保留视频轨");
        assert!(!probe.has_audio, "夹具本就无音轨");
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
    async fn failed_validation_preserves_merged_output_for_rescue() {
        let ffmpeg = required_ffmpeg_bin();
        let temp = tempdir().expect("temporary directory");
        // 无视频轨的分段：合并能跑完，但输出缺视频轨道，校验必然失败。
        let voice_only = temp.path().join("room_segment_0001.flv");
        generate_single_track_fixture(&ffmpeg, &voice_only, false).await;

        let error = merge_segments_to_mp4(&ffmpeg, &[voice_only.clone(), voice_only])
            .await
            .expect_err("缺视频轨的输出必须校验失败");
        let message = error.to_string();
        assert!(
            message.contains("缺少视频轨道"),
            "错误文案需说明失败原因: {message}"
        );
        assert!(
            message.contains("已保留未通过校验的合并产物") && message.contains(".mp4.unverified"),
            "错误文案需说明保留了什么: {message}"
        );
        let salvage = temp.path().join("room_segment_0001.mp4.unverified");
        assert!(
            tokio::fs::metadata(&salvage)
                .await
                .map(|metadata| metadata.len())
                .unwrap_or(0)
                > 0,
            "校验失败必须保留合并产物供用户抢救"
        );
        assert!(
            tokio::fs::metadata(temp.path().join("room_segment_0001.mp4"))
                .await
                .is_err(),
            "未通过校验的输出不得冒充成品"
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

    #[test]
    fn diagnostics_keep_error_text_when_path_present() {
        let value = redact_diagnostics(
            "Unrecognized option 'user_agent'.\nError splitting the argument list: Option not found",
        );
        assert!(value.contains("Unrecognized option"));
        assert!(value.contains("Option not found"));

        let value = redact_diagnostics(
            r"Failed to open D:\data\live\room_1_segment_0.flv: Connection refused",
        );
        assert!(value.contains("Connection refused"));
        assert!(!value.contains(r"D:\data"));
        assert!(value.contains("<path>"));
    }
}
