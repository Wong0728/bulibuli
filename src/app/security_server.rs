use crate::api;
use crate::api::auth::{session_cookie, set_session_cookie};
use crate::error::ApiResponse;
use crate::services::auth::{ClientInfo, SessionAuth};
use crate::services::security_config::{is_effectively_loopback, AccessMode};
use crate::state::bili::BiliState;
use crate::state::SharedState;
use crate::ws::WebSocketManager;
use anyhow::Context;
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
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(state.infra.cancellation.clone()));
    // graceful shutdown 会等待所有在途连接（含 Socket.IO WebSocket 长连接）结束且无超时；
    // 浏览器页面开着就会让 Ctrl+C 后的进程永久挂起。给一个有限宽限期，超时后放弃
    // 等待（连接随 serve future 被 drop 而关闭），让 main 的清理链路得以执行。
    const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
    match tokio::time::timeout(SHUTDOWN_GRACE, server).await {
        Ok(result) => result?,
        Err(_) => {
            warn!("优雅关机宽限期（10 秒）已到，仍有活跃连接，强制关闭服务");
        }
    }
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
        warn!("高危配置：匿名兼容请求已关闭 TLS 证书验证；带登录态请求仍严格校验，仅建议用于 Local/LAN 故障排查");
    }
    validate_tls_policy(&security.mode, state.infra.config.tls_verify)?;
    let (host, port) = match security.mode {
        AccessMode::Local => ("127.0.0.1", state.infra.config.port),
        AccessMode::Lan => ("[::]", state.infra.config.port),
        AccessMode::Proxy => ("127.0.0.1", state.infra.config.port),
    };
    let listener = if security.mode == AccessMode::Lan {
        bind_lan_with_port_fallback(port).await?
    } else {
        // Local/Proxy 模式统一使用 IPv4 fallback。
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

fn validate_tls_policy(mode: &AccessMode, tls_verify: bool) -> anyhow::Result<()> {
    if *mode == AccessMode::Proxy && !tls_verify {
        anyhow::bail!("proxy 模式禁止关闭 TLS 证书验证");
    }
    Ok(())
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
    let static_pages = Arc::new(StaticPages {
        index: tokio::fs::read(static_root.join("index.html"))
            .await
            .context("读取 index.html 失败")?,
        pair: tokio::fs::read(static_root.join("pair.html"))
            .await
            .context("读取 pair.html 失败")?,
    });
    let api = api::router().layer(rate_limit);
    let security_layer = middleware::from_fn_with_state(state.clone(), enforce_request_security);
    Ok(Router::new()
        .route("/", get(index))
        .route_service(
            "/favicon.ico",
            ServeFile::new(static_root.join("bulibuli.ico")),
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
        .route("/index.html", get(index))
        .route_service(
            "/settings.html",
            ServeFile::new(static_root.join("settings.html")),
        )
        .nest_service("/css", ServeDir::new(static_root.join("css")))
        .nest_service("/js", ServeDir::new(static_root.join("js")))
        .merge(api)
        .layer(middleware::from_fn(trace_request))
        .layer(socket_layer)
        .layer(Extension(static_pages))
        .layer(security_layer)
        .with_state(state))
}

struct StaticPages {
    index: Vec<u8>,
    pair: Vec<u8>,
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
    Extension(pages): Extension<Arc<StaticPages>>,
    Extension(client): Extension<ClientInfo>,
    headers: HeaderMap,
) -> Response {
    let token = session_cookie(&headers).unwrap_or_default();
    let authenticated = state.bili.auth.authenticate(&token, client.ip).await;
    let (bytes, session) = match authenticated {
        Ok(Some(session)) => (pages.index.clone(), Some(session)),
        Ok(None) => (pages.pair.clone(), None),
        Err(error) => return error.into_response(),
    };
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
    // 总是注入 Option<SessionAuth>：auth_bypass_ips 命中时没有 SessionAuth，
    // 依赖会话上下文的 handler（logout/邀请）以 Extension<Option<SessionAuth>> 提取，
    // 避免 axum MissingExtension 直接 500。
    request.extensions_mut().insert(session.clone());
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
    add_security_headers(&mut response, &path);
    response
}

/// 主 Web 界面的服务端 RBAC 边界。隐藏 UI 仅改善使用体验；
/// 此检查阻止已配对的 Operator/Viewer 会话通过直接调用 API 越权。
fn authorize_session(request: &Request<Body>, session: &SessionAuth) -> Result<(), Box<Response>> {
    let path = request.uri().path();
    let mutating = is_mutating(request.method());

    // 账号凭证、全部基础设施设置和设备邀请仅限 Owner 操作。
    // `/api/settings` 目前同时包含业务和进程级配置，因此在拆分出独立业务接口前，
    // 在拆分出独立业务接口前，继续保持 Owner-only。
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

    // Viewer 可查看状态、历史和日志，但不能修改任务、
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

fn parse_host_authority(raw: &str) -> Option<(String, Option<u16>)> {
    let url = url::Url::parse(&format!("http://{raw}")).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url
        .host_str()?
        .trim_matches(&['[', ']'][..])
        .to_ascii_lowercase();
    let explicit_port = if let Some(rest) = raw.strip_prefix('[') {
        let (_, suffix) = rest.split_once(']')?;
        suffix.strip_prefix(':')
    } else {
        raw.rsplit_once(':')
            .and_then(|(host, port)| (!host.contains(':')).then_some(port))
    };
    let port = explicit_port
        .map(str::parse)
        .transpose()
        .ok()
        .flatten()
        .or(url.port());
    if port == Some(0) {
        return None;
    }
    Some((host, port))
}

fn host_allowed(mode: &AccessMode, domain: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(raw_host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some((host, port)) = parse_host_authority(raw_host) else {
        return false;
    };
    match mode {
        AccessMode::Proxy => domain.is_some_and(|domain| {
            host.eq_ignore_ascii_case(domain) && (port.is_none() || port == Some(443))
        }),
        AccessMode::Local => {
            (host == "127.0.0.1" || host == "localhost" || host == "::1")
                && port.is_none_or(|value| value > 0)
        }
        AccessMode::Lan => {
            (host == "localhost" || host.parse::<IpAddr>().is_ok())
                && port.is_none_or(|value| value > 0)
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
    // 恒定时间比较（与配对码的 ct_eq 一致）：长度先归一再逐字节比较，
    // 避免短路比较理论上泄露前缀匹配长度。
    use subtle::ConstantTimeEq;
    if csrf.len() != session.csrf_token.len()
        || !bool::from(csrf.as_bytes().ct_eq(session.csrf_token.as_bytes()))
    {
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

fn add_security_headers(response: &mut Response, path: &str) {
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
        // connect-src 仅允许同源（Socket.IO/WebSocket 均同源），不开放裸 wss:，
        // 防止页面连接到任意主机。
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    if is_static_asset(path) {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=300"),
        );
    }
}

fn is_static_asset(path: &str) -> bool {
    matches!(
        path,
        "/favicon.ico"
            | "/pair.css"
            | "/pair.js"
            | "/pair-font.woff2"
            | "/index.html"
            | "/settings.html"
    ) || path.starts_with("/css/")
        || path.starts_with("/js/")
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
            Ok(listener) => {
                if offset > 0 {
                    info!("端口 {start_port} 不可用，已回退到 {port}");
                }
                return Ok(listener);
            }
            // 不仅 catch AddrInUse：Windows Hyper-V 保留端口返回 PermissionDenied (10013)，
            // 同样需要回退到下一个端口。
            Err(error) => {
                warn!("绑定 {host}:{port} 失败: {error}，尝试下一个端口");
                continue;
            }
        }
    }
    Err(anyhow::anyhow!(
        "端口 {start_port} 起连续 100 个端口均不可用"
    ))
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
                if offset > 0 {
                    info!("端口 {start_port} 不可用，已回退到 {port}");
                }
                return Ok(tokio::net::TcpListener::from_std(listener)?);
            }
            // 同 bind_with_port_fallback：PermissionDenied (Windows 10013) 也回退。
            Err(error) => {
                warn!("绑定 [::]:{port} 失败: {error}，尝试下一个端口");
                continue;
            }
        }
    }
    Err(anyhow::anyhow!(
        "端口 {start_port} 起连续 100 个双栈端口均不可用"
    ))
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

    #[test]
    fn proxy_requires_tls_verification() {
        assert!(validate_tls_policy(&AccessMode::Local, false).is_ok());
        assert!(validate_tls_policy(&AccessMode::Lan, false).is_ok());
        assert!(validate_tls_policy(&AccessMode::Proxy, true).is_ok());
        assert!(validate_tls_policy(&AccessMode::Proxy, false).is_err());
    }

    fn host_header(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn host_authority_matching_rejects_lookalike_hosts_and_bad_ports() {
        assert!(host_allowed(
            &AccessMode::Local,
            None,
            &host_header("127.0.0.1:8080")
        ));
        assert!(host_allowed(
            &AccessMode::Local,
            None,
            &host_header("localhost")
        ));
        assert!(host_allowed(
            &AccessMode::Lan,
            None,
            &host_header("[::1]:5000")
        ));
        assert!(!host_allowed(
            &AccessMode::Local,
            None,
            &host_header("127.0.0.1.evil:8080")
        ));
        assert!(!host_allowed(
            &AccessMode::Local,
            None,
            &host_header("127.0.0.1:not-a-port")
        ));
    }

    #[test]
    fn proxy_host_accepts_only_configured_domain_and_https_port() {
        assert!(host_allowed(
            &AccessMode::Proxy,
            Some("example.com"),
            &host_header("example.com")
        ));
        assert!(host_allowed(
            &AccessMode::Proxy,
            Some("example.com"),
            &host_header("example.com:443")
        ));
        assert!(!host_allowed(
            &AccessMode::Proxy,
            Some("example.com"),
            &host_header("example.com:80")
        ));
        assert!(!host_allowed(
            &AccessMode::Lan,
            None,
            &host_header("evil.example:5000")
        ));
    }
}
