use crate::error::{AppError, AppResult};
use std::net::IpAddr;
use url::{Host, Url};

const ALLOWED_ROOTS: &[&str] = &["bilibili.com", "bilivideo.com", "hdslb.com"];

/// Normalize Bilibili resource URLs at the server boundary. Bilibili APIs may
/// return protocol-relative or legacy HTTP image URLs; both are upgraded to
/// HTTPS before the regular allow-list validation runs.
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
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.octets()[0] == 0)
        }
        IpAddr::V6(ip) => {
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
}
