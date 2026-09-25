use crate::error::{AppError, AppResult};
use std::net::IpAddr;
use url::{Host, Url};

/// B 站自有域白名单：**凭据作用域**，不是访问围栏。
/// Cookie 只允许随名单内主机出站（封面/代理等 B 站资源请求，以及白名单内
/// CDN 的下载直链）；名单外主机照常可下载（不带 Cookie，签名即鉴权）。
/// `bilivideo.cn`/`bilivideo.com` 同属 B 站 CDN 域族，`akamaized.net` 是
/// Akamai 镜像（upos-hz-mirrorakam），`szbdyd.com`/`mountaintoys.cn`/
/// `solseed.cn` 是 PCDN/P2P 域族——playurl 会轮换返回，名单外的新域
/// 不阻塞下载，只影响是否附带 Cookie。
const ALLOWED_ROOTS: &[&str] = &[
    "bilibili.com",
    "bilivideo.com",
    "bilivideo.cn",
    "hdslb.com",
    "akamaized.net",
    "szbdyd.com",
    "mountaintoys.cn",
    "solseed.cn",
];

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
        // 只记 host：URL query 携带签名与时效参数，不得落入日志。
        tracing::warn!("拒绝非白名单下载资源域名: {host}");
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
    ensure_public_dns(host).await?;
    Ok(parsed)
}

/// 下载直链校验：签名即鉴权，域名不做枚举。
///
/// B 站 CDN/PCDN 域族持续轮换（bilivideo 双域、akamaized、szbdyd、
/// mountaintoys、solseed……），枚举式白名单只会不断误拒正常任务。
/// 借鉴对标项目 Bili23 的结论：取流 URL 的鉴权信息全在签名 query 里，
/// Cookie 只随白名单内主机出站（见 [`host_allows_credentials`]），因此
/// 下载直链不需要域名白名单，安全边界是 DNS 私网/环网拦截。
/// mcdn 节点常带非 443 端口（如 4483），同样放行。
pub async fn validate_download_url(raw: &str) -> AppResult<Url> {
    let parsed = validate_download_syntax(raw)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::BadRequest("下载 URL 缺少域名".to_string()))?;
    ensure_public_dns(&host.to_ascii_lowercase()).await?;
    Ok(parsed)
}

/// 下载直链的同步语法校验（不含 DNS）：scheme/用户信息/域名形态。
fn validate_download_syntax(raw: &str) -> AppResult<Url> {
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
    if parsed.scheme() != "https" {
        return Err(AppError::BadRequest("下载 URL 必须使用 HTTPS".to_string()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::BadRequest(
            "下载 URL 不允许携带用户信息".to_string(),
        ));
    }
    let Host::Domain(_) = parsed
        .host()
        .ok_or_else(|| AppError::BadRequest("下载 URL 不允许使用 IP 地址".to_string()))?
    else {
        return Err(AppError::BadRequest(
            "下载 URL 不允许使用 IP 地址".to_string(),
        ));
    };
    Ok(parsed)
}

async fn ensure_public_dns(host: &str) -> AppResult<()> {
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
    Ok(())
}

/// 该 URL 的主机是否在 B 站自有域白名单内。
/// Cookie 只允许随白名单内主机出站：白名单从"准入围栏"降级为"凭据作用域"，
/// 名单外的新 PCDN 域照常下载（不带 Cookie），Cookie 永不出 B 站域。
pub fn host_allows_credentials(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url.trim()) else {
        return false;
    };
    let Some(Host::Domain(host)) = parsed.host() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    ALLOWED_ROOTS
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
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
    fn accepts_bilivideo_cn_cdn_family_for_mcdn_candidates() {
        assert!(validate_syntax("https://xy1x2xy.mcdn.bilivideo.cn/v1/a.m4s").is_ok());
        assert!(validate_syntax("https://bilivideo.cn/v1/a.m4s").is_ok());
        // Akamai 镜像域与 PCDN/P2P 域族（对标项目 Bili23 的 PCDN 黑名单同源）
        assert!(validate_syntax("https://upos-hz-mirrorakam.akamaized.net/v1/a.m4s").is_ok());
        assert!(validate_syntax("https://xy1x2xy.szbdyd.com/v1/a.m4s").is_ok());
        assert!(validate_syntax("https://xy1x2xy.mountaintoys.cn/v1/a.m4s").is_ok());
        // 实测 playurl 返回过的 PCDN 域（upos-lcdn-cqgl01.solseed.cn）
        assert!(validate_syntax("https://upos-lcdn-cqgl01.solseed.cn/v1/a.m4s").is_ok());
        // 后缀混淆与端口边界不因新增域放宽
        assert!(validate_syntax("https://bilivideo.cn.evil.example/v1/a.m4s").is_err());
        assert!(validate_syntax("https://evil-bilivideo.cn/v1/a.m4s").is_err());
        assert!(validate_syntax("https://evil.akamaized.net.evil.example/v1/a.m4s").is_err());
        assert!(validate_syntax("https://xy.mcdn.bilivideo.cn:8082/v1/a.m4s").is_err());
    }

    #[test]
    fn download_urls_skip_domain_enumeration_but_keep_syntax_fence() {
        // 下载直链不做域名单：签名 query 即鉴权，新 PCDN 域即插即用。
        assert!(validate_download_syntax("https://unknown-cdn.example.com/v1/a.m4s").is_ok());
        // mcdn 节点常带非 443 端口，下载直链放行。
        assert!(validate_download_syntax("https://xy.mcdn.bilivideo.cn:4483/v1/a.m4s").is_ok());
        // 其余语法围栏不变：协议升级、拒绝用户信息与 IP 字面量。
        assert_eq!(
            validate_download_syntax("//upos.example.com/a.m4s")
                .expect("protocol relative")
                .as_str(),
            "https://upos.example.com/a.m4s"
        );
        assert!(validate_download_syntax("https://user@upos.example.com/a.m4s").is_err());
        assert!(validate_download_syntax("https://127.0.0.1/a.m4s").is_err());
        assert!(validate_download_syntax("file:///etc/passwd").is_err());
    }

    #[test]
    fn credentials_only_leave_for_whitelisted_hosts() {
        // Cookie 作用域：名单内主机携带凭据，名单外主机（含新 PCDN 域）不带。
        assert!(host_allows_credentials(
            "https://upos-sz-mirrorcos.bilivideo.com/a.m4s"
        ));
        assert!(host_allows_credentials(
            "https://upos-lcdn-cqgl01.solseed.cn/a.m4s"
        ));
        assert!(!host_allows_credentials(
            "https://unknown-cdn.example.com/a.m4s"
        ));
        assert!(!host_allows_credentials(
            "https://bilivideo.com.evil.example/a.m4s"
        ));
        assert!(!host_allows_credentials("not a url"));
        // 后缀混淆不得借白名单带走凭据。
        assert!(!host_allows_credentials(
            "https://evil.hdslb.com.evil.example/a.m4s"
        ));
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
