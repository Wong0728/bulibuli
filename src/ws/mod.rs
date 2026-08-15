use crate::services::audit_log::AuditEvent;
use crate::services::auth::SessionAuth;
use crate::services::file_safety::validate_uid;
use anyhow::Result;
use socketioxide::{
    extract::{Data, SocketRef},
    SocketIo,
};
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
    /// 下载进度与博主日志按房间发送，未订阅的客户端不会接收业务事件。
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

            s.on(
                "blogger:logs:subscribe",
                move |s: SocketRef, Data(data): Data<serde_json::Value>| async move {
                    if let Some(raw_uid) = data.get("uid").and_then(|v| v.as_str()) {
                        let uid = match validate_uid(raw_uid) {
                            Ok(uid) => uid,
                            Err(error) => {
                                warn!("[WebSocket] 拒绝无效博主 UID {raw_uid:?}: {error}");
                                return;
                            }
                        };
                        let uid = uid.as_str();
                        let room = format!("blogger:{uid}");
                        s.join(room);
                        if let Err(e) = s.emit(
                            "subscribed",
                            &serde_json::json!({
                                "id": uuid::Uuid::new_v4().to_string(),
                                "uid": uid,
                                "message": format!("已订阅博主 {uid} 的日志更新"),
                            }),
                        ) {
                            warn!("[WebSocket] 发送订阅确认失败: {e}");
                        }
                        info!("[WebSocket] 客户端 {} 订阅博主日志 uid={}", s.id, uid);
                    } else {
                        warn!(
                            "[WebSocket] 客户端 {} subscribe_blogger_logs 缺少 uid 字段",
                            s.id
                        );
                    }
                },
            );

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

            s.on(
                "blogger:logs:unsubscribe",
                move |s: SocketRef, Data(data): Data<serde_json::Value>| async move {
                    if let Some(raw_uid) = data.get("uid").and_then(|v| v.as_str()) {
                        let uid = match validate_uid(raw_uid) {
                            Ok(uid) => uid,
                            Err(error) => {
                                warn!("[WebSocket] 拒绝取消无效博主 UID {raw_uid:?}: {error}");
                                return;
                            }
                        };
                        let uid = uid.as_str();
                        s.leave(format!("blogger:{uid}"));
                        info!("[WebSocket] 客户端 {} 取消订阅博主日志 uid={}", s.id, uid);
                    } else {
                        warn!(
                            "[WebSocket] 客户端 {} unsubscribe_blogger_logs 缺少 uid 字段",
                            s.id
                        );
                    }
                },
            );

            s.on("download:unsubscribe", move |s: SocketRef| async move {
                s.leave("download");
            });

            s.on_disconnect(move |s: SocketRef| async move {
                info!("[WebSocket] 客户端 {} 已断开", s.id);
            });
        });
    }

    pub async fn broadcast_log(&self, uid: Option<&str>, message: &str, level: &str) -> Result<()> {
        let data = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "uid": uid,
            "message": message,
            "level": level,
            "time": chrono::Local::now().format("%H:%M:%S").to_string(),
        });

        if let Some(io) = self.io.get().cloned() {
            if let Some(uid) = uid {
                io.to(format!("blogger:{uid}"))
                    .emit("log:update", &data)
                    .await
                    .map_err(|e| anyhow::anyhow!("广播日志事件失败: {e}"))?;
            } else {
                io.emit("log:update", &data)
                    .await
                    .map_err(|e| anyhow::anyhow!("广播日志事件失败: {e}"))?;
            }
        }
        Ok(())
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
