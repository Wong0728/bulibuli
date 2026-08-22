use crate::services::audit_log::AuditEvent;
use crate::services::auth::SessionAuth;
use anyhow::Result;
use socketioxide::{extract::SocketRef, SocketIo};
use std::sync::Arc;
use tokio::sync::{broadcast, OnceCell};
use tracing::{info, warn};

#[derive(Clone)]
pub struct WebSocketManager {
    io: Arc<OnceCell<SocketIo>>,
}

impl WebSocketManager {
    pub fn new() -> Self {
        Self {
            io: Arc::new(OnceCell::new()),
        }
    }

    pub async fn attach(&self, io: SocketIo) {
        if self.io.set(io).is_err() {
            warn!("[WebSocket] Socket.IO 已附加，忽略重复 attach");
        }
    }

    /// 注册 Socket.IO 事件处理器。
    /// 下载进度按房间发送，未订阅的客户端不会接收业务事件。
    /// 博主日志不在此推送：WS 房间链路已移除，前端统一走 /api/logs/blogger HTTP 轮询。
    pub fn setup_handlers(io: &SocketIo) {
        io.ns("/", async move |s: SocketRef| {
            let session_id = s
                .req_parts()
                .extensions
                .get::<SessionAuth>()
                .map(|session| session.id.clone());
            let Some(session_id) = session_id else {
                if let Err(error) = s.disconnect() {
                    warn!("拒绝无会话 Socket.IO 连接失败: {error}");
                }
                return;
            };
            s.join(format!("session:{session_id}"));
            info!("[WebSocket] 客户端已连接: {}", s.id);
            if let Err(e) = s.emit(
                "connected",
                &serde_json::json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "message": "连接成功"
                }),
            ) {
                warn!("[WebSocket] 发送连接确认失败: {e}");
            }

            s.on("download:subscribe", move |s: SocketRef| async move {
                s.join("download");
                if let Err(e) = s.emit(
                    "subscribed",
                    &serde_json::json!({
                        "id": uuid::Uuid::new_v4().to_string(),
                        "message": "已订阅下载进度更新",
                    }),
                ) {
                    warn!("[WebSocket] 发送订阅确认失败: {e}");
                }
                info!("[WebSocket] 客户端 {} 订阅下载进度", s.id);
            });

            s.on("download:unsubscribe", move |s: SocketRef| async move {
                s.leave("download");
            });

            s.on_disconnect(move |s: SocketRef| async move {
                info!("[WebSocket] 客户端 {} 已断开", s.id);
            });
        });
    }

    /// 广播下载进度。`bvid` 包含在 `data` 内，由前端区分任务。
    pub async fn broadcast_download_progress(&self, mut data: serde_json::Value) -> Result<()> {
        if let Some(object) = data.as_object_mut() {
            object
                .entry("id")
                .or_insert_with(|| serde_json::Value::String(uuid::Uuid::new_v4().to_string()));
        }
        if let Some(io) = self.io.get().cloned() {
            io.to("download")
                .emit("download:progress", &data)
                .await
                .map_err(|e| anyhow::anyhow!("广播下载进度失败: {e}"))?;
        }
        Ok(())
    }

    pub async fn disconnect_session(&self, session_id: &str) -> Result<()> {
        if let Some(io) = self.io.get().cloned() {
            if session_id == "all" {
                io.disconnect()
                    .await
                    .map_err(|error| anyhow::anyhow!("disconnect all sessions: {error}"))?;
            } else {
                io.to(format!("session:{session_id}"))
                    .disconnect()
                    .await
                    .map_err(|error| anyhow::anyhow!("disconnect session: {error}"))?;
            }
        }
        Ok(())
    }

    pub async fn broadcast_system(&self, event: &str, mut data: serde_json::Value) -> Result<()> {
        if let Some(object) = data.as_object_mut() {
            object
                .entry("id")
                .or_insert_with(|| serde_json::Value::String(uuid::Uuid::new_v4().to_string()));
        }
        if let Some(io) = self.io.get().cloned() {
            io.emit(event, &data)
                .await
                .map_err(|error| anyhow::anyhow!("broadcast {event} failed: {error}"))?;
        }
        Ok(())
    }
}

/// 审计事件桥接：订阅 `AuditEventSender` 广播通道，把每条审计事件转发给 WebSocket 客户端。
///
/// 这样前端 Web GUI 可实时感知 AI/TUI/其他端的写操作（操作成功后才广播，符合
/// “操作未提交前不广播”的设计原则）。敏感操作（Cookie 保存等）走 `record_silent`，
/// 不经此通道，事件流不暴露隐私信息。
///
/// 在 `main.rs` 启动时 spawn 一次即可；任务随 `cancellation` 取消退出。
pub fn start_audit_event_bridge(
    ws: Arc<WebSocketManager>,
    mut rx: broadcast::Receiver<AuditEvent>,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let payload = match serde_json::to_value(&event) {
                        Ok(value) => value,
                        Err(error) => {
                            warn!("[WebSocket] 序列化审计事件失败: {error}");
                            continue;
                        }
                    };
                    if let Err(error) = ws.broadcast_system("audit:event", payload).await {
                        warn!("[WebSocket] 广播审计事件失败: {error}");
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("[WebSocket] 审计事件通道已关闭，桥接任务退出");
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!("[WebSocket] 审计事件桥接跟不上节奏，丢弃 {skipped} 条事件（DB 仍完整）");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::audit_log::AuditEvent;
    use std::time::Duration;

    #[tokio::test]
    async fn unattached_manager_broadcasts_are_noop_success() {
        // 未 attach Socket.IO 时所有广播/断开都必须安全地返回 Ok，不得 panic。
        let manager = WebSocketManager::new();
        manager
            .broadcast_download_progress(serde_json::json!({"bvid": "BV1xx411c7mD"}))
            .await
            .expect("broadcast_download_progress");
        manager
            .disconnect_session("some-session")
            .await
            .expect("disconnect_session");
        manager
            .disconnect_session("all")
            .await
            .expect("disconnect all");
        manager
            .broadcast_system("audit:event", serde_json::json!({}))
            .await
            .expect("broadcast_system");
    }

    #[tokio::test]
    async fn attach_is_idempotent_and_broadcasts_stay_safe() {
        let manager = WebSocketManager::new();
        let (layer1, io1) = SocketIo::new_layer();
        WebSocketManager::setup_handlers(&io1);
        manager.attach(io1).await;
        // 重复 attach：应被忽略而不是 panic / 覆盖。
        let (_layer2, io2) = SocketIo::new_layer();
        manager.attach(io2).await;
        drop(layer1);
        // 已 attach 但无客户端连接：广播仍必须成功（空房间 emit 是合法操作）。
        manager
            .broadcast_download_progress(serde_json::json!({"bvid": "BV"}))
            .await
            .expect("progress to empty room");
        manager
            .disconnect_session("nobody")
            .await
            .expect("disconnect empty room");
    }

    #[tokio::test]
    async fn audit_event_bridge_survives_events_and_closed_channel() {
        let manager = Arc::new(WebSocketManager::new());
        let (layer, io) = SocketIo::new_layer();
        WebSocketManager::setup_handlers(&io);
        manager.attach(io).await;
        drop(layer);

        let (tx, rx) = tokio::sync::broadcast::channel::<AuditEvent>(8);
        start_audit_event_bridge(manager.clone(), rx);
        let event = AuditEvent {
            at: "2026-08-21T00:00:00Z".to_string(),
            source: "test",
            caller_id: "caller".to_string(),
            route_or_command: "/api/backup".to_string(),
            target_type: "db".to_string(),
            target_id: None,
            action: "backup".to_string(),
            outcome: "Success",
            new_version: None,
            request_id: "req-1".to_string(),
        };
        // 事件可序列化且发送后桥接不 panic。
        serde_json::to_value(&event).expect("serialize audit event");
        tx.send(event).expect("send audit event");
        // 关闭通道后桥接任务应正常退出（Closed 分支），不 panic、不悬挂进程。
        drop(tx);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
