//! B 站直播弹幕 WebSocket 采集器。
//!
//! 连接流程：
//! 1. 从 `live_danmu_conf` 获取 token + host 列表
//! 2. 建立 WebSocket 连接（`wss://{host}:{wss_port}/sub`）
//! 3. 5 秒内发送认证包（op=7，使用登录账号 UID）
//! 4. 收到认证回复后启动心跳定时器（30 秒间隔）
//! 5. 循环接收消息 → 解压 → 解析命令 → 通过 channel 发送
//! 6. 仅对可恢复网络错误重连（最多 3 次，指数退避）

pub mod commands;
pub mod protocol;

use crate::services::bili_api::BiliApi;
use anyhow::{anyhow, Context, Result};
use commands::IncomingLiveCommand;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use protocol::{make_auth_packet, make_heartbeat_packet, parse_commands, validate_auth_reply};
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, COOKIE, ORIGIN, USER_AGENT};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// 最大重连次数。
/// 心跳间隔。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// 接收超时（60 秒无消息视为断连）。
const RECV_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RECONNECT_ATTEMPTS: u32 = 3;
const MAX_SEEN_EVENT_KEYS: usize = 20_000;

/// 弹幕采集器：管理 WebSocket 连接生命周期。
pub struct DanmuCollector;

impl DanmuCollector {
    /// 启动弹幕采集。
    ///
    /// 返回 `(JoinHandle, Receiver)`：
    /// - `JoinHandle`：后台任务句柄，可用于等待任务结束
    /// - `Receiver<LiveCommand>`：命令接收端，调用方循环读取即可
    ///
    /// 通过 `CancellationToken` 控制停止。
    pub async fn start(
        room_id: i64,
        token: String,
        hosts: Vec<(String, i32)>,
        bili_api: Arc<BiliApi>,
        cookies: String,
        cancellation: CancellationToken,
    ) -> Result<(
        tokio::task::JoinHandle<()>,
        mpsc::Receiver<IncomingLiveCommand>,
    )> {
        if hosts.is_empty() {
            return Err(anyhow!("弹幕服务器列表为空"));
        }
        let credential = crate::services::credential::Credential::from_cookie_header(&cookies);
        if !credential.is_logged_in() {
            return Err(anyhow!("互动采集需要有效的 B站登录态"));
        }
        let account_uid = credential
            .dede_user_id
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|uid| *uid > 0)
            .ok_or_else(|| anyhow!("登录 Cookie 缺少有效 DedeUserID，无法认证弹幕连接"))?;

        let (tx, rx) = mpsc::channel(4096);

        let handle = tokio::spawn(async move {
            Self::run_loop(
                room_id,
                account_uid,
                token,
                hosts,
                Some(bili_api),
                Some(cookies),
                cancellation,
                tx,
            )
            .await;
        });

        Ok((handle, rx))
    }

    /// 主循环：连接 → 认证 → 收消息 → 断线重连。
    #[allow(clippy::too_many_arguments)]
    async fn run_loop(
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
                    let _ = tx.try_send(connection_status(
                        "unavailable",
                        Some("弹幕 WebSocket 端点全部无效".to_string()),
                    ));
                    return;
                }
                continue;
            };
            if reconnect_attempts == 0 {
                let _ = tx.try_send(connection_status("connecting", None));
            }
            debug!(room_id, %ws_url, "弹幕采集器尝试连接");

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
                    let _ = tx.try_send(connection_status("degraded", Some(e.to_string())));
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
                                            let _ = tx.try_send(connection_status(
                                                "unavailable",
                                                Some(e.to_string()),
                                            ));
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
                                        tokio::time::sleep(std::time::Duration::from_secs(
                                            5u64.min(reconnect_attempts as u64 * 5),
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
                        let _ = tx.try_send(connection_status("unavailable", Some(e.to_string())));
                        return;
                    }
                    reconnect_attempts += 1;
                    if reconnect_attempts > MAX_RECONNECT_ATTEMPTS {
                        warn!(room_id, attempts = reconnect_attempts, error_chain = ?e, "弹幕采集可恢复重试次数耗尽，熔断本次会话");
                        let _ = tx.try_send(connection_status("unavailable", Some(e.to_string())));
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

    /// 单次连接 → 认证 → 收消息循环。
    ///
    /// 正常退出（cancellation）返回 Ok，异常返回 Err。
    #[allow(clippy::too_many_arguments)]
    async fn connect_and_receive(
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
        request.headers_mut().insert(
            USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0 BilibiliLiveRecorder"),
        );
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
        let _ = tx.try_send(connection_status("capturing", None));

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
                            if let Message::Binary(data) = msg {
                                match parse_commands(&data) {
                                    Ok(commands) => {
                                        for cmd_json in commands {
                                            let pending = dropped.swap(0, Ordering::Relaxed);
                                            if pending > 0 {
                                                let gap = IncomingLiveCommand::from_json(serde_json::json!({"cmd":"CAPTURE_GAP","data":{"dropped":pending,"reason":"interaction_queue_full"}}));
                                                if tx.send(gap).await.is_err() { return Ok(()); }
                                            }
                                            let cmd = IncomingLiveCommand::from_json(cmd_json);
                                            if let commands::LiveCommand::Danmaku { uid, text, .. } = &cmd.command {
                                                if !seen.insert(history_key(&cmd.raw, *uid, text)) { continue; }
                                            }
                                            let low_priority = cmd.command.is_low_priority()
                                                && !commands::is_link_command(&cmd.cmd);
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

fn websocket_url(host: &str, wss_port: i32) -> Option<String> {
    #[cfg(test)]
    if host.starts_with("ws://") {
        return Some(format!("{host}:{wss_port}/sub"));
    }
    let raw = if host.starts_with("wss://") {
        format!("{host}:{wss_port}/sub")
    } else {
        format!("wss://{host}:{wss_port}/sub")
    };
    crate::services::bili_url_policy::validate_live_endpoint_syntax(&raw, true)
        .ok()
        .map(|url| url.to_string())
}

/// 去重集合：HashSet 判重 + VecDeque 记录插入顺序。
/// 满容量时按最旧插入淘汰——此前按 HashSet 迭代序随机淘汰，
/// 可能淘汰刚插入的 key 使去重立即失效。
#[derive(Default)]
pub(super) struct SeenKeys {
    set: HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl SeenKeys {
    /// 插入并返回是否为新 key。容量超限时先淘汰最旧条目。
    fn insert(&mut self, key: String) -> bool {
        if self.set.len() >= MAX_SEEN_EVENT_KEYS {
            while let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
                if self.set.len() < MAX_SEEN_EVENT_KEYS {
                    break;
                }
            }
        }
        self.order.push_back(key.clone());
        self.set.insert(key)
    }
}

fn reconnect_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    #[cfg(test)]
    {
        Duration::from_millis(20u64 * 2u64.pow(exponent))
    }
    #[cfg(not(test))]
    {
        Duration::from_secs(2u64.pow(exponent).min(30))
    }
}

fn is_fatal_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("操作码非法")
        || message.contains("缺少 code")
        || message.contains("协议版本")
        || message.contains("包头")
}

fn is_auth_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("认证失败")
        || message.contains("认证回复")
        || message.contains("操作码非法")
        || message.contains("-101")
        || message.contains("-352")
        || message.contains("-412")
        || message.contains("-799")
        || message.contains("unauthorized")
}

fn history_key(value: &serde_json::Value, uid: i64, text: &str) -> String {
    if let Some(id) = value
        .get("id_str")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/data/id_str")
                .and_then(serde_json::Value::as_str)
        })
    {
        return format!("id:{id}");
    }
    // 无服务端 id 时的近似去重：加宽时间桶到 3 秒，降低同用户短时间重发相同
    // 弹幕被误丢的概率（此前 10 秒桶在刷屏场景明显丢计数）。
    format!("msg:{uid}:{text}:{}", chrono::Utc::now().timestamp() / 3)
}

fn connection_status(status: &str, error: Option<String>) -> IncomingLiveCommand {
    IncomingLiveCommand::from_json(serde_json::json!({
        "cmd": "DANMU_CONNECTION_STATUS", "data": { "status": status, "error": error }
    }))
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
            let _ = read.next().await;
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
                    if matches!(event.command, commands::LiveCommand::Danmaku { .. }) { break event; }
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
            let _ = read.next().await;
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

    #[tokio::test]
    async fn local_websocket_disconnects_retry_then_circuit_breaks() {
        let (listener, host) = local_server().await;
        let port = listener.local_addr().expect("address").port() as i32;
        let server = tokio::spawn(async move {
            for _ in 0..=MAX_RECONNECT_ATTEMPTS {
                let (stream, _) = listener.accept().await.expect("accept client");
                let ws = accept_async(stream).await.expect("upgrade websocket");
                let (mut write, mut read) = ws.split();
                let _ = read.next().await;
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
}
