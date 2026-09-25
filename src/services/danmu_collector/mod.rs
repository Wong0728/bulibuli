//! B 站直播弹幕 WebSocket 采集器。
//!
//! 连接流程：
//! 1. 从 `live_danmu_conf` 获取 token + host 列表
//! 2. 建立 WebSocket 连接（`wss://{host}:{wss_port}/sub`）
//! 3. 5 秒内发送认证包（op=7，使用登录账号 UID）
//! 4. 收到认证回复后启动心跳定时器（30 秒间隔）
//! 5. 循环接收消息 → 解压 → 解析命令 → 通过 channel 发送
//! 6. 仅对可恢复网络错误重连（指数退避 + 抖动；连接稳定运行后重连预算归零，
//!    避免全会话累计导致偶发闪断被永久熔断）
//!
//! 子模块职责：
//! - `commands`：推送 JSON 命令的类型化解析与命令识别分类
//! - `parsers`：各命令 JSON 载荷的字段提取
//! - `protocol`：WebSocket 二进制协议（封包 / 解包 / 解压）
//! - `runner`：连接生命周期主循环（重连、token 刷新、熔断）
//! - `connection`：单次连接的认证、历史回填与消息接收
//! - `dedup`：事件去重（有界 SeenKeys 与去重 key 构造）
//! - `reconnect`：重连退避、抖动与错误分类

pub mod commands;
pub mod protocol;

mod connection;
mod dedup;
mod parsers;
mod reconnect;
mod runner;

use crate::services::bili_api::BiliApi;
use anyhow::{anyhow, Result};
use commands::IncomingLiveCommand;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

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
}

/// 构造连接状态事件。
fn connection_status(status: &str, error: Option<String>) -> IncomingLiveCommand {
    IncomingLiveCommand::from_json(serde_json::json!({
        "cmd": "DANMU_CONNECTION_STATUS", "data": { "status": status, "error": error }
    }))
}

/// 尽力投递连接状态事件：通道满或接收端已关闭时丢弃并留 debug 日志。
/// 状态事件是辅助信息，不应阻塞采集主循环，也不应在接收端关闭后报错中断。
fn send_connection_status(
    tx: &mpsc::Sender<IncomingLiveCommand>,
    status: &str,
    error: Option<String>,
) {
    if let Err(error) = tx.try_send(connection_status(status, error)) {
        debug!("连接状态事件 {status} 未投递: {error}");
    }
}
