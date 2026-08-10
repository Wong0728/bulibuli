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
use tracing::{debug, error, info, warn};

const MAX_DIAGNOSTIC_BYTES: usize = 1024 * 1024;

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
            Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).trim().to_string(),
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

        // `q` is FFmpeg's portable graceful-stop command. Closing stdin after
        // it is written lets the muxer finish its trailer on Windows and Unix.
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
    if segments.is_empty() {
        return Err(anyhow!("没有可合并的直播分段"));
    }

    let first = segments
        .first()
        .expect("segments checked above")
        .to_path_buf();
    let output = first.with_extension("mp4");
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

    let result = run_merge_command(ffmpeg_path, &list_path, &output).await;
    if let Err(error) = tokio::fs::remove_file(&list_path).await {
        debug!(path = %list_path.display(), "删除临时分段清单失败: {error}");
    }

    let output_size = tokio::fs::metadata(&output)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    match result {
        Ok(()) if output_size > 0 => {
            info!(output = %output.display(), segments = segments.len(), "直播分段合并完成");
            Ok(output)
        }
        Ok(()) => Err(anyhow!("FFmpeg 合并完成但输出 MP4 为空")),
        Err(error) => Err(error),
    }
}

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
}
