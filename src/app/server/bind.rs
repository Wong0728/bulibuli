//! 监听器绑定与访问地址推导。
//!
//! - 按安全模式选择绑定策略：Local/Proxy 绑 IPv4 回环，LAN 绑 IPv6 双栈
//! - 端口被占用（含 Windows Hyper-V 保留端口）时按序回退
//! - `main_url` / `accessible_main_urls` 供 TUI 状态栏与 Setup 页展示

use crate::services::security_config::AccessMode;
use crate::state::SharedState;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use tracing::{info, warn};

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

pub(super) fn detect_local_ips() -> Vec<String> {
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

pub(super) fn validate_tls_policy(mode: &AccessMode, tls_verify: bool) -> anyhow::Result<()> {
    if *mode == AccessMode::Proxy && !tls_verify {
        anyhow::bail!("proxy 模式禁止关闭 TLS 证书验证");
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_requires_tls_verification() {
        assert!(validate_tls_policy(&AccessMode::Local, false).is_ok());
        assert!(validate_tls_policy(&AccessMode::Lan, false).is_ok());
        assert!(validate_tls_policy(&AccessMode::Proxy, true).is_ok());
        assert!(validate_tls_policy(&AccessMode::Proxy, false).is_err());
    }

    #[test]
    fn detect_local_ips_always_contains_loopback() {
        let ips = detect_local_ips();
        assert!(ips.contains(&"127.0.0.1".to_string()));
    }

    #[test]
    fn format_host_brackets_ipv6() {
        assert_eq!(format_host("192.168.1.2"), "192.168.1.2");
        assert_eq!(format_host("::1"), "[::1]");
        assert_eq!(format_host("[::1]"), "[::1]");
    }
}
