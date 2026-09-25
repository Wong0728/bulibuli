//! 服务器生命周期：serve 主循环与优雅关停。
//!
//! 关停时机：先等服务真的收到关闭信号（Ctrl+C / SIGTERM / cancellation），
//! 然后给一个有限宽限期等待在途连接（含 Socket.IO WebSocket）清理完毕。

use crate::state::SharedState;
use std::net::SocketAddr;
use tokio::signal;
use tracing::{error, info, warn};

pub async fn serve(
    state: SharedState,
    listener: tokio::net::TcpListener,
    actual_port: u16,
) -> anyhow::Result<()> {
    let security = state.bili.security.current();
    let host = match security.mode {
        crate::services::security_config::AccessMode::Local => "127.0.0.1",
        crate::services::security_config::AccessMode::Lan => "[::]",
        crate::services::security_config::AccessMode::Proxy => "127.0.0.1",
    };
    info!(mode = ?security.mode, "server listening on http://{host}:{actual_port}");
    if security.mode == crate::services::security_config::AccessMode::Lan {
        warn!("LAN 模式使用 HTTP，仅适用于可信局域网；公网访问请使用 proxy + HTTPS");
    }
    let app = super::build_router(state.clone()).await?;
    // 关停时机：先等服务真的收到关闭信号（Ctrl+C / SIGTERM / cancellation），
    // 然后给一个有限宽限期等待在途连接（含 Socket.IO WebSocket）清理完毕。
    // 浏览器页面开着就会让 Ctrl+C 后的进程永久挂起，所以宽限期到了就放弃
    // 等待（连接随 serve future 被 drop 而关闭），让 main 的清理链路得以执行。
    //
    // 注意：宽限期的计时起点是"收到关闭信号"，不是"serve() 开始"。
    // 之前用 `tokio::time::timeout(SHUTDOWN_GRACE, server)` 错误地让计时器从
    // serve() 开始跑，导致程序在无人关机的情况下也会在 10 秒后被自爆。
    const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(state.infra.cancellation.clone()));
    // race: server 自然结束（graceful shutdown 完成）vs 收到信号 + 宽限期到。
    // 任一分支触发后，丢弃另一个 future；丢弃 server task 会强制关闭所有在途连接。
    use std::future::IntoFuture;
    let mut server = Box::pin(server.into_future());
    let grace_after_signal = async {
        shutdown_signal(state.infra.cancellation.clone()).await;
        info!(
            "收到关闭信号，开始 {} 秒优雅关停宽限期",
            SHUTDOWN_GRACE.as_secs()
        );
        // 主动断开 Socket.IO 连接：浏览器页面开着时 WS 连接永不结束，
        // 优雅关停只能等满宽限期（Ctrl+C 后进程要多挂最长 10 秒才能退出）。
        // 主动断开后连接随即收敛，宽限期退化为兜底上限。
        if let Err(error) = state.infra.ws.disconnect_session("all").await {
            warn!("关停时主动断开 Socket.IO 连接失败: {error}");
        }
        tokio::time::sleep(SHUTDOWN_GRACE).await;
    };
    tokio::pin!(grace_after_signal);
    tokio::select! {
        result = &mut server => {
            result?;
        }
        _ = &mut grace_after_signal => {
            warn!("优雅关停宽限期（{} 秒）已到，仍有活跃连接，强制关闭服务", SHUTDOWN_GRACE.as_secs());
        }
    }
    Ok(())
}

pub(crate) async fn shutdown_signal(cancellation: tokio_util::sync::CancellationToken) {
    let ctrl_c = async {
        if let Err(error) = signal::ctrl_c().await {
            error!("installing Ctrl+C signal handler failed: {error}");
        }
    };
    #[cfg(windows)]
    let terminate = async {
        match signal::windows::ctrl_close() {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => error!("installing close signal handler failed: {error}"),
        }
    };
    // CTRL_BREAK：测试模式（build.py）把本程序放进独立进程组，Ctrl+C 被父进程
    // 收到后经 GenerateConsoleCtrlEvent 显式转发为本事件，是测试工作流的主要
    // 优雅关停入口。
    #[cfg(windows)]
    let ctrl_break = async {
        match signal::windows::ctrl_break() {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => error!("installing ctrl-break signal handler failed: {error}"),
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => error!("installing SIGTERM handler failed: {error}"),
        }
    };
    #[cfg(not(any(windows, unix)))]
    let terminate = std::future::pending::<()>();
    #[cfg(windows)]
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
        _ = ctrl_break => {},
        _ = cancellation.cancelled() => {},
    }
    #[cfg(not(windows))]
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
        _ = cancellation.cancelled() => {},
    }
}
