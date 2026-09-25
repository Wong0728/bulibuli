//! 单次 WebSocket 连接的完整生命周期：握手请求构造、认证、历史弹幕回填、
//! 心跳与消息接收循环。断线重连与 token 刷新由 `runner` 负责。

use super::commands::{is_link_command, IncomingLiveCommand, LiveCommand};
use super::dedup::{history_key, SeenKeys};
use super::protocol::{
    make_auth_packet, make_heartbeat_packet, parse_commands, validate_auth_reply,
};
use super::{send_connection_status, DanmuCollector};
use crate::services::bili_api::BiliApi;
use anyhow::{anyhow, Context, Result};
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, COOKIE, ORIGIN, USER_AGENT};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

/// 心跳间隔。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// 接收超时（60 秒无消息视为断连）。
pub(super) const RECV_TIMEOUT: Duration = Duration::from_secs(60);

impl DanmuCollector {
    /// 单次连接 → 认证 → 收消息循环。
    ///
    /// 正常退出（cancellation）返回 Ok，异常返回 Err。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn connect_and_receive(
        room_id: i64,
        account_uid: i64,
        token: &str,
        ws_url: &str,
        bili_api: Option<&Arc<BiliApi>>,
        cookies: Option<&str>,
        cancellation: &CancellationToken,
        tx: &mpsc::Sender<IncomingLiveCommand>,
        dropped: &Arc<AtomicU64>,
        seen: &mut SeenKeys,
    ) -> Result<()> {
        // 先从 URL 构造请求，让 tungstenite 补齐 WebSocket 握手所需的请求头，
        // 尤其是 Sec-WebSocket-Key。
        let mut request = ws_url
            .into_client_request()
            .context("build WebSocket request failed")?;
        // WS 握手的 UA 必须与设备指纹上报给 ExClimbWuzhi 的那份一致：同一会话里出现
        // 两种客户端特征本身就是风控命中面。拿不到配置时（无 BiliApi 的测试路径）才回退。
        const FALLBACK_USER_AGENT: &str = "Mozilla/5.0 BilibiliLiveRecorder";
        let user_agent = bili_api
            .and_then(|api| HeaderValue::from_str(api.user_agent()).ok())
            .unwrap_or_else(|| HeaderValue::from_static(FALLBACK_USER_AGENT));
        request.headers_mut().insert(USER_AGENT, user_agent);
        request.headers_mut().insert(
            ORIGIN,
            HeaderValue::from_static("https://live.bilibili.com"),
        );
        if let Some(cookies) = cookies.filter(|value| !value.trim().is_empty()) {
            if let Ok(value) = HeaderValue::from_str(cookies) {
                request.headers_mut().insert(COOKIE, value);
            }
        }
        let (ws_stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .context("WebSocket 连接失败")?;

        let (mut write, mut read) = ws_stream.split();

        // 发送认证包
        let auth_packet = make_auth_packet(room_id, account_uid, token);
        write
            .send(Message::Binary(auth_packet.into()))
            .await
            .context("发送认证包失败")?;

        debug!(room_id, "已发送弹幕认证包");

        // 等待认证回复（5 秒超时）
        let auth_reply = tokio::time::timeout(Duration::from_secs(5), read.next())
            .await
            .context("等待认证回复超时")?
            .context("认证回复流结束")?
            .context("接收认证回复失败")?;

        let Message::Binary(data) = auth_reply else {
            return Err(anyhow!("认证回复不是二进制协议包"));
        };
        validate_auth_reply(&data).context("校验认证回复失败")?;

        info!(room_id, "弹幕认证成功");
        send_connection_status(tx, "capturing", None);

        if let (Some(bili_api), Some(cookies)) = (bili_api, cookies) {
            if let Ok(history) = bili_api.live_recent_danmaku(room_id, cookies).await {
                for item in history {
                    let text = item
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let uid = item
                        .get("uid")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0);
                    let name = item
                        .get("nickname")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let key = history_key(&item, uid, text);
                    if seen.insert(key) {
                        let mut incoming = IncomingLiveCommand::from_json(serde_json::json!({
                            "cmd":"DANMU_MSG", "info":[[], text, [uid, name]], "history_id":item.get("id_str")
                        }));
                        incoming.history_backfill = true;
                        if tx.send(incoming).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }

        // 心跳定时器
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.tick().await; // 消耗首次立即触发

        // 消息接收循环
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    debug!(room_id, "弹幕采集器收到取消");
                    return Ok(());
                }
                _ = heartbeat.tick() => {
                    let packet = make_heartbeat_packet();
                    if let Err(e) = write.send(Message::Binary(packet.into())).await {
                        return Err(anyhow!("发送心跳失败: {e}"));
                    }
                    debug!(room_id, "已发送弹幕心跳");
                }
                result = tokio::time::timeout(RECV_TIMEOUT, read.next()) => {
                    match result {
                        Ok(Some(Ok(msg))) => {
                            match msg {
                                Message::Binary(data) => {
                                match parse_commands(&data) {
                                    Ok(commands) => {
                                        for cmd_json in commands {
                                            let pending = dropped.swap(0, Ordering::Relaxed);
                                            if pending > 0 {
                                                let gap = IncomingLiveCommand::from_json(serde_json::json!({"cmd":"CAPTURE_GAP","data":{"dropped":pending,"reason":"interaction_queue_full"}}));
                                                if tx.send(gap).await.is_err() { return Ok(()); }
                                            }
                                            let cmd = IncomingLiveCommand::from_json(cmd_json);
                                            if let LiveCommand::Danmaku { uid, text, .. } = &cmd.command {
                                                if !seen.insert(history_key(&cmd.raw, *uid, text)) { continue; }
                                            }
                                            let low_priority = cmd.command.is_low_priority()
                                                && !is_link_command(&cmd.cmd);
                                            let sent = if low_priority {
                                                match tx.try_send(cmd) {
                                                    Ok(()) => true,
                                                    Err(mpsc::error::TrySendError::Full(_)) => { dropped.fetch_add(1, Ordering::Relaxed); true },
                                                    Err(mpsc::error::TrySendError::Closed(_)) => false,
                                                }
                                            } else { tx.send(cmd).await.is_ok() };
                                            if !sent {
                                                debug!(room_id, "弹幕 channel 接收端已关闭");
                                                return Ok(());
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        debug!(room_id, "解析弹幕命令失败: {e}");
                                    }
                                }
                                }
                                // 服务端主动关闭：立即断开重连，而不是忽略 Close 帧
                                // 后干等下一个接收超时（最长 60s）才判定断连。
                                Message::Close(_) => {
                                    return Err(anyhow!("WebSocket 服务端关闭连接"));
                                }
                                _ => {}
                            }
                        }
                        Ok(Some(Err(e))) => {
                            return Err(anyhow!("WebSocket 接收错误: {e}"));
                        }
                        Ok(None) => {
                            return Err(anyhow!("WebSocket 流结束"));
                        }
                        Err(_) => {
                            return Err(anyhow!("WebSocket 接收超时 ({RECV_TIMEOUT:?})"));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::danmu_collector::protocol::{make_packet, op};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    async fn local_server() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local WebSocket");
        let address = listener.local_addr().expect("local address");
        (listener, format!("ws://{}", address.ip()))
    }

    #[tokio::test]
    async fn local_websocket_authenticates_and_delivers_tail_event() {
        let (listener, host) = local_server().await;
        let port = listener.local_addr().expect("address").port() as i32;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let ws = accept_async(stream).await.expect("upgrade websocket");
            let (mut write, mut read) = ws.split();
            assert!(matches!(read.next().await, Some(Ok(Message::Binary(_)))));
            write
                .send(Message::Binary(
                    make_packet(&serde_json::json!({"code": 0}), op::AUTH_REPLY).into(),
                ))
                .await
                .expect("send auth reply");
            write
                .send(Message::Binary(
                    make_packet(
                        &serde_json::json!({
                            "cmd": "DANMU_MSG", "info": [[], "tail", [42, "tester"]]
                        }),
                        op::COMMAND,
                    )
                    .into(),
                ))
                .await
                .expect("send tail event");
            let _drained = read.next().await;
        });

        let (tx, mut rx) = mpsc::channel(8);
        let cancellation = CancellationToken::new();
        let dropped = Arc::new(AtomicU64::new(0));
        let mut seen = SeenKeys::default();
        let url = format!("{host}:{port}/sub");
        let run = DanmuCollector::connect_and_receive(
            1,
            7,
            "token",
            &url,
            None,
            None,
            &cancellation,
            &tx,
            &dropped,
            &mut seen,
        );
        tokio::pin!(run);
        let event = loop {
            tokio::select! {
                result = &mut run => panic!("collector exited before event: {result:?}"),
                event = rx.recv() => {
                    let event = event.expect("collector event");
                    if matches!(event.command, LiveCommand::Danmaku { .. }) { break event; }
                }
            }
        };
        assert_eq!(
            event
                .raw
                .pointer("/info/1")
                .and_then(serde_json::Value::as_str),
            Some("tail")
        );
        cancellation.cancel();
        run.await.expect("cancelled collector exits cleanly");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn local_websocket_rejects_invalid_auth_reply() {
        let (listener, host) = local_server().await;
        let port = listener.local_addr().expect("address").port() as i32;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let ws = accept_async(stream).await.expect("upgrade websocket");
            let (mut write, mut read) = ws.split();
            let _consumed = read.next().await;
            write
                .send(Message::Binary(
                    make_packet(&serde_json::json!({"code": -101}), op::AUTH_REPLY).into(),
                ))
                .await
                .expect("send rejected auth reply");
        });
        let (tx, _rx) = mpsc::channel(8);
        let cancellation = CancellationToken::new();
        let dropped = Arc::new(AtomicU64::new(0));
        let mut seen = SeenKeys::default();
        let error = DanmuCollector::connect_and_receive(
            1,
            7,
            "token",
            &format!("{host}:{port}/sub"),
            None,
            None,
            &cancellation,
            &tx,
            &dropped,
            &mut seen,
        )
        .await
        .expect_err("invalid auth must fail");
        assert!(error.to_string().contains("认证"));
        server.await.expect("server task");
    }
}
