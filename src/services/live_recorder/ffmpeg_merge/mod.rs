//! FLV 分段合并：concat 清单构建、磁盘空间预检、FFmpeg 合并执行（可取消/超时）
//! 与产物改名。校验与媒体探测见 `ffmpeg_probe`。

#[cfg(test)]
mod tests;

use super::ffmpeg_probe::verify_merged_output;
use super::ffmpeg_session::{read_diagnostic, redact_diagnostics};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const MERGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 合并预检的额外安全余量：除输出体积（约等于输入分段总和）外保留的空间。
const MERGE_FREE_SPACE_MARGIN_BYTES: u64 = 1024 * 1024 * 1024;
/// 校验失败时保留的合并产物后缀：不匹配 `*.mp4`，也不被启动清扫删除。
const UNVERIFIED_SUFFIX: &str = ".unverified";

/// 按顺序合并 FLV 分段并封装为 MP4。
///
/// 本函数不会删除任何输入分段；调用方只有在确认输出文件有效后才能清理输入。
pub(super) async fn merge_segments_to_mp4(
    ffmpeg_path: &Path,
    segments: &[PathBuf],
) -> Result<PathBuf> {
    merge_segments_to_mp4_inner(ffmpeg_path, segments, None).await
}

/// 合并分段，同时允许后台任务终止 FFmpeg 子进程。
/// 取消合并时保留源分段文件。
pub(super) async fn merge_segments_to_mp4_cancelable(
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
        // 清理失败无害：残留的 .partial 会被下次启动清扫。
        tokio::fs::remove_file(&partial_output).await.ok();
    }
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        // 清理失败无害：残留的 .partial 会被下次启动清扫。
        tokio::fs::remove_file(&partial_output).await.ok();
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
            // 清理失败无害：残留的 .partial 会被下次启动清扫。
            tokio::fs::remove_file(&partial_output).await.ok();
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

    let mut child = crate::services::spawn_util::child_lifetime::spawn(&mut cmd)
        .context("failed to start FFmpeg merge")?;
    let stdout_task = tokio::spawn(read_diagnostic(child.stdout.take()));
    let stderr_task = tokio::spawn(read_diagnostic(child.stderr.take()));
    let status = if let Some(cancellation) = cancellation {
        tokio::select! {
            _ = cancellation.cancelled() => {
                // 强杀回收是兜底清理：kill 失败由 kill_on_drop 兜底，不改变取消结果。
                if let Err(error) = child.kill().await {
                    debug!(error = %error, "取消合并时强制终止 FFmpeg 失败，交由 kill_on_drop 回收");
                }
                child.wait().await.ok();
                stdout_task.await.ok();
                stderr_task.await.ok();
                return Err(anyhow!("FFmpeg merge cancelled"));
            }
            result = tokio::time::timeout(MERGE_TIMEOUT, child.wait()) => {
                match result {
                    Ok(result) => result.context("failed to wait for FFmpeg merge")?,
                    Err(_) => {
                        warn!(timeout_secs = MERGE_TIMEOUT.as_secs(), "FFmpeg merge timed out");
                        // 强杀回收是兜底清理：kill 失败由 kill_on_drop 兜底，不改变超时结果。
                        if let Err(error) = child.kill().await {
                            debug!(error = %error, "合并超时后强制终止 FFmpeg 失败，交由 kill_on_drop 回收");
                        }
                        child.wait().await.ok();
                        stdout_task.await.ok();
                        stderr_task.await.ok();
                        return Err(anyhow!("FFmpeg merge timed out"));
                    }
                }
            }
        }
    } else {
        match tokio::time::timeout(MERGE_TIMEOUT, child.wait()).await {
            Ok(result) => result.context("failed to wait for FFmpeg merge")?,
            Err(_) => {
                warn!(
                    timeout_secs = MERGE_TIMEOUT.as_secs(),
                    "FFmpeg merge timed out"
                );
                // 强杀回收是兜底清理：kill 失败由 kill_on_drop 兜底，不改变超时结果。
                if let Err(error) = child.kill().await {
                    debug!(error = %error, "合并超时后强制终止 FFmpeg 失败，交由 kill_on_drop 回收");
                }
                child.wait().await.ok();
                stdout_task.await.ok();
                stderr_task.await.ok();
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

fn escape_concat_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}
