//! Setup 独立端口服务：始终绑定 localhost，提供 Setup 向导页面和 API。
//!
//! - 端口：`main_port + 1`，带 port fallback 逻辑防冲突
//! - 绑定：127.0.0.1（仅本机可访问）
//! - 认证：无需认证（仅本机可访问）
//! - 完成后保留供重新配置

use crate::state::SharedState;
use axum::{
    body::Body,
    http::{header, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::sync::atomic::Ordering;
use tower_http::services::ServeFile;
use tracing::info;

/// 启动 Setup 独立端口服务。
///
/// 始终绑定 127.0.0.1，端口为 `main_port + 1`（带 fallback）。
/// 返回实际绑定的端口号。
pub async fn start_setup_server(state: SharedState) -> anyhow::Result<u16> {
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

    info!("setup server listening on http://127.0.0.1:{actual_port}");

    let app = build_setup_router(state);

    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app.into_make_service()).await {
            tracing::warn!(%error, "setup server 异常退出");
        }
    });

    Ok(actual_port)
}

fn build_setup_router(state: SharedState) -> Router {
    let static_root = state.infra.paths.static_dir();
    let setup_api = crate::api::setup::router();

    Router::new()
        .route("/", get(serve_setup_page))
        .route_service(
            "/setup.css",
            ServeFile::new(static_root.join("css").join("setup.css")),
        )
        .route_service(
            "/setup.js",
            ServeFile::new(static_root.join("js").join("setup.js")),
        )
        .route_service(
            "/bulibuli.ico",
            ServeFile::new(static_root.join("bulibuli.ico")),
        )
        .nest_service(
            "/css",
            tower_http::services::ServeDir::new(static_root.join("css")),
        )
        .nest_service(
            "/js",
            tower_http::services::ServeDir::new(static_root.join("js")),
        )
        .merge(setup_api)
        .layer(middleware::from_fn(add_setup_security))
        .with_state(state)
}

async fn serve_setup_page(
    axum::extract::State(state): axum::extract::State<SharedState>,
) -> Response {
    let static_root = state.infra.paths.static_dir();
    match tokio::fs::read(static_root.join("setup.html")).await {
        Ok(bytes) => {
            let mut response = Response::new(Body::from(bytes));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(error) => {
            tracing::error!(%error, "读取 setup.html 失败");
            (axum::http::StatusCode::NOT_FOUND, "setup.html not found").into_response()
        }
    }
}

async fn add_setup_security(request: Request<Body>, next: Next) -> Response {
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
    let mut response = next.run(request).await;
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

#[cfg(test)]
mod tests {
    use super::origin_authority;

    #[test]
    fn setup_origin_keeps_the_port_in_the_comparison() {
        assert_eq!(origin_authority("http://127.0.0.1:3001/setup"), Some("127.0.0.1:3001"));
        assert!(origin_authority("https://127.0.0.1:3001").is_none());
        assert!(origin_authority("http://127.0.0.1:3002").is_some());
    }
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
