//! 连接生命周期主循环：host 轮询、断线重连（指数退避 + 抖动）、
//! 鉴权失败后的 token / 服务器列表刷新、重试预算与熔断。

use super::commands::IncomingLiveCommand;
use super::dedup::SeenKeys;
use super::reconnect::{
    is_auth_error, is_fatal_error, reconnect_backoff, websocket_url, with_reconnect_jitter,
};
use super::{send_connection_status, DanmuCollector};
use crate::services::bili_api::BiliApi;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// 最大重连次数。
const MAX_RECONNECT_ATTEMPTS: u32 = 3;
/// 连接稳定运行达到该时长（一个完整接收超时周期）后断开，视为健康连接的
/// 偶发闪断：重连预算归零重新计数。否则预算全会话累计，第 4 次闪断即永久熔断。
#[cfg(not(test))]
const STABLE_CONNECTION_RESET: Duration = super::connection::RECV_TIMEOUT;
/// 测试构建下用更短的窗口，让「稳定连接后预算归零」可被快速验证。
#[cfg(test)]
const STABLE_CONNECTION_RESET: Duration = Duration::from_millis(150);

impl DanmuCollector {
    /// 主循环：连接 → 认证 → 收消息 → 断线重连。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_loop(
        room_id: i64,
        account_uid: i64,
        mut token: String,
        mut hosts: Vec<(String, i32)>,
        bili_api: Option<Arc<BiliApi>>,
        cookies: Option<String>,
        cancellation: CancellationToken,
        tx: mpsc::Sender<IncomingLiveCommand>,
    ) {
        let mut reconnect_attempts = 0u32;
        let dropped = Arc::new(AtomicU64::new(0));
        let mut seen = SeenKeys::default();

        loop {
            if cancellation.is_cancelled() {
                info!(room_id, "弹幕采集器收到取消信号，退出");
                return;
            }

            // 选择 host（轮询）
            let (host, wss_port) = &hosts[reconnect_attempts as usize % hosts.len()];
            let Some(ws_url) = websocket_url(host, *wss_port) else {
                warn!(room_id, host, "拒绝不受支持的弹幕 WebSocket 端点");
                reconnect_attempts += 1;
                if reconnect_attempts > MAX_RECONNECT_ATTEMPTS {
                    send_connection_status(
                        &tx,
                        "unavailable",
                        Some("弹幕 WebSocket 端点全部无效".to_string()),
                    );
                    return;
                }
                continue;
            };
            if reconnect_attempts == 0 {
                send_connection_status(&tx, "connecting", None);
            }
            tracing::debug!(room_id, %ws_url, "弹幕采集器尝试连接");

            let attempt_started = std::time::Instant::now();
            match Self::connect_and_receive(
                room_id,
                account_uid,
                &token,
                &ws_url,
                bili_api.as_ref(),
                cookies.as_deref(),
                &cancellation,
                &tx,
                &dropped,
                &mut seen,
            )
            .await
            {
                Ok(()) => {
                    info!(room_id, "弹幕采集器正常退出");
                    return;
                }
                Err(e) => {
                    // 连接成功后稳定运行超过一个完整接收周期才断开：视为健康连接
                    // 的偶发闪断，重连预算归零重新计数，避免全会话累计导致
                    // 第 4 次闪断（超过 MAX_RECONNECT_ATTEMPTS）后永久熔断。
                    let uptime = attempt_started.elapsed();
                    if reconnect_attempts > 0 && uptime >= STABLE_CONNECTION_RESET {
                        reconnect_attempts = 0;
                        info!(
                            room_id,
                            ?uptime,
                            "弹幕连接稳定运行后断开，重连预算已归零重新计数"
                        );
                    }
                    send_connection_status(&tx, "degraded", Some(e.to_string()));
                    if is_auth_error(&e) {
                        match (bili_api.as_ref(), cookies.as_deref()) {
                            (Some(bili_api), Some(cookies)) => {
                                match bili_api.live_danmu_conf(room_id, cookies).await {
                                    Ok(conf) if !conf.host_server_list.is_empty() => {
                                        // 鉴权刷新路径同样计入重连预算：账号被持续拒绝
                                        // （封禁/风控）时若归零计数并立即重连，会形成
                                        // 连接→鉴权失败→刷新→重连的无限循环。
                                        reconnect_attempts += 1;
                                        if reconnect_attempts > MAX_RECONNECT_ATTEMPTS {
                                            warn!(
                                                room_id,
                                                attempts = reconnect_attempts,
                                                "弹幕鉴权失败刷新后重试仍持续失败，熔断本次会话"
                                            );
                                            send_connection_status(
                                                &tx,
                                                "unavailable",
                                                Some(e.to_string()),
                                            );
                                            return;
                                        }
                                        token = conf.token;
                                        hosts = conf
                                            .host_server_list
                                            .into_iter()
                                            .map(|host| (host.host, host.wss_port))
                                            .collect();
                                        warn!(
                                            room_id,
                                            attempts = reconnect_attempts,
                                            "弹幕鉴权失败后刷新 token 与服务器列表，退避后重连"
                                        );
                                        tokio::time::sleep(with_reconnect_jitter(
                                            std::time::Duration::from_secs(
                                                5u64.min(reconnect_attempts as u64 * 5),
                                            ),
                                        ))
                                        .await;
                                        continue;
                                    }
                                    Ok(_) => {
                                        warn!(room_id, "鉴权失败后刷新弹幕配置为空，继续使用旧配置")
                                    }
                                    Err(error) => {
                                        warn!(room_id, "鉴权失败后刷新弹幕 token 失败: {error}")
                                    }
                                }
                            }
                            _ => warn!(room_id, "测试传输未配置 B站 API，跳过弹幕 token 刷新"),
                        }
                    }
                    if is_fatal_error(&e) {
                        warn!(room_id, error_chain = ?e, "弹幕采集遇到不可恢复错误，熔断本次会话");
                        send_connection_status(&tx, "unavailable", Some(e.to_string()));
                        return;
                    }
                    reconnect_attempts += 1;
                    if reconnect_attempts > MAX_RECONNECT_ATTEMPTS {
                        warn!(room_id, attempts = reconnect_attempts, error_chain = ?e, "弹幕采集可恢复重试次数耗尽，熔断本次会话");
                        send_connection_status(&tx, "unavailable", Some(e.to_string()));
                        return;
                    }
                    if (reconnect_attempts as usize).is_multiple_of(hosts.len()) {
                        match (bili_api.as_ref(), cookies.as_deref()) {
                            (Some(bili_api), Some(cookies)) => {
                                match bili_api.live_danmu_conf(room_id, cookies).await {
                                    Ok(conf) if !conf.host_server_list.is_empty() => {
                                        token = conf.token;
                                        hosts = conf
                                            .host_server_list
                                            .into_iter()
                                            .map(|host| (host.host, host.wss_port))
                                            .collect();
                                        info!(
                                            room_id,
                                            "弹幕服务器已完整轮换，已刷新 token 与服务器列表"
                                        );
                                    }
                                    Ok(_) => warn!(
                                        room_id,
                                        "刷新弹幕配置返回空服务器列表，继续使用旧配置"
                                    ),
                                    Err(error) => {
                                        warn!(room_id, "刷新弹幕 token 失败，继续重试: {error}")
                                    }
                                }
                            }
                            _ => warn!(room_id, "测试传输未配置 B站 API，跳过弹幕 token 刷新"),
                        }
                    }
                    let backoff = reconnect_backoff(reconnect_attempts);
                    warn!(
                        room_id,
                        attempt = reconnect_attempts,
                        ?backoff,
                        "弹幕采集器连接断开，准备重连: {e}"
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = cancellation.cancelled() => return,
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
    use futures::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn local_websocket_disconnects_retry_then_circuit_breaks() {
        let (listener, host) = local_server().await;
        let port = listener.local_addr().expect("address").port() as i32;
        let server = tokio::spawn(async move {
            for _ in 0..=MAX_RECONNECT_ATTEMPTS {
                let (stream, _) = listener.accept().await.expect("accept client");
                let ws = accept_async(stream).await.expect("upgrade websocket");
                let (mut write, mut read) = ws.split();
                let _consumed = read.next().await;
                write
                    .send(Message::Binary(
                        make_packet(&serde_json::json!({"code": 0}), op::AUTH_REPLY).into(),
                    ))
                    .await
                    .expect("send auth reply");
                write.close().await.expect("close websocket");
            }
        });
        let (tx, mut rx) = mpsc::channel(32);
        let cancellation = CancellationToken::new();
        let hosts = vec![(host, port); (MAX_RECONNECT_ATTEMPTS + 1) as usize];
        let runner = tokio::spawn(DanmuCollector::run_loop(
            1,
            7,
            "token".to_owned(),
            hosts,
            None,
            None,
            cancellation,
            tx,
        ));
        let mut unavailable = false;
        while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await {
            if event
                .raw
                .pointer("/data/status")
                .and_then(serde_json::Value::as_str)
                == Some("unavailable")
            {
                unavailable = true;
                break;
            }
        }
        assert!(
            unavailable,
            "retry exhaustion must publish an unavailable state"
        );
        runner.await.expect("collector runner");
        server.await.expect("server task");
    }

    /// B3 回归：连接成功并稳定运行超过阈值后断开，重连预算必须归零，
    /// 不能因全会话累计闪断次数而永久熔断。
    #[tokio::test]
    async fn stable_connection_resets_reconnect_budget() {
        let (listener, host) = local_server().await;
        let port = listener.local_addr().expect("address").port() as i32;
        let server = tokio::spawn(async move {
            async fn flash_disconnect(listener: &TcpListener) {
                let (stream, _) = listener.accept().await.expect("accept client");
                let ws = accept_async(stream).await.expect("upgrade websocket");
                let (mut write, mut read) = ws.split();
                let _consumed = read.next().await;
                write
                    .send(Message::Binary(
                        make_packet(&serde_json::json!({"code": 0}), op::AUTH_REPLY).into(),
                    ))
                    .await
                    .expect("send auth reply");
                write.close().await.expect("close websocket");
            }
            // 两次闪断：消耗 2 次预算
            flash_disconnect(&listener).await;
            flash_disconnect(&listener).await;
            // 第三次：认证成功后稳定运行超过 STABLE_CONNECTION_RESET 再断开
            let (stream, _) = listener.accept().await.expect("accept client");
            let ws = accept_async(stream).await.expect("upgrade websocket");
            let (mut write, mut read) = ws.split();
            let _consumed = read.next().await;
            write
                .send(Message::Binary(
                    make_packet(&serde_json::json!({"code": 0}), op::AUTH_REPLY).into(),
                ))
                .await
                .expect("send auth reply");
            tokio::time::sleep(Duration::from_millis(500)).await;
            write.close().await.expect("close stable websocket");
            // 若预算未归零，接下来第 4 次闪断会触发永久熔断
            flash_disconnect(&listener).await;
            // 最后一条连接保持打开，等待测试取消
            let (stream, _) = listener.accept().await.expect("accept client");
            let ws = accept_async(stream).await.expect("upgrade websocket");
            let (mut write, mut read) = ws.split();
            let _consumed = read.next().await;
            write
                .send(Message::Binary(
                    make_packet(&serde_json::json!({"code": 0}), op::AUTH_REPLY).into(),
                ))
                .await
                .expect("send auth reply");
            let _kept_open = read.next().await;
            let _closed = write.close().await;
        });

        let (tx, mut rx) = mpsc::channel(64);
        let cancellation = CancellationToken::new();
        let runner = tokio::spawn(DanmuCollector::run_loop(
            1,
            7,
            "token".to_owned(),
            vec![(host, port)],
            None,
            None,
            cancellation.clone(),
            tx,
        ));
        // 共 5 次连接（2 闪断 + 1 稳定 + 1 闪断 + 1 保持），
        // 若稳定连接未归零预算，第 4 次断开后会发布 unavailable。
        let mut capturing = 0usize;
        let mut unavailable = false;
        let mut last_status = String::new();
        while capturing < 5 {
            let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await
            else {
                break;
            };
            let status = event
                .raw
                .pointer("/data/status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<none>")
                .to_string();
            eprintln!("[test] status={status}");
            match status.as_str() {
                "capturing" => capturing += 1,
                "unavailable" => {
                    unavailable = true;
                    break;
                }
                _ => {}
            }
            last_status = status;
        }
        cancellation.cancel();
        // 用超时保护 JoinHandle：熔断路径下 server 会停在多余的 accept 上，
        // 无超时保护会把「断言失败」拖成「测试挂死」。
        let _runner_done = tokio::time::timeout(Duration::from_secs(5), runner).await;
        let _server_done = tokio::time::timeout(Duration::from_secs(5), server).await;
        assert!(!unavailable, "稳定连接后断开不得触发永久熔断");
        assert_eq!(
            capturing, 5,
            "应完成全部 5 次连接（预算已归零），last_status={last_status}"
        );
    }

    async fn local_server() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local WebSocket");
        let address = listener.local_addr().expect("local address");
        (listener, format!("ws://{}", address.ip()))
    }
}
