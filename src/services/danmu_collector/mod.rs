//! B 站直播弹幕 WebSocket 采集器。
//!
//! 连接流程：
//! 1. 从 `live_danmu_conf` 获取 token + host 列表
//! 2. 建立 WebSocket 连接（`wss://{host}:{wss_port}/sub`）
//! 3. 5 秒内发送认证包（op=7, uid=0 游客模式）
//! 4. 收到认证回复后启动心跳定时器（30 秒间隔）
//! 5. 循环接收消息 → 解压 → 解析命令 → 通过 channel 发送
//! 6. 断线自动重连（最多 3 次，指数退避）

pub mod commands;
pub mod protocol;

use crate::services::bili_api::BiliApi;
use anyhow::{anyhow, Context, Result};
use commands::IncomingLiveCommand;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use protocol::{make_auth_packet, make_heartbeat_packet, parse_commands};
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// 最大重连次数。
/// 心跳间隔。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// 接收超时（60 秒无消息视为断连）。
const RECV_TIMEOUT: Duration = Duration::from_secs(60);

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

        let (tx, rx) = mpsc::channel(4096);

        let handle = tokio::spawn(async move {
            Self::run_loop(room_id, token, hosts, bili_api, cookies, cancellation, tx).await;
        });

        Ok((handle, rx))
    }

    /// 主循环：连接 → 认证 → 收消息 → 断线重连。
    async fn run_loop(
        room_id: i64,
        mut token: String,
        mut hosts: Vec<(String, i32)>,
        bili_api: Arc<BiliApi>,
        cookies: String,
        cancellation: CancellationToken,
        tx: mpsc::Sender<IncomingLiveCommand>,
    ) {
        let mut reconnect_attempts = 0u32;
        let dropped = Arc::new(AtomicU64::new(0));
        let mut seen = HashSet::new();

        loop {
            if cancellation.is_cancelled() {
                info!(room_id, "弹幕采集器收到取消信号，退出");
                return;
            }

            // 选择 host（轮询）
            let (host, wss_port) = &hosts[reconnect_attempts as usize % hosts.len()];
            let ws_url = format!("wss://{host}:{wss_port}/sub");
            if reconnect_attempts == 0 {
                let _ = tx.try_send(connection_status("connecting", None));
            }
            debug!(room_id, %ws_url, "弹幕采集器尝试连接");

            match Self::connect_and_receive(
                room_id,
                &token,
                &ws_url,
                &bili_api,
                &cookies,
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
                    reconnect_attempts += 1;
                    if reconnect_attempts as usize % hosts.len() == 0 {
                        match bili_api.live_danmu_conf(room_id, &cookies).await {
                            Ok(conf) if !conf.host_server_list.is_empty() => {
                                token = conf.token;
                                hosts = conf
                                    .host_server_list
                                    .into_iter()
                                    .map(|host| (host.host, host.wss_port))
                                    .collect();
                                info!(room_id, "弹幕服务器已完整轮换，已刷新 token 与服务器列表");
                            }
                            Ok(_) => warn!(room_id, "刷新弹幕配置返回空服务器列表，继续使用旧配置"),
                            Err(error) => warn!(room_id, "刷新弹幕 token 失败，继续重试: {error}"),
                        }
                    }
                    let exponent = reconnect_attempts.saturating_sub(1).min(5);
                    let backoff = Duration::from_secs(2u64.pow(exponent).min(30));
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
        token: &str,
        ws_url: &str,
        bili_api: &Arc<BiliApi>,
        cookies: &str,
        cancellation: &CancellationToken,
        tx: &mpsc::Sender<IncomingLiveCommand>,
        dropped: &Arc<AtomicU64>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .context("WebSocket 连接失败")?;

        let (mut write, mut read) = ws_stream.split();

        // 发送认证包
        let auth_packet = make_auth_packet(room_id, token);
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

        if let Message::Binary(data) = auth_reply {
            let commands = parse_commands(&data).context("解析认证回复失败")?;
            for cmd in &commands {
                if let Some(code) = cmd.get("code").and_then(|v| v.as_i64()) {
                    if code != 0 {
                        return Err(anyhow!("弹幕认证失败: code={code}"));
                    }
                }
            }
        }

        info!(room_id, "弹幕认证成功");
        let _ = tx.try_send(connection_status("capturing", None));

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
                                            let low_priority = matches!(cmd.command, commands::LiveCommand::WatchedChange { .. } | commands::LiveCommand::Interact { .. })
                                                || matches!(&cmd.command, commands::LiveCommand::Other { cmd } if !["VOICE_JOIN", "LINK_MIC", "PK_", "LIVE_MULTI_VIEW"].iter().any(|prefix| cmd.starts_with(prefix)));
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
    format!("msg:{uid}:{text}:{}", chrono::Utc::now().timestamp() / 10)
}

fn connection_status(status: &str, error: Option<String>) -> IncomingLiveCommand {
    IncomingLiveCommand::from_json(serde_json::json!({
        "cmd": "DANMU_CONNECTION_STATUS", "data": { "status": status, "error": error }
    }))
}
