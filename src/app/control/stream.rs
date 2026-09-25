//! IPC 请求/响应帧读写与事件流。
//!
//! 职责：把字节流切成一条 ctl 请求、把执行结果写成统一 JSON 信封、
//! 以及 `events --watch` 的长连接 JSON Lines 推送。传输层（命名管道/Unix socket）见 [`super::transport`]。

use crate::error::{AppError, AppResult};
use crate::state::SharedState;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::helpers::{error_code_for, format_response};

/// 单条命令最大字节数（IPC 请求侧与客户端发送侧共用同一上限）。
pub(super) const MAX_COMMAND_BYTES: usize = 8 * 1024;

/// IPC 请求读取的空闲超时：客户端连接后长时间不发送（或不关写端）时
/// 断开连接，防止单个慢速/挂起客户端长期占用服务端任务。
const IPC_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// 客户端等一条 ctl 响应的总超时。`events --watch` 这类流式命令不受此约束
/// （它们本就要一直推），见 [`read_control_response`] 的 `streaming` 参数。
const IPC_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// 一次 ctl 请求的读取结果。
enum ControlRequestRead {
    Complete,
    Timeout,
    TooLong,
}

/// 读取一条以 `\n` 收尾的 ctl 请求。
///
/// 请求必须由换行符（或对端关闭）界定：Windows 命名管道的客户端没有半关闭语义
/// （`DisconnectNamedPipe` 只能服务端调用），客户端 `shutdown()` 不产生 EOF，
/// 用 `read_to_end` 等结束会一直卡到空闲超时——Windows 上每条 `ctl` 命令都会失败。
async fn read_control_request<S>(
    stream: &mut S,
    buffer: &mut Vec<u8>,
) -> AppResult<ControlRequestRead>
where
    S: AsyncRead + Unpin,
{
    let mut byte = [0u8; 1];
    loop {
        let read = match tokio::time::timeout(IPC_READ_TIMEOUT, stream.read(&mut byte)).await {
            Ok(result) => result?,
            Err(_) => return Ok(ControlRequestRead::Timeout),
        };
        if read == 0 {
            return Ok(ControlRequestRead::Complete);
        }
        if byte[0] == b'\n' {
            return Ok(ControlRequestRead::Complete);
        }
        buffer.push(byte[0]);
        if buffer.len() > MAX_COMMAND_BYTES {
            return Ok(ControlRequestRead::TooLong);
        }
    }
}

/// 读取服务端写回的一条 ctl 响应。
///
/// 响应侧不能照搬请求侧的换行框定：`format_response` 用的是
/// `serde_json::to_string_pretty`，响应体本身是多行、内部全是换行，按第一个 `\n`
/// 截断只会拿到 `{` 或半截字段。JSON 单值自终止，因此这里改用两条硬约束：
///
/// 1. **总超时**（`events --watch` 等流式命令除外，它们本就要一直推）：
///    正常路径靠服务端任务结束 drop 管道句柄产生的 EOF 收尾，但 Windows 命名管道的
///    `poll_shutdown` 只做 flush 不产生 EOF，一旦服务端任务挂住客户端就无限等待。
/// 2. **空响应一律算失败**：`handle_stream` 在读请求阶段直接返回 Err、或 `execute`
///    panic 时，句柄被 drop 而一个字节都没写。原先客户端拿到空串后按成功返回，
///    `main.rs` 的 JSON 解析失败又落回兜底分支，最终 `exit 0`（假成功）。
pub(super) async fn read_control_response<S>(stream: &mut S, streaming: bool) -> AppResult<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut buffer = Vec::new();
    if streaming {
        stream.read_to_end(&mut buffer).await?;
    } else {
        tokio::time::timeout(IPC_RESPONSE_TIMEOUT, stream.read_to_end(&mut buffer))
            .await
            .map_err(|_| {
                AppError::Internal(format!(
                    "控制响应读取超时（{} 秒未收到完整响应）",
                    IPC_RESPONSE_TIMEOUT.as_secs()
                ))
            })??;
    }
    if buffer.iter().all(u8::is_ascii_whitespace) {
        return Err(AppError::Internal(
            "控制响应为空：服务端未返回任何内容".to_string(),
        ));
    }
    Ok(buffer)
}

/// 判断是否为流式命令（events --watch），供 handle_stream 路由到流式处理。
pub(super) fn is_streaming_command(args: &[String]) -> bool {
    args.first().is_some_and(|c| c == "events") && args.iter().any(|a| a == "--watch")
}

/// 流式处理 `events --watch`：订阅 AuditEventSender，逐条写 JSON Lines 到客户端。
/// 30 秒无事件时发空行保活；客户端断开时退出。
async fn handle_streaming<S>(mut stream: S, state: SharedState) -> AppResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use tokio::sync::broadcast::error::RecvError;
    let mut rx = state.infra.audit_log.subscribe();
    // 先发送 hello，让客户端确认流已建立
    let hello = json!({
        "type": "stream_opened",
        "message": "audit event stream connected",
    });
    stream.write_all(format!("{}\n", hello).as_bytes()).await?;
    stream.flush().await?;
    loop {
        match tokio::time::timeout(Duration::from_secs(30), rx.recv()).await {
            Ok(Ok(event)) => {
                let line = serde_json::to_string(&event).unwrap_or_default() + "\n";
                if stream.write_all(line.as_bytes()).await.is_err() {
                    break; // 客户端已断开
                }
                if stream.flush().await.is_err() {
                    break;
                }
            }
            Ok(Err(RecvError::Closed)) => break,
            Ok(Err(RecvError::Lagged(_))) => {
                // 客户端跟不上节奏导致事件丢失，发送一条 warning 说明情况。
                // 通知失败可忽略：客户端大概率已断开，下一轮循环即退出。
                let warn = json!({"type": "lagged", "message": "部分事件被丢弃"});
                stream
                    .write_all(format!("{}\n", warn).as_bytes())
                    .await
                    .ok();
                stream.flush().await.ok();
            }
            Err(_) => {
                // 30 秒超时：发送空行保活，供客户端判断连接仍存活（写失败同样忽略）。
                if stream.write_all(b"\n").await.is_err() {
                    break;
                }
                stream.flush().await.ok();
            }
        }
    }
    // 关停通知，失败可忽略：客户端读侧靠 EOF/超时自行收尾。
    stream.shutdown().await.ok();
    Ok(())
}

pub(super) async fn handle_stream<S>(mut stream: S, state: SharedState) -> AppResult<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buffer = Vec::new();
    match read_control_request(&mut stream, &mut buffer).await {
        Ok(ControlRequestRead::Complete) => {}
        // 超时也走统一错误信封，客户端能区分"连上了但被服务端断开"和"连接失败"。
        Ok(ControlRequestRead::Timeout) => {
            let body = json!({
                "ok": false,
                "error": format!("控制命令读取超时（{} 秒无完整请求）", IPC_READ_TIMEOUT.as_secs()),
                "code": "IPC_TIMEOUT",
            });
            stream.write_all(format_response(&body).as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
        Ok(ControlRequestRead::TooLong) => {
            let body = json!({"ok": false, "error": "控制命令过长", "code": "BAD_REQUEST"});
            stream.write_all(format_response(&body).as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    // 解析失败返回统一错误信封（而非直接断开让客户端看到空响应/UTF-8 错误），
    // 与 execute 层的错误信封格式保持一致。
    let args: Vec<String> = match serde_json::from_slice(&buffer) {
        Ok(args) => args,
        Err(error) => {
            let body = json!({
                "ok": false,
                "error": format!("控制命令不是合法的 JSON 参数数组: {error}"),
                "code": "BAD_REQUEST",
            });
            stream.write_all(format_response(&body).as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
    };
    // 流式命令（events --watch）：路由到 handle_streaming，保持长连接推 JSON Lines
    // 但先走 AI 模式门控：未启用时拒绝订阅事件流（避免泄露审计信息）
    if is_streaming_command(&args) {
        if !state.infra.ai_skill_enabled.load(Ordering::Relaxed) {
            let body = json!({
                "ok": false,
                "error": "AI Skill 模式未启用，events --watch 不可用",
                "code": "AI_SKILL_DISABLED"
            });
            stream.write_all(format_response(&body).as_bytes()).await?;
            stream.shutdown().await?;
            return Ok(());
        }
        return handle_streaming(stream, state).await;
    }
    let response = super::execute(&state, &args).await;
    let body = match response {
        Ok(value) => json!({"ok": true, "data": value}),
        Err(error) => {
            let code = error_code_for(&error);
            json!({"ok": false, "error": error.to_string(), "code": code})
        }
    };
    stream.write_all(format_response(&body).as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// 换行即视为请求完整：Windows 命名管道客户端不会产生 EOF。
    #[tokio::test]
    async fn control_request_ends_at_newline() {
        let mut stream = Cursor::new(&b"[\"pair\"]\ntrailing"[..]);
        let mut buffer = Vec::new();
        let outcome = read_control_request(&mut stream, &mut buffer)
            .await
            .expect("读取请求");
        assert!(matches!(outcome, ControlRequestRead::Complete));
        assert_eq!(buffer, br#"["pair"]"#.to_vec(), "换行后的字节不得混入请求");
    }

    /// 老客户端（不发换行、直接关写端）仍然可用：EOF 同样界定请求结束。
    #[tokio::test]
    async fn control_request_accepts_eof_without_newline() {
        let mut stream = Cursor::new(&br#"["help"]"#[..]);
        let mut buffer = Vec::new();
        let outcome = read_control_request(&mut stream, &mut buffer)
            .await
            .expect("读取请求");
        assert!(matches!(outcome, ControlRequestRead::Complete));
        assert_eq!(buffer, br#"["help"]"#.to_vec());
    }

    #[tokio::test]
    async fn control_request_rejects_oversized_payload() {
        let payload = vec![b'a'; MAX_COMMAND_BYTES + 8];
        let mut stream = Cursor::new(&payload[..]);
        let mut buffer = Vec::new();
        let outcome = read_control_request(&mut stream, &mut buffer)
            .await
            .expect("读取请求");
        assert!(matches!(outcome, ControlRequestRead::TooLong));
    }

    /// 服务端一个字节都没写就 drop 句柄（handle_stream 提前返回 Err / execute panic）：
    /// 空响应必须报错，不能被上层判成成功。
    #[tokio::test]
    async fn control_response_rejects_empty_payload() {
        let mut stream = Cursor::new(&[][..]);
        let error = read_control_response(&mut stream, false)
            .await
            .expect_err("空响应应判为错误");
        assert!(error.to_string().contains("空"), "错误文案应指出空响应");
    }

    /// 只写了空白字节同样算没有响应：避免上层拿到 "\n" 后打印空行并 exit 0。
    #[tokio::test]
    async fn control_response_rejects_whitespace_only_payload() {
        let mut stream = Cursor::new(&b"  \n\t\n"[..]);
        assert!(read_control_response(&mut stream, false).await.is_err());
    }

    /// 响应体是 pretty JSON，内部全是换行：读取必须取回完整文档，
    /// 这正是响应侧不能照搬请求侧换行框定的原因。
    #[tokio::test]
    async fn control_response_keeps_pretty_json_with_inner_newlines() {
        let body = format_response(&json!({"ok": true, "data": {"a": 1, "b": 2}}));
        assert!(body.lines().count() > 1, "pretty JSON 应为多行");
        let mut stream = Cursor::new(body.clone().into_bytes());
        let read = read_control_response(&mut stream, false)
            .await
            .expect("读取响应");
        assert_eq!(read, body.into_bytes(), "不得在第一个换行处截断响应");
    }

    /// 流式命令（events --watch）按 EOF 收尾且不加总超时：超时会砍断长连接订阅。
    #[tokio::test]
    async fn control_response_accepts_json_lines_for_streaming_commands() {
        let payload = b"{\"ok\":true,\"data\":{}}\n{\"ok\":true,\"data\":{}}\n";
        let mut stream = Cursor::new(&payload[..]);
        let read = read_control_response(&mut stream, true)
            .await
            .expect("流式响应");
        assert_eq!(read, payload.to_vec());
    }
}
