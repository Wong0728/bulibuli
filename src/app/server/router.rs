//! 路由组装：静态资源与 API 挂载、Socket.IO 层、请求日志中间件与有界限流器。

use crate::api;
use crate::services::auth::ClientInfo;
use crate::state::SharedState;
use crate::ws::WebSocketManager;
use anyhow::Context;
use axum::{
    body::Body,
    extract::{MatchedPath, State},
    http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use socketioxide::SocketIo;
use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};
use tracing::warn;

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
                return super::net_headers::api_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    429,
                    "请求过于频繁",
                );
            }
            let path = request.uri().path();
            // 免认证公开端点单独限流（复用有界 key 集合设施），防止匿名打点
            // 占用连接与探测资源。
            if (path == "/api/health" || path == "/api/ready") && !health_limiter.check(client_ip) {
                return super::net_headers::api_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    429,
                    "请求过于频繁",
                );
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
                return super::net_headers::api_error(
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
    let security_layer = middleware::from_fn_with_state(
        state.clone(),
        super::request_filter::enforce_request_security,
    );
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

use axum::Extension;

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
    let token = crate::api::auth::session_cookie(
        &headers,
        state.bili.security.current().mode == crate::services::security_config::AccessMode::Proxy,
    )
    .unwrap_or_default();
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
        if let Err(error) = crate::api::auth::set_session_cookie(&state.bili, &mut response, &token)
        {
            return error.into_response();
        }
    }
    response
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
