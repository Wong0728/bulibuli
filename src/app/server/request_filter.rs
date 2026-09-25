//! 请求安全中间件：来源 IP 判定、访问策略、Host/Origin 校验、会话认证与 CSRF。
//!
//! 这是所有进入主服务的请求的第一道闸门；RBAC 细粒度授权见 [`super::rbac`]。

use crate::api::auth::session_cookie;
use crate::services::auth::{ClientInfo, SessionAuth};
use crate::services::security_config::AccessMode;
use crate::state::bili::BiliState;
use crate::state::SharedState;
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::SocketAddr;

use super::net_headers::{add_security_headers, api_error, effective_client_ip, host_allowed};
use super::rbac::{authorize_session, bypass_session};

pub(super) async fn enforce_request_security(
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
    let (client_ip, ip_is_client) =
        match effective_client_ip(&config.mode, peer.ip(), request.headers()) {
            Ok(value) => value,
            Err(response) => return response,
        };
    if !ip_is_client && config.ip_policy_configured() {
        // 按 IP 的限制性规则需要真实客户端地址才能求值。这里不静默按反代的回环地址
        // 放行（那会让 access_rules / geo_cn 悄悄失效），也不对未配置任何限制的默认
        // 反代部署报错——那种情况本就没有"被忽略的意图"。
        tracing::warn!(
            %client_ip,
            "Proxy 模式已配置按 IP 的访问规则或地理限制，但未信任 x-forwarded-for，无法求值，请求已拒绝"
        );
        return api_error(
            StatusCode::FORBIDDEN,
            403,
            "已配置按 IP 的访问规则/地理限制，但 Proxy 模式拿不到真实客户端地址：请设置 BILI__TRUST_PROXY_HEADERS=true，并保证反向代理覆盖（而非透传）x-forwarded-for",
        );
    }
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
        ip_is_client,
    });

    let path = request.uri().path().to_string();
    let public = is_public_path(&path);
    let bypassed_auth = config.should_bypass_auth(client_ip, ip_is_client);
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
            let token = session_cookie(request.headers(), config.mode == AccessMode::Proxy)
                .unwrap_or_default();
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
        if let Err(error) = crate::api::auth::set_session_cookie(&state.bili, &mut response, &token)
        {
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

pub(super) fn is_mutating(method: &Method) -> bool {
    matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 历史路径既不是公开路径，也不允许作为静态资源直接命中。
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
                !super::super::net_headers::is_static_asset(path),
                "{path} must not be served as an asset"
            );
        }
        assert!(is_public_path("/app/assets/index.js"));
        assert!(super::super::net_headers::is_static_asset(
            "/app/assets/index.js"
        ));
    }
}
