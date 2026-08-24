use crate::api;
use crate::api::auth::{session_cookie, set_session_cookie};
use crate::error::ApiResponse;
use crate::services::auth::{ClientInfo, SessionAuth, SessionRole};
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
    let candidates =
        accessible_main_urls_for_mode(&security.mode, security.proxy_domain.as_deref(), port);
    if security.mode == AccessMode::Lan {
        candidates
            .iter()
            .find(|url| !url.contains("127.0.0.1") && !url.contains("[::1]"))
            .cloned()
            .or_else(|| candidates.into_iter().next())
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}"))
    } else {
        candidates
            .into_iter()
            .next()
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}"))
    }
}

/// 返回当前实际监听端口对应的用户可访问地址。
///
/// 端口尚未绑定时返回空集合，调用方不能把配置端口当成已就绪端口展示。
pub fn accessible_main_urls(state: &SharedState) -> Vec<String> {
    let port = state.infra.actual_main_port.load(Ordering::Relaxed);
    if port == 0 {
        return Vec::new();
    }
    let security = state.bili.security.current();
    accessible_main_urls_for_mode(&security.mode, security.proxy_domain.as_deref(), port)
}

fn accessible_main_urls_for_mode(
    mode: &AccessMode,
    proxy_domain: Option<&str>,
    port: u16,
) -> Vec<String> {
    match mode {
        AccessMode::Proxy => proxy_domain
            .map(|domain| format!("https://{domain}"))
            .or_else(|| Some(format!("http://127.0.0.1:{port}")))
            .into_iter()
            .collect(),
        AccessMode::Local => vec![format!("http://127.0.0.1:{port}")],
        AccessMode::Lan => detect_local_ips()
            .into_iter()
            .map(|ip| format!("http://{}:{port}", format_host(&ip)))
            .collect(),
    }
}

pub(crate) fn detect_local_ips() -> Vec<String> {
    let mut ips = vec!["127.0.0.1".to_string()];
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
        }
    }
    if let Ok(socket) = std::net::UdpSocket::bind("[::]:0") {
        if socket.connect("[2001:4860:4860::8888]:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = format_host(&addr.ip().to_string());
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
        }
    }
    ips
}

fn format_host(ip: &str) -> String {
    let value = ip.trim_matches(['[', ']']);
    if value.contains(':') {
        format!("[{value}]")
    } else {
        value.to_string()
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

/// governor keyed 限流器的 key 集合容量上限：governor 默认不回收 key，
/// LAN/Proxy 模式下轮换 IPv6 源地址（或对公开路径高频打点）会让内部状态
/// 无界增长。与 services/auth.rs 的 MAX_TRACKED_IPS 同值同模式。
const MAX_RATE_LIMIT_KEYS: usize = 10_000;
/// governor 状态清理的最小间隔：攻击者轮换源地址时避免每次请求都做
/// O(n) 的 retain_recent 扫描（该扫描本身就是一种 CPU 放大面）。
const RATE_LIMIT_CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// 带 key 集合上限的 governor 限流器（S1）。
///
/// governor 的 keyed 状态（内部 DashMap）没有按 key 删除的 API，key 一旦
/// 创建只能整体等待回收。这里复用 `services/auth.rs::evict_stalest_ip` 的
/// 有界近似 LRU 模式：自己维护一份有界的"key 最近活动时间"表，总量超上限
/// 时淘汰最久未活动的 key；同时按节流频率调用 `retain_recent` +
/// `shrink_to_fit`，让 governor 丢弃已离开限流窗口（空闲数秒）的 key 桶并
/// 归还内存。被限流拒绝的请求同样计入 key 集合（公开路径被匿名刷也会占用
/// 桶），因此有界化必须在 check 路径而非放行路径上做。
struct KeyedRateLimiter {
    limiter: governor::RateLimiter<
        IpAddr,
        governor::state::keyed::DefaultKeyedStateStore<IpAddr>,
        governor::clock::DefaultClock,
    >,
    /// key 最近活动时间（近似 LRU 的淘汰依据）。
    last_seen: std::sync::Mutex<std::collections::HashMap<IpAddr, std::time::Instant>>,
    /// 上次 governor 状态清理时间（节流用）。
    last_cleanup: std::sync::Mutex<std::time::Instant>,
    /// 累计检查次数：周期性触发清理，避免低流量时 key 长期滞留。
    checks: std::sync::atomic::AtomicU64,
}

impl KeyedRateLimiter {
    fn new(quota: governor::Quota) -> Self {
        Self {
            limiter: governor::RateLimiter::keyed(quota),
            last_seen: Default::default(),
            last_cleanup: std::sync::Mutex::new(std::time::Instant::now()),
            checks: Default::default(),
        }
    }

    /// 检查一次限额并记录 key 活动；返回是否放行。
    fn check(&self, key: IpAddr) -> bool {
        let allowed = self.limiter.check_key(&key).is_ok();
        self.track(key);
        allowed
    }

    /// 记录 key 活动并按需淘汰/清理：有界集合超上限时淘汰最久未活动的 key
    /// （仅淘汰自记账表；governor 内部桶由 retain_recent 兜底回收）。
    fn track(&self, key: IpAddr) {
        let overflowed = {
            let mut last_seen = self
                .last_seen
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !last_seen.contains_key(&key) {
                evict_stalest_key(&mut last_seen, MAX_RATE_LIMIT_KEYS);
            }
            last_seen.insert(key, std::time::Instant::now());
            last_seen.len() >= MAX_RATE_LIMIT_KEYS
        };
        // 每 4096 次检查或 key 集合溢出时尝试清理 governor 内部状态。
        let due = overflowed
            || self
                .checks
                .fetch_add(1, Ordering::Relaxed)
                .is_multiple_of(4096);
        if !due {
            return;
        }
        let mut last_cleanup = self
            .last_cleanup
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if last_cleanup.elapsed() < RATE_LIMIT_CLEANUP_INTERVAL {
            return;
        }
        *last_cleanup = std::time::Instant::now();
        drop(last_cleanup);
        // retain_recent 只保留仍处于限流窗口内的 key（本项目的配额下为
        // 空闲 2-3 秒内），一次性攻击地址随后即被整体丢弃。
        self.limiter.retain_recent();
        self.limiter.shrink_to_fit();
    }
}

/// 总量达到上限时淘汰最久未活动的 key（近似 LRU；仅在超限时执行 O(n)
/// 扫描，与 services/auth.rs::evict_stalest_ip 同模式同取舍）。
fn evict_stalest_key(
    last_seen: &mut std::collections::HashMap<IpAddr, std::time::Instant>,
    cap: usize,
) {
    while last_seen.len() >= cap {
        let Some(stalest) = last_seen
            .iter()
            .min_by_key(|(ip, at)| (*at, *ip))
            .map(|(ip, _)| *ip)
        else {
            return;
        };
        last_seen.remove(&stalest);
    }
}

pub(crate) async fn build_router(state: SharedState) -> anyhow::Result<Router> {
    let (socket_layer, io) = SocketIo::new_layer();
    WebSocketManager::setup_handlers(&io);
    state.infra.ws.attach(io).await;

    let limiter = Arc::new(KeyedRateLimiter::new(
        // 正常页面切换会并发加载多个只读接口；认证/配对本身另有登录尝试限流。
        governor::Quota::per_second(NonZeroU32::new(20).expect("non-zero quota"))
            .allow_burst(NonZeroU32::new(60).expect("non-zero burst")),
    ));
    let expensive_limiter = Arc::new(KeyedRateLimiter::new(
        // 搜索、房间详情和视频详情会放大 B 站上游请求，单独限制每个客户端。
        governor::Quota::per_second(NonZeroU32::new(5).expect("non-zero quota"))
            .allow_burst(NonZeroU32::new(10).expect("non-zero burst")),
    ));
    let health_limiter = Arc::new(KeyedRateLimiter::new(
        // /api/health 是免认证公开端点，匿名可刷；限到每 IP 2/s（前端心跳远低于此）。
        governor::Quota::per_second(NonZeroU32::new(2).expect("non-zero quota"))
            .allow_burst(NonZeroU32::new(10).expect("non-zero burst")),
    ));
    let rate_limit = middleware::from_fn(move |request: Request<Body>, next: Next| {
        let limiter = limiter.clone();
        let expensive_limiter = expensive_limiter.clone();
        let health_limiter = health_limiter.clone();
        async move {
            // GET 包含健康检查、配对状态和页面轮询，不能让旧标签页的只读请求
            // 抢占写请求限额；配对尝试本身另由 AuthService 做每 IP/全局限制。
            let client_ip = request
                .extensions()
                .get::<ClientInfo>()
                .map(|client| client.ip)
                .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            if !matches!(
                *request.method(),
                Method::GET | Method::HEAD | Method::OPTIONS
            ) && !limiter.check(client_ip)
            {
                return api_error(StatusCode::TOO_MANY_REQUESTS, 429, "请求过于频繁");
            }
            let path = request.uri().path();
            // 免认证公开端点单独限流（复用有界 key 集合设施），防止匿名打点
            // 占用连接与探测资源。
            if (path == "/api/health" || path == "/api/ready") && !health_limiter.check(client_ip) {
                return api_error(StatusCode::TOO_MANY_REQUESTS, 429, "请求过于频繁");
            }
            let expensive_read = matches!(
                path,
                "/api/blogger/search"
                    | "/api/blogger/series"
                    | "/api/blogger/series-videos"
                    | "/api/live/room-info"
                    | "/api/video/info"
            );
            if expensive_read && !expensive_limiter.check(client_ip) {
                return api_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    429,
                    "查询过于频繁，请稍后再试",
                );
            }
            next.run(request).await
        }
    });

    let static_root = state.infra.paths.static_dir();
    let static_pages = Arc::new(StaticPages {
        // 新主界面是 Vue 3 + Vite 产物（static/app/index.html）。
        index: tokio::fs::read(static_root.join("app").join("index.html"))
            .await
            .context("读取 static/app/index.html 失败，请先跑 web/ 的 vite build")?,
    });
    let api = api::router().layer(rate_limit);
    let security_layer = middleware::from_fn_with_state(state.clone(), enforce_request_security);
    Ok(Router::new()
        .route("/", get(index))
        .route_service(
            "/favicon.ico",
            ServeFile::new(static_root.join("bulibuli.ico")),
        )
        .route("/index.html", get(index))
        // 历史书签统一回到单一 Vue 入口。
        .route("/settings.html", get(redirect_to_main))
        // Vue3 + Vite 重写的新主界面，产物落在 static/app/ 下，
        // 由 Vite 自动写入带 hash 的资源文件名，可走长缓存。
        .nest_service("/app", ServeDir::new(static_root.join("app")))
        .merge(api)
        .layer(middleware::from_fn(trace_request))
        .layer(socket_layer)
        .layer(Extension(static_pages))
        .layer(security_layer)
        .with_state(state))
}

struct StaticPages {
    index: Vec<u8>,
}

/// `/settings.html` 直链访问重定向回主界面。
async fn redirect_to_main() -> axum::response::Redirect {
    axum::response::Redirect::permanent("/")
}

async fn trace_request(request: Request<Body>, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    let response = next.run(request).await;
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        warn!(%method, %route, status = status.as_u16(), "http request");
    } else {
        tracing::debug!(%method, %route, status = status.as_u16(), "http request");
    }
    response
}

async fn index(
    State(state): State<SharedState>,
    Extension(pages): Extension<Arc<StaticPages>>,
    Extension(client): Extension<ClientInfo>,
    headers: HeaderMap,
) -> Response {
    let token = session_cookie(&headers).unwrap_or_default();
    let session = match state.bili.auth.authenticate(&token, client.ip).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let bytes = pages.index.clone();
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
    let public = is_public_path(&path);
    let bypassed_auth = config.should_bypass_auth(client_ip);
    let mut session = None;
    if !public {
        if bypassed_auth {
            // auth_bypass_ips 白名单命中：跳过凭证校验，但仍以 Owner 身份走
            // authorize_session 的 RBAC 流程，避免白名单把角色授权一并绕过。
            tracing::info!(%client_ip, "请求来源 IP 在认证跳过白名单中，以 Owner 身份免凭证放行");
            let value = bypass_session();
            if let Err(response) = authorize_session(&request, &value) {
                return *response;
            }
            request.extensions_mut().insert(Some(value.clone()));
            session = Some(value);
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
    // 总是注入 Option<SessionAuth>：公开路径无会话时为 None，
    // 依赖会话上下文的 handler（logout/邀请）以 Extension<Option<SessionAuth>> 提取，
    // 避免 axum MissingExtension 直接 500。
    request.extensions_mut().insert(session.clone());
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

fn is_public_path(path: &str) -> bool {
    matches!(
        path,
        "/" | "/favicon.ico"
            | "/settings.html"
            // Vue3 主界面：静态资源全部免认证，仅 /api/* 仍走会话校验。
            | "/app" | "/app/"
            | "/app/index.html"
            | "/api/health"
            | "/api/ready"
            | "/api/auth/state"
            | "/api/auth/pair"
    ) || path.starts_with("/app/assets/")
        || path.starts_with("/app/css/")
}

/// auth_bypass_ips 命中时注入的合成会话：跳过凭证校验但保留 Owner 角色的
/// RBAC 边界（owner-only 接口放行、Viewer/Operator 限制不适用——白名单本意
/// 就是完全信任该来源）。无 csrf_token：写请求走 sec-fetch-site 同源校验分支。
fn bypass_session() -> SessionAuth {
    SessionAuth {
        id: "auth-bypass".to_string(),
        csrf_token: String::new(),
        rotated_token: None,
        role: SessionRole::Owner,
    }
}

/// 主 Web 界面的服务端 RBAC 边界。隐藏 UI 仅改善使用体验；
/// 此检查阻止已配对的 Operator/Viewer 会话通过直接调用 API 越权。
fn authorize_session(request: &Request<Body>, session: &SessionAuth) -> Result<(), Box<Response>> {
    let path = request.uri().path();
    let mutating = is_mutating(request.method());

    // 账号凭证、设置写入/敏感运维接口和设备邀请仅限 Owner 操作。
    // 业务设置的精确 GET 允许 Operator/Viewer 只读查看真实值。
    // /api/setup/* 能改写访问模式与默认策略（提权面），且暴露本机网络拓扑，
    // 全部端点仅限 Owner；未完成 onboarding 的新用户走仅回环的独立 setup 端口。
    let owner_only = path.starts_with("/api/cookies/")
        || path.starts_with("/api/settings/")
        || (path == "/api/settings" && mutating)
        || path.starts_with("/api/auth/invitations")
        || path.starts_with("/api/update/check")
        || path.starts_with("/api/update/apply")
        || path.starts_with("/api/setup/")
        || path.starts_with("/api/backup");
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
    // 默认不信任 XFF：反代若透传（而非覆盖）该头，客户端可伪造来源 IP，
    // 命中 auth_bypass_ips 白名单即免凭证获得 Owner。反代部署必须显式设置
    // BILI__TRUST_PROXY_HEADERS=true（并保证反代覆盖而非透传该头）才启用。
    let trust_proxy = std::env::var("BILI__TRUST_PROXY_HEADERS")
        .is_ok_and(|value| matches!(value.trim(), "true" | "1" | "on" | "yes"));
    if !trust_proxy {
        tracing::debug!(
            "proxy 模式未设置 BILI__TRUST_PROXY_HEADERS=true，忽略 x-forwarded-for，使用 TCP 对端地址"
        );
        return Ok(peer);
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
            | "/index.html"
            // Vue 3 + Vite 产物带 hash，可长缓存。
            | "/app" | "/app/" | "/app/index.html"
    ) || path.starts_with("/app/assets/")
        || path.starts_with("/app/css/")
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
        assert!(authorize_session(&settings, &session(SessionRole::Viewer)).is_ok());
        assert!(authorize_session(&settings, &session(SessionRole::Operator)).is_ok());
        let settings_write = Request::builder()
            .method(Method::PUT)
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        assert!(authorize_session(&settings_write, &session(SessionRole::Viewer)).is_err());
        assert!(authorize_session(&settings_write, &session(SessionRole::Operator)).is_err());
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
    fn auth_bypass_injects_owner_identity_through_rbac() {
        // 白名单命中：合成 Owner 会话必须通过 owner-only 路径的授权检查。
        let setup = Request::builder()
            .method(Method::POST)
            .uri("/api/setup/apply")
            .body(Body::empty())
            .unwrap();
        let credentials = Request::builder()
            .method(Method::POST)
            .uri("/api/cookies/save")
            .body(Body::empty())
            .unwrap();
        let bypass = bypass_session();
        assert_eq!(bypass.role, SessionRole::Owner);
        assert!(authorize_session(&setup, &bypass).is_ok());
        assert!(authorize_session(&credentials, &bypass).is_ok());
        // 对照：同样的请求换成 Operator 会话必须被拒——bypass 不得降低 RBAC 强度，
        // 也不能让非 Owner 角色借白名单越权。
        let operator = session(SessionRole::Operator);
        assert!(authorize_session(&setup, &operator).is_err());
        // Viewer 的只读限制同样照常生效。
        let viewer_read = Request::builder()
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        assert!(authorize_session(&viewer_read, &bypass).is_ok());
    }

    #[test]
    fn legacy_paths_are_neither_public_nor_static_assets() {
        for path in [
            "/legacy",
            "/legacy/",
            "/legacy/index.html",
            "/legacy/js/app.js",
        ] {
            assert!(!is_public_path(path), "{path} must require authentication");
            assert!(
                !is_static_asset(path),
                "{path} must not be served as an asset"
            );
        }
        assert!(is_public_path("/app/assets/index.js"));
        assert!(is_static_asset("/app/assets/index.js"));
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

#[cfg(test)]
mod rate_limiter_tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    /// 淘汰必须钳制集合容量并优先丢掉最久未活动的 key（近似 LRU）。
    #[test]
    fn evict_stalest_key_bounds_map_and_drops_oldest() {
        let ip = |last: u8| IpAddr::V4(Ipv4Addr::new(192, 0, 2, last));
        let mut last_seen = HashMap::new();
        let base = Instant::now();
        for index in 0..3u8 {
            last_seen.insert(ip(index), base - Duration::from_secs(100 - index as u64));
        }
        // ip(0) 最久未活动，应被优先淘汰。
        evict_stalest_key(&mut last_seen, 3);
        assert_eq!(last_seen.len(), 2);
        assert!(!last_seen.contains_key(&ip(0)));
        assert!(last_seen.contains_key(&ip(2)));
    }

    /// 限流器在超容量写入 distinct key 时自记账集合不得无界增长
    /// （governor 原生 keyed 状态没有此约束，这是 S1 的修复点）。
    /// O(n) 淘汰扫描仅在到达上限后触发（10k 之后每次插入一次），
    /// 全量 10k+50 个 key 的写入保持测试轻量。
    #[test]
    fn keyed_limiter_tracks_bounded_key_set() {
        let limiter = KeyedRateLimiter::new(
            governor::Quota::per_second(NonZeroU32::new(1_000_000).expect("quota"))
                .allow_burst(NonZeroU32::new(1_000_000).expect("burst")),
        );
        for index in 0..(MAX_RATE_LIMIT_KEYS + 50) {
            let key = IpAddr::V4(Ipv4Addr::new(
                198,
                51,
                (index / 256 % 256) as u8,
                (index % 256) as u8,
            ));
            limiter.check(key);
        }
        let len = limiter
            .last_seen
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len();
        assert!(
            len <= MAX_RATE_LIMIT_KEYS,
            "key 集合在容量 {MAX_RATE_LIMIT_KEYS} 之外增长: len={len}"
        );
    }
}
