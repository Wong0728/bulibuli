//! FFmpeg 直播录制会话：进程启动、优雅停止、诊断输出收集与脱敏。

use crate::services::spawn_util::child_lifetime;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, info, warn};

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
            // 进程已不在：仅消费诊断任务避免遗留，输出无需保留。
            self.diagnostics().await;
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
                stop_error = Some(anyhow::anyhow!("FFmpeg 停止超时，已强制终止"));
                if let Err(error) = child.kill().await {
                    stop_error = Some(anyhow::anyhow!("强制终止 FFmpeg 失败: {error}"));
                } else if let Err(error) = child.wait().await {
                    stop_error = Some(anyhow::anyhow!("回收被强制终止的 FFmpeg 失败: {error}"));
                }
            }
            Ok(Err(error)) => {
                stop_error = Some(anyhow::anyhow!("等待 FFmpeg 停止失败: {error}"));
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

/// 持续消费进程输出，同时只保留最后 1 MiB 诊断文本。
pub(super) async fn read_diagnostic<R>(reader: Option<R>) -> Result<Vec<u8>>
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
