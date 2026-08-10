use crate::api;
use crate::api::auth::{session_cookie, set_session_cookie};
use crate::error::ApiResponse;
use crate::services::auth::{ClientInfo, SessionAuth};
use crate::services::security_config::{is_effectively_loopback, AccessMode};
use crate::state::bili::BiliState;
use crate::state::SharedState;
use crate::ws::WebSocketManager;
use axum::{
    body::Body,
    extract::{ConnectInfo, Extension, MatchedPath, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use socketioxide::SocketIo;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::signal;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{error, info, warn};

pub async fn serve(
    state: SharedState,
    listener: tokio::net::TcpListener,
    actual_port: u16,
) -> anyhow::Result<()> {
    let security = state.bili.security.current();
    let host = match security.mode {
        AccessMode::Local => "127.0.0.1",
        AccessMode::Lan => "[::]",
        AccessMode::Proxy => "127.0.0.1",
    };
    info!(mode = ?security.mode, "server listening on http://{host}:{actual_port}");
    if security.mode == AccessMode::Lan {
        warn!("LAN 模式使用 HTTP，仅适用于可信局域网；公网访问请使用 proxy + HTTPS");
    }
    let app = build_router(state.clone()).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(state.infra.cancellation.clone()))
    .await?;
    Ok(())
}

/// 根据当前生效的安全模式生成用户可访问的主界面 URL。
/// 端口来自实际 listener，避免端口冲突 fallback 后仍显示配置端口。
pub fn main_url(state: &SharedState) -> String {
    let security = state.bili.security.current();
    let actual_port = state.infra.actual_main_port.load(Ordering::Relaxed);
    let port = if actual_port == 0 {
        state.infra.config.port.max(1)
    } else {
        actual_port
    };
    match security.mode {
        AccessMode::Proxy => security
            .proxy_domain
            .map(|domain| format!("https://{domain}"))
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}")),
        AccessMode::Local | AccessMode::Lan => format!("http://127.0.0.1:{port}"),
    }
}

/// 绑定主服务器端口，根据配置的访问模式选择绑定策略。
///
/// Local 模式明确绑定 IPv4 回环，避免 Windows 上 IPv6 `::1` 成功但
/// `127.0.0.1` 入口不可达的问题。
/// 返回绑定的 `TcpListener` 和实际端口号。
pub async fn bind_main_listener(
    state: &SharedState,
) -> anyhow::Result<(tokio::net::TcpListener, u16)> {
    let security = state.bili.security.current();
    if !state.infra.config.tls_verify {
        warn!("高危配置：TLS 证书验证已关闭，下载凭据可能暴露；禁止进入 proxy 模式");
    }
    if security.mode == AccessMode::Proxy && !state.infra.config.tls_verify {
        anyhow::bail!("proxy 模式禁止关闭 TLS 证书验证");
    }
    let (host, port) = match security.mode {
        AccessMode::Local => ("127.0.0.1", state.infra.config.port),
        AccessMode::Lan => ("[::]", state.infra.config.port),
        AccessMode::Proxy => ("127.0.0.1", state.infra.config.port),
    };
    let listener = if security.mode == AccessMode::Lan {
        bind_lan_with_port_fallback(port).await?
    } else if security.mode == AccessMode::Local {
        bind_with_port_fallback("127.0.0.1", port).await?
    } else if security.mode == AccessMode::Proxy {
        tokio::net::TcpListener::bind((host, port)).await?
    } else {
        bind_with_port_fallback(host, port).await?
    };
    let actual_port = listener.local_addr()?.port();
    state
        .infra
        .actual_main_port
        .store(actual_port, Ordering::Relaxed);
    std::fs::write(
        state.infra.paths.data_dir.join("actual_port.txt"),
        actual_port.to_string(),
    )?;
    Ok((listener, actual_port))
}

async fn build_router(state: SharedState) -> anyhow::Result<Router> {
    let (socket_layer, io) = SocketIo::new_layer();
    WebSocketManager::setup_handlers(&io);
    state.infra.ws.attach(io).await;

    let limiter = Arc::new(governor::RateLimiter::keyed(
        governor::Quota::per_second(NonZeroU32::new(5).expect("non-zero quota"))
            .allow_burst(NonZeroU32::new(15).expect("non-zero burst")),
    ));
    let rate_limit = middleware::from_fn(move |request: Request<Body>, next: Next| {
        let limiter = limiter.clone();
        async move {
            let client_ip = request
                .extensions()
                .get::<ClientInfo>()
                .map(|client| client.ip)
                .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            if limiter.check_key(&client_ip).is_err() {
                return api_error(StatusCode::TOO_MANY_REQUESTS, 429, "请求过于频繁");
            }
            next.run(request).await
        }
    });

    let static_root = state.infra.paths.static_dir();
    let api = api::router().layer(rate_limit);
    let security_layer = middleware::from_fn_with_state(state.clone(), enforce_request_security);
    Ok(Router::new()
        .route("/", get(index))
        .route_service(
            "/favicon.ico",
            ServeFile::new(static_root.join("bilibili.ico")),
        )
        .route_service(
            "/pair.css",
            ServeFile::new(static_root.join("css").join("pair.css")),
        )
        .route_service(
            "/pair.js",
            ServeFile::new(static_root.join("js").join("pair.js")),
        )
        .route_service(
            "/pair-font.woff2",
            ServeFile::new(
                static_root
                    .join("css")
                    .join("lib")
                    .join("webfonts")
                    .join("JetBrains Mono.woff2"),
            ),
        )
        .route_service(
            "/index.html",
            ServeFile::new(static_root.join("index.html")),
        )
        .nest_service("/css", ServeDir::new(static_root.join("css")))
        .nest_service("/js", ServeDir::new(static_root.join("js")))
        .merge(api)
        .layer(middleware::from_fn(trace_request))
        .layer(socket_layer)
        .layer(security_layer)
        .with_state(state))
}

async fn trace_request(request: Request<Body>, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    let response = next.run(request).await;
    info!(%method, %route, status = response.status().as_u16(), "http request");
    response
}

async fn index(
    State(state): State<SharedState>,
    Extension(client): Extension<ClientInfo>,
    headers: HeaderMap,
) -> Response {
    let token = session_cookie(&headers).unwrap_or_default();
    let authenticated = state.bili.auth.authenticate(&token, client.ip).await;
    let (file, session) = match authenticated {
        Ok(Some(session)) => ("index.html", Some(session)),
        Ok(None) => ("pair.html", None),
        Err(error) => return error.into_response(),
    };
    match tokio::fs::read(state.infra.paths.static_dir().join(file)).await {
        Ok(bytes) => {
            let mut response = Response::new(Body::from(bytes));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            if let Some(token) = session.and_then(|value| value.rotated_token) {
                if let Err(error) = set_session_cookie(&state.bili, &mut response, &token) {
                    return error.into_response();
                }
            }
            response
        }
        Err(error) => crate::error::AppError::Io(error).into_response(),
    }
}

async fn enforce_request_security(
    State(state): State<SharedState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|value| value.0)
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
    let config = state.bili.security.current();
    let client_ip = match effective_client_ip(&config.mode, peer.ip(), request.headers()) {
        Ok(ip) => ip,
        Err(response) => return response,
    };
    let (allowed, explicit_allow) = config.client_allowed(client_ip);
    if !allowed {
        return api_error(StatusCode::FORBIDDEN, 403, "当前来源已被访问策略拒绝");
    }
    if !host_allowed(
        &config.mode,
        config.proxy_domain.as_deref(),
        request.headers(),
    ) {
        return api_error(StatusCode::BAD_REQUEST, 400, "Host 不在允许范围内");
    }
    request.extensions_mut().insert(ClientInfo {
        ip: client_ip,
        explicit_allow,
    });

    let path = request.uri().path().to_string();
    let public = matches!(
        path.as_str(),
        "/" | "/favicon.ico"
            | "/pair.css"
            | "/pair.js"
            | "/pair-font.woff2"
            | "/api/health"
            | "/api/ready"
            | "/api/auth/state"
            | "/api/auth/pair"
    );
    let mut session = None;
    if !public {
        // 仅当 IP 在 auth_bypass_ips 配置中明确列出时才跳过认证
        if config.should_bypass_auth(client_ip) {
            tracing::info!(%client_ip, "请求来源 IP 在认证跳过白名单中，跳过认证");
        } else {
            let token = session_cookie(request.headers()).unwrap_or_default();
            match state.bili.auth.authenticate(&token, client_ip).await {
                Ok(Some(value)) => {
                    if let Err(response) = authorize_session(&request, &value) {
                        return *response;
                    }
                    request.extensions_mut().insert(value.clone());
                    session = Some(value);
                }
                Ok(None) => return api_error(StatusCode::UNAUTHORIZED, 401, "需要先完成设备配对"),
                Err(error) => return error.into_response(),
            }
        }
    }
    let bypassed_auth = config.should_bypass_auth(client_ip);
    if path.starts_with("/socket.io/") {
        // Socket.IO 轮询/升级请求无法附带自定义 CSRF 头，改用同源校验兜底：
        // 浏览器同源 GET 不发送 Origin，仅在存在时校验；POST 必带 Origin 且必须匹配。
        if !socket_origin_allowed(&state.bili, request.method(), request.headers()) {
            return api_error(StatusCode::FORBIDDEN, 403, "Socket.IO Origin 无效");
        }
    } else if is_mutating(request.method()) {
        if bypassed_auth {
            // 认证跳过的请求仍需检查 Origin 防跨站，但无需 CSRF Token
            if request
                .headers()
                .get("sec-fetch-site")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value == "cross-site")
            {
                return api_error(StatusCode::FORBIDDEN, 403, "拒绝跨站修改请求");
            }
        } else if let Err(message) = validate_browser_write(&state.bili, &request, session.as_ref())
        {
            return api_error(StatusCode::FORBIDDEN, 403, message);
        }
    }
    let mut response = next.run(request).await;
    if let Some(token) = session.and_then(|value| value.rotated_token) {
        if let Err(error) = set_session_cookie(&state.bili, &mut response, &token) {
            return error.into_response();
        }
    }
    add_security_headers(&mut response);
    response
}

/// Server-side RBAC boundary for the main Web.  UI hiding is only a usability
/// feature; this check prevents direct API calls from escalating a paired
/// Operator or Viewer session.
fn authorize_session(request: &Request<Body>, session: &SessionAuth) -> Result<(), Box<Response>> {
    let path = request.uri().path();
    let mutating = is_mutating(request.method());

    // Account credentials, all infrastructure settings, and device invitations
    // are Owner-only.  `/api/settings` currently contains both business and
    // process-level values, so it remains Owner-only until its business subset
    // is split into its own endpoint.
    let owner_only = path.starts_with("/api/cookies/")
        || path.starts_with("/api/settings")
        || path.starts_with("/api/auth/invitations");
    if owner_only && !session.role.is_owner() {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            403,
            "此操作需要 Owner 权限",
        )));
    }

    // A viewer may inspect status, history, and logs, but never change tasks,
    // bloggers, rules, or recordings.  Logging out is the single safe write.
    if mutating && !session.role.can_operate() && path != "/api/auth/logout" {
        return Err(Box::new(api_error(
            StatusCode::FORBIDDEN,
            403,
            "Viewer 仅可查看内容",
        )));
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
fn effective_client_ip(
    mode: &AccessMode,
    peer: IpAddr,
    headers: &HeaderMap,
) -> Result<IpAddr, Response> {
    if *mode != AccessMode::Proxy {
        return Ok(peer);
    }
    if !is_effectively_loopback(peer) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            403,
            "proxy 模式只信任本机反向代理",
        ));
    }
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, 400, "缺少可信客户端地址"))?;
    if forwarded.contains(',') {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            400,
            "客户端地址头格式无效",
        ));
    }
    forwarded
        .trim()
        .parse()
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, 400, "客户端地址无效"))
}

fn host_allowed(mode: &AccessMode, domain: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    match mode {
        AccessMode::Proxy => {
            domain.is_some_and(|domain| host == domain || host == format!("{domain}:443"))
        }
        AccessMode::Local => {
            host.starts_with("127.0.0.1:")
                || host.starts_with("localhost:")
                || host.starts_with("[::1]:")
        }
        AccessMode::Lan => {
            let without_port = host
                .strip_prefix('[')
                .and_then(|value| value.split_once(']').map(|pair| pair.0))
                .or_else(|| host.rsplit_once(':').map(|pair| pair.0))
                .unwrap_or(host);
            without_port == "localhost" || without_port.parse::<IpAddr>().is_ok()
        }
    }
}

fn validate_browser_write(
    bili: &BiliState,
    request: &Request<Body>,
    session: Option<&SessionAuth>,
) -> Result<(), &'static str> {
    let headers = request.headers();
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "cross-site")
    {
        return Err("拒绝跨站修改请求");
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or("缺少 Host")?;
    let expected = match bili.security.current().mode {
        AccessMode::Proxy => bili
            .security
            .current()
            .proxy_domain
            .map(|domain| format!("https://{domain}"))
            .ok_or("proxy 域名未配置")?,
        AccessMode::Local | AccessMode::Lan => format!("http://{host}"),
    };
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or("缺少 Origin")?;
    if origin != expected {
        return Err("Origin 不在允许范围内");
    }
    if request.uri().path() == "/api/auth/pair" {
        return Ok(());
    }
    let session = session.ok_or("缺少有效会话")?;
    let csrf = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .ok_or("缺少 CSRF Token")?;
    if csrf != session.csrf_token {
        return Err("CSRF Token 无效");
    }
    Ok(())
}

fn socket_origin_allowed(bili: &BiliState, method: &Method, headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "cross-site")
    {
        return false;
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let security = bili.security.current();
    let expected = match security.mode {
        AccessMode::Proxy => security
            .proxy_domain
            .map(|domain| format!("https://{domain}")),
        AccessMode::Local | AccessMode::Lan => Some(format!("http://{host}")),
    };
    let Some(expected) = expected else {
        return false;
    };
    match headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        // 浏览器同源 GET（轮询握手 / WebSocket 升级前）可能不带 Origin，
        // 此时放行并由会话认证兜底；跨站 GET 已被上方 sec-fetch-site 拦截。
        None => *method == Method::GET,
        Some(origin) => origin == expected,
    }
}

fn is_mutating(method: &Method) -> bool {
    matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    )
}

fn add_security_headers(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        // connect-src 仅同源（Socket.IO/WebSocket 均同源），不开放裸 wss: 防连向任意主机
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
}

fn api_error(status: StatusCode, code: i64, message: &'static str) -> Response {
    (
        status,
        axum::Json(ApiResponse::<serde_json::Value> {
            code,
            message: message.to_string(),
            data: serde_json::Value::Null,
        }),
    )
        .into_response()
}

async fn bind_with_port_fallback(
    host: &str,
    start_port: u16,
) -> anyhow::Result<tokio::net::TcpListener> {
    for offset in 0..100 {
        let Some(port) = start_port.checked_add(offset) else {
            break;
        };
        match tokio::net::TcpListener::bind((host, port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow::anyhow!("no available port"))
}

async fn bind_lan_with_port_fallback(start_port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    for offset in 0..100 {
        let Some(port) = start_port.checked_add(offset) else {
            break;
        };
        let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_only_v6(false)?;
        socket.set_reuse_address(true)?;
        socket.set_nonblocking(true)?;
        let address = SocketAddr::new(std::net::Ipv6Addr::UNSPECIFIED.into(), port);
        match socket.bind(&address.into()) {
            Ok(()) => {
                socket.listen(1024)?;
                let listener: std::net::TcpListener = socket.into();
                return Ok(tokio::net::TcpListener::from_std(listener)?);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow::anyhow!("no available dual-stack port"))
}

async fn shutdown_signal(cancellation: tokio_util::sync::CancellationToken) {
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
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
        _ = cancellation.cancelled() => {},
    }
}

#[cfg(test)]
mod role_tests {
    use super::*;
    use crate::services::auth::SessionRole;

    fn session(role: SessionRole) -> SessionAuth {
        SessionAuth {
            id: "test".to_string(),
            csrf_token: "csrf".to_string(),
            rotated_token: None,
            role,
        }
    }

    #[test]
    fn delegated_sessions_cannot_access_credentials_or_settings() {
        let credentials = Request::builder()
            .uri("/api/cookies/status")
            .body(Body::empty())
            .unwrap();
        let settings = Request::builder()
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        assert!(authorize_session(&credentials, &session(SessionRole::Operator)).is_err());
        assert!(authorize_session(&settings, &session(SessionRole::Viewer)).is_err());
        assert!(authorize_session(&credentials, &session(SessionRole::Owner)).is_ok());
    }

    #[test]
    fn viewer_cannot_mutate_but_operator_can_manage_business_actions() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/download/pause")
            .body(Body::empty())
            .unwrap();
        assert!(authorize_session(&request, &session(SessionRole::Viewer)).is_err());
        assert!(authorize_session(&request, &session(SessionRole::Operator)).is_ok());
    }
}
