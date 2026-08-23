//! Setup 独立端口服务：始终绑定 localhost，提供 Setup 向导页面和 API。
//!
//! - 端口：`main_port + 1`，带 port fallback 逻辑防冲突
//! - 绑定：127.0.0.1（仅本机可访问）
//! - 认证：无需认证（仅本机可访问）
//! - 生命周期：默认仅在 onboarding 未完成时存在，向导完成后自动关停
//!   （免认证端口不应常驻）；设 `BILI__SETUP_PORT_ENABLED=true` 可强制常驻，
//!   `=false` 则完全不启动。

use crate::state::SharedState;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use tokio_util::sync::CancellationToken;
use tower_http::services::{ServeDir, ServeFile};
use tracing::info;

/// setup server 的关停句柄；onboarding 完成时取消以触发 graceful shutdown。
static SETUP_SHUTDOWN: OnceLock<CancellationToken> = OnceLock::new();

/// 解析 `BILI__SETUP_PORT_ENABLED`：true=常驻，false=禁用，未设置=None（默认：
/// 仅 onboarding 期间存在）。
fn setup_port_enabled_from_env() -> Option<bool> {
    match std::env::var("BILI__SETUP_PORT_ENABLED").as_deref() {
        Ok("false" | "0" | "off" | "no") => Some(false),
        Ok("true" | "1" | "on" | "yes") => Some(true),
        _ => None,
    }
}

/// 启动 Setup 独立端口服务。
///
/// 始终绑定 127.0.0.1，端口为 `main_port + 1`（带 fallback）。
/// 返回实际绑定的端口号；被禁用或 onboarding 已完成时返回 0（不启动）。
pub async fn start_setup_server(state: SharedState) -> anyhow::Result<u16> {
    let enabled = setup_port_enabled_from_env();
    if enabled == Some(false) {
        state.infra.actual_setup_port.store(0, Ordering::Relaxed);
        info!("setup server 未启动（BILI__SETUP_PORT_ENABLED=false）");
        return Ok(0);
    }
    let onboarding_completed =
        crate::app::onboarding::StartupState::load(&state.infra.paths.data_dir)
            .onboarding_completed;
    if enabled.is_none() && onboarding_completed {
        // 默认模式：免认证端口只在首次配置期间存在，完成后不再监听。
        state.infra.actual_setup_port.store(0, Ordering::Relaxed);
        info!("setup server 未启动（onboarding 已完成；如需重新配置可设 BILI__SETUP_PORT_ENABLED=true 后重启）");
        return Ok(0);
    }

    let main_port = state.infra.config.port;
    let setup_port = main_port
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("主端口 65535 没有可用的 Setup 端口"))?;

    let listener = bind_setup_port(setup_port).await?;
    let actual_port = listener.local_addr()?.port();

    // 写入 InfraState 供 API 查询
    state
        .infra
        .actual_setup_port
        .store(actual_port, Ordering::Relaxed);

    info!(
        "setup server listening on http://127.0.0.1:{actual_port}（首次配置向导专用端口；完成初始化后自动关闭，日常使用主端口的网页管理界面即可）"
    );

    let app = build_setup_router(state.clone());
    let cancellation = SETUP_SHUTDOWN.get_or_init(CancellationToken::new).clone();
    let shutdown = cancellation.clone();

    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
        {
            tracing::warn!(%error, "setup server 异常退出");
        }
        state.infra.actual_setup_port.store(0, Ordering::Relaxed);
        info!("setup server 已关闭");
    });

    Ok(actual_port)
}

/// 关停 setup server（幂等）。onboarding 完成后由 apply 接口调用；
/// 默认模式下自动触发，`BILI__SETUP_PORT_ENABLED=true` 时常驻不关。
pub fn shutdown_setup_server() {
    if let Some(cancellation) = SETUP_SHUTDOWN.get() {
        cancellation.cancel();
    }
}

fn build_setup_router(state: SharedState) -> Router {
    let setup_api = crate::api::setup::router();
    let static_root = state.infra.paths.static_dir();

    // 直接在 Setup 端口上提供完整页面与向导所需的 API（health / auth / setup），
    // 而不是 307 重定向到主端口——首次启动时主端口尚未开始 serve，
    // 重定向只会把用户引向一个暂时无响应的地址。
    Router::new()
        .route_service(
            "/",
            ServeFile::new(static_root.join("app").join("index.html")),
        )
        .route_service(
            "/favicon.ico",
            ServeFile::new(static_root.join("bulibuli.ico")),
        )
        .nest_service("/app", ServeDir::new(static_root.join("app")))
        .merge(crate::api::auth::router())
        .merge(crate::api::health::router())
        .merge(setup_api)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            inject_setup_session,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            add_setup_security,
        ))
        .with_state(state)
}

/// Setup 端口没有主服务的请求安全中间件，这里补一个轻量会话注入：
/// 认证 Cookie 有效则挂上 `Option<SessionAuth>`，让 /api/auth/csrf 等
/// 依赖该 Extension 的 handler 正常工作；setup 端口仅回环可访问，对端恒为本机。
async fn inject_setup_session(
    State(state): State<SharedState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let ip: IpAddr = "127.0.0.1".parse().expect("static loopback ip");
    let token = crate::api::auth::session_cookie(request.headers()).unwrap_or_default();
    let session = state
        .bili
        .auth
        .authenticate(&token, ip)
        .await
        .ok()
        .flatten();
    request.extensions_mut().insert(session);
    next.run(request).await
}

async fn add_setup_security(
    State(state): State<SharedState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let host_allowed = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_loopback_host_header);
    if !host_allowed {
        return (StatusCode::BAD_REQUEST, "setup only accepts loopback Host").into_response();
    }
    if matches!(
        request.method(),
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    ) {
        // CSRF 防护必须绑定到当前 Setup 服务的完整 authority（包含端口）。
        // 仅判断 localhost/127.0.0.1 会放行其它本机端口上的恶意页面。
        let origin_allowed = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .and_then(origin_authority)
            .and_then(|origin| {
                request
                    .headers()
                    .get(header::HOST)
                    .and_then(|v| v.to_str().ok())
                    .map(|host| origin.eq_ignore_ascii_case(host.trim()))
            })
            .unwrap_or(false);
        if !origin_allowed {
            return (
                StatusCode::FORBIDDEN,
                "setup request requires a loopback Origin",
            )
                .into_response();
        }
    }
    let path = request.uri().path().to_string();
    let mut response = next.run(request).await;
    // 默认模式：onboarding 完成即关停这个免认证端口，避免其常驻。
    if path == "/api/setup/apply"
        && response.status().is_success()
        && setup_port_enabled_from_env().is_none()
        && crate::app::onboarding::StartupState::load(&state.infra.paths.data_dir)
            .onboarding_completed
    {
        info!(
            "onboarding 已完成，自动关闭 setup 端口（设 BILI__SETUP_PORT_ENABLED=true 可保持常驻）"
        );
        shutdown_setup_server();
    }
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn is_loopback_host_header(value: &str) -> bool {
    let value = value.trim();
    let host = if let Some(end) = value.find(']') {
        value
            .get(usize::from(value.starts_with('['))..end)
            .unwrap_or_default()
    } else {
        value.split(':').next().unwrap_or(value)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn origin_authority(value: &str) -> Option<&str> {
    let authority = value.strip_prefix("http://")?;
    let authority = authority.split('/').next()?;
    (!authority.is_empty() && is_loopback_host_header(authority)).then_some(authority)
}

/// 绑定 Setup 端口：绑定 127.0.0.1（IPv4 回环），带 port fallback。
async fn bind_setup_port(start_port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    for offset in 0..10 {
        let Some(port) = start_port.checked_add(offset) else {
            break;
        };
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                if offset > 0 {
                    info!("Setup 端口 {start_port} 不可用，已回退到 {port}");
                }
                return Ok(listener);
            }
            Err(error) => {
                tracing::warn!("绑定 127.0.0.1:{port} 失败: {error}，尝试下一个端口");
                continue;
            }
        }
    }
    Err(anyhow::anyhow!(
        "Setup 端口 {start_port} 起连续 10 个端口均不可用"
    ))
}

#[cfg(test)]
mod tests {
    use super::origin_authority;

    #[test]
    fn setup_origin_keeps_the_port_in_the_comparison() {
        assert_eq!(
            origin_authority("http://127.0.0.1:3001/setup"),
            Some("127.0.0.1:3001")
        );
        assert!(origin_authority("https://127.0.0.1:3001").is_none());
        assert!(origin_authority("http://127.0.0.1:3002").is_some());
    }
}
