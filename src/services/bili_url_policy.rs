use crate::error::{AppError, AppResult};
use std::net::IpAddr;
use url::{Host, Url};

const ALLOWED_ROOTS: &[&str] = &["bilibili.com", "bilivideo.com", "hdslb.com"];

/// 在服务端边界规范化 B 站资源 URL。
/// B 站 API 可能返回协议相对地址或旧版 HTTP 图片地址，统一升级为 HTTPS 后
/// 再执行常规白名单校验。
pub fn normalize_syntax(raw: &str) -> AppResult<Url> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AppError::BadRequest("下载 URL 不能为空".to_string()));
    }
    let normalized = if raw.starts_with("//") {
        format!("https:{raw}")
    } else {
        raw.to_string()
    };
    let mut parsed = Url::parse(&normalized)
        .map_err(|_| AppError::BadRequest("下载 URL 格式无效".to_string()))?;
    if parsed.scheme() == "http" {
        parsed
            .set_scheme("https")
            .map_err(|_| AppError::BadRequest("下载 URL 协议无效".to_string()))?;
    }
    validate_syntax(parsed.as_str())
}

pub fn validate_syntax(raw: &str) -> AppResult<Url> {
    let parsed =
        Url::parse(raw).map_err(|_| AppError::BadRequest("下载 URL 格式无效".to_string()))?;
    if parsed.scheme() != "https" {
        return Err(AppError::BadRequest("下载 URL 必须使用 HTTPS".to_string()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::BadRequest(
            "下载 URL 不允许携带用户信息".to_string(),
        ));
    }
    if parsed.port().is_some_and(|port| port != 443) {
        return Err(AppError::BadRequest(
            "下载 URL 不允许使用非标准端口".to_string(),
        ));
    }
    let Host::Domain(host) = parsed
        .host()
        .ok_or_else(|| AppError::BadRequest("下载 URL 缺少域名".to_string()))?
    else {
        return Err(AppError::BadRequest(
            "下载 URL 不允许使用 IP 地址".to_string(),
        ));
    };
    let host = host.to_ascii_lowercase();
    if !ALLOWED_ROOTS
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
    {
        return Err(AppError::BadRequest("不支持的 B 站资源域名".to_string()));
    }
    Ok(parsed)
}

pub fn validate_live_endpoint_syntax(raw: &str, websocket: bool) -> AppResult<Url> {
    let parsed = Url::parse(raw.trim())
        .map_err(|_| AppError::BadRequest("直播端点 URL 格式无效".to_string()))?;
    let expected = if websocket { "wss" } else { "https" };
    if parsed.scheme() != expected {
        return Err(AppError::BadRequest(format!("直播端点必须使用 {expected}")));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::BadRequest(
            "直播端点不允许携带用户信息".to_string(),
        ));
    }
    // ponytail：直播端点不限制端口；B 站弹幕 WSS 使用 2244/2245，而不是 443。
    // 上方的域名白名单才是实际安全边界。
    let Host::Domain(host) = parsed
        .host()
        .ok_or_else(|| AppError::BadRequest("直播端点缺少域名".to_string()))?
    else {
        return Err(AppError::BadRequest(
            "直播端点不允许使用 IP 地址".to_string(),
        ));
    };
    let host = host.to_ascii_lowercase();
    if !ALLOWED_ROOTS
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
    {
        return Err(AppError::BadRequest(
            "不支持的 B 站直播端点域名".to_string(),
        ));
    }
    Ok(parsed)
}

pub async fn validate(raw: &str) -> AppResult<Url> {
    let parsed = normalize_syntax(raw)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::BadRequest("下载 URL 缺少域名".to_string()))?;
    let addresses = tokio::net::lookup_host((host, 443))
        .await
        .map_err(|_| AppError::BadRequest("下载域名无法解析".to_string()))?;
    let mut found = false;
    for address in addresses {
        found = true;
        if !is_public_ip(address.ip()) {
            return Err(AppError::BadRequest(
                "下载域名解析到不允许的网络地址".to_string(),
            ));
        }
    }
    if !found {
        return Err(AppError::BadRequest("下载域名没有可用地址".to_string()));
    }
    Ok(parsed)
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            let cgnat = a == 100 && (64..=127).contains(&b); // 100.64.0.0/10 运营商级 NAT
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || a == 0
                || cgnat)
        }
        IpAddr::V6(ip) => {
            // IPv4-mapped（::ffff:a.b.c.d）归一后按 IPv4 规则判定，
            // 否则 ::ffff:10.0.0.1 会绕过私网检查。
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            let octets = ip.octets();
            let unique_local = octets[0] & 0xfe == 0xfc;
            let link_local = octets[0] == 0xfe && octets[1] & 0xc0 == 0x80;
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || unique_local
                || link_local)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_https_bilibili_domains() {
        assert!(validate_syntax("https://upos-sz-mirrorcos.bilivideo.com/a.m4s").is_ok());
        assert!(validate_syntax("http://upos-sz-mirrorcos.bilivideo.com/a.m4s").is_err());
        assert!(validate_syntax("https://bilivideo.com.evil.example/a").is_err());
        assert!(validate_syntax("https://127.0.0.1/a").is_err());
        assert!(validate_syntax("file:///etc/passwd").is_err());
    }

    #[test]
    fn normalizes_legacy_bilibili_resource_urls() {
        assert_eq!(
            normalize_syntax("//i0.hdslb.com/a.jpg")
                .expect("protocol relative")
                .as_str(),
            "https://i0.hdslb.com/a.jpg"
        );
        assert_eq!(
            normalize_syntax("http://i0.hdslb.com/a.jpg")
                .expect("legacy http")
                .as_str(),
            "https://i0.hdslb.com/a.jpg"
        );
        assert!(normalize_syntax("http://example.com/a.jpg").is_err());
        assert!(normalize_syntax("/relative/a.jpg").is_err());
    }

    #[test]
    fn validates_live_endpoint_scheme_and_authority() {
        assert!(validate_live_endpoint_syntax("https://cdn.bilivideo.com/live", false).is_ok());
        assert!(validate_live_endpoint_syntax("wss://broadcast.bilibili.com/sub", true).is_ok());
        assert!(validate_live_endpoint_syntax("http://cdn.bilivideo.com/live", false).is_err());
        // ponytail：直播端点允许使用非标准端口（弹幕 WSS 使用 2245）。
        assert!(validate_live_endpoint_syntax("wss://cdn.bilivideo.com:8443/sub", true).is_ok());
        assert!(validate_live_endpoint_syntax("wss://cdn.bilivideo.com:2245/sub", true).is_ok());
        assert!(
            validate_live_endpoint_syntax("https://user@cdn.bilivideo.com/live", false).is_err()
        );
        assert!(validate_live_endpoint_syntax("https://127.0.0.1/live", false).is_err());
    }
}
