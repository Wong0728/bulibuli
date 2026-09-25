//! 网络信任策略与响应加固：proxy 模式的真实客户端 IP、Host/Authority 校验、
//! 安全响应头（CSP 等）与统一 API 错误信封。

use crate::error::ApiResponse;
use crate::services::security_config::{is_effectively_loopback, AccessMode};
use axum::{
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use std::net::IpAddr;

/// 统一 API 错误信封：状态码 + 业务码 + 消息。
pub(super) fn api_error(status: StatusCode, code: i64, message: &'static str) -> Response {
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

/// proxy 模式下的真实客户端地址判定。
///
/// 返回 `(客户端地址, 地址是否来自真实客户端)`。
#[allow(clippy::result_large_err)]
pub(super) fn effective_client_ip(
    mode: &AccessMode,
    peer: IpAddr,
    headers: &HeaderMap,
) -> Result<(IpAddr, bool), Response> {
    if *mode != AccessMode::Proxy {
        return Ok((peer, true));
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
        // 此时唯一可得的地址是本机反代的回环地址，它不代表发起请求的设备。
        tracing::warn!(
            "proxy 模式未设置 BILI__TRUST_PROXY_HEADERS=true：忽略 x-forwarded-for，\
             客户端地址按 TCP 对端（本机反向代理回环地址）记录，按 IP 的能力判定不可用"
        );
        return Ok((peer, false));
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
        .map(|ip| (ip, true))
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

pub(super) fn host_allowed(mode: &AccessMode, domain: Option<&str>, headers: &HeaderMap) -> bool {
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

pub(super) fn add_security_headers(response: &mut Response, path: &str) {
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
        // index.html 是资源清单入口，必须每次回源校验：发版后旧 hash 资源会被
        // 清理/覆盖，浏览器缓存旧 index 只会加载到已被替换的旧界面。
        // 带内容 hash 的产物才允许缓存。
        let cache_control = if is_html_entry(path) {
            "no-cache"
        } else {
            "public, max-age=300"
        };
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
    }
}

fn is_html_entry(path: &str) -> bool {
    matches!(path, "/index.html" | "/app" | "/app/" | "/app/index.html")
}

pub(super) fn is_static_asset(path: &str) -> bool {
    matches!(
        path,
        "/favicon.ico"
            | "/index.html"
            // Vue 3 + Vite 产物带 hash，可长缓存。
            | "/app" | "/app/" | "/app/index.html"
    ) || path.starts_with("/app/assets/")
        || path.starts_with("/app/css/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

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
