//! Aria2 下载器管理：类型定义在本文件，子进程生命周期见 `process`，
//! JSON-RPC 调用见 `rpc`，Windows Job Object 绑定见 `win_job`。

mod process;
mod rpc;
#[cfg(windows)]
mod win_job;

use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Child;
use tokio::sync::Mutex;

/// Build the only RPC endpoint accepted by the application.  Aria2 host is
/// stored as an authority, never as a user-controlled URL fragment.
pub(crate) fn rpc_endpoint(host: &str, port: u16) -> anyhow::Result<String> {
    let host = host.trim();
    if host.is_empty()
        || host.contains("://")
        || host.contains('/')
        || host.contains('@')
        || host.contains('?')
        || host.contains('#')
    {
        anyhow::bail!("invalid aria2 RPC host");
    }

    let (authority, loopback) = if let Some(ipv6) =
        host.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
    {
        let ip: std::net::IpAddr = ipv6
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid aria2 IPv6 host"))?;
        (format!("[{ipv6}]"), ip.is_loopback())
    } else if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if ip.is_ipv6() {
            (format!("[{host}]"), ip.is_loopback())
        } else {
            (host.to_owned(), ip.is_loopback())
        }
    } else {
        if host.contains(':') {
            anyhow::bail!("invalid aria2 host");
        }
        let parsed = url::Url::parse(&format!("http://{host}:{port}/"))
            .map_err(|_| anyhow::anyhow!("invalid aria2 host"))?;
        if parsed.host_str() != Some(host) || parsed.username() != "" || parsed.password().is_some()
        {
            anyhow::bail!("invalid aria2 host");
        }
        (
            host.to_owned(),
            matches!(
                host.to_ascii_lowercase().as_str(),
                "localhost" | "localhost."
            ),
        )
    };

    let scheme = if loopback { "http" } else { "https" };
    Ok(format!("{scheme}://{authority}:{port}/jsonrpc"))
}

const DEFAULT_ARIA2_PORT: u16 = 6800;
const MAX_RETRIES: u32 = 3;
const BASE_RETRY_DELAY_MS: u64 = 500;
/// is_available 结果的缓存有效期。
/// 一秒缓存可合并高频状态轮询，同时保持故障感知及时。
const AVAILABILITY_CACHE_TTL: Duration = Duration::from_secs(1);

/// Aria2 工作模式。
/// - `Embedded`：程序内置启动 aria2c 子进程
/// - `System`：使用系统已安装的 aria2c（程序仍负责启动子进程）
/// - `External`：连接外部已运行的 Aria2 RPC（不启动子进程）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aria2Mode {
    Embedded,
    System,
    External,
}

impl Aria2Mode {
    /// 从配置字符串解析，兼容旧值 "local" → Embedded。
    fn from_str(s: &str) -> Self {
        match s {
            "embedded" | "local" => Aria2Mode::Embedded,
            "system" => Aria2Mode::System,
            _ => Aria2Mode::External,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Aria2Mode::Embedded => "embedded",
            Aria2Mode::System => "system",
            Aria2Mode::External => "external",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Aria2Error {
    Connection(String),
    Timeout(String),
    Rpc { code: i64, message: String },
    Unexpected(String),
}

impl std::fmt::Display for Aria2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Aria2Error::Connection(msg) => write!(f, "Aria2 连接失败: {msg}"),
            Aria2Error::Timeout(msg) => write!(f, "Aria2 请求超时: {msg}"),
            Aria2Error::Rpc { code, message } => write!(f, "Aria2 RPC 错误 [{code}]: {message}"),
            Aria2Error::Unexpected(msg) => write!(f, "Aria2 未知错误: {msg}"),
        }
    }
}

impl std::error::Error for Aria2Error {}

#[derive(Clone)]
pub struct Aria2Manager {
    client: Client,
    paths: Arc<crate::config::AppPaths>,
    inner: Arc<Mutex<Aria2Inner>>,
}

struct Aria2Inner {
    port: u16,
    secret: String,
    mode: Aria2Mode,
    child: Option<Child>,
    rpc_url: String,
    /// is_available 结果缓存：(可用性, 写入时间)。
    /// TTL 由 AVAILABILITY_CACHE_TTL 控制，过期后下次调用重新发起 RPC。
    available_cache: Option<(bool, Instant)>,
    /// 当前启动/连接尝试开始时间；用于限制 `starting` 状态的最长持续时间。
    started_at: Option<Instant>,
    /// 本轮进程/RPC 是否曾经成功就绪。就绪后再次失联不应回退成 `starting`。
    ready: bool,
    /// 最近一次可诊断错误，不包含密钥等敏感配置。
    last_error: Option<String>,
    /// Windows：kill-on-close 的 Job Object，用于父进程退出（含被强杀）时连带结束 aria2c 子进程。
    #[cfg(windows)]
    job: Option<win_job::Job>,
}

#[derive(Debug, Clone, Default)]
pub struct Aria2Status {
    pub status: String,
    pub progress_percent: i32,
    pub downloaded_size: i64,
    pub total_size: i64,
    pub speed: i64,
    pub error_message: Option<String>,
    pub filename: String,
}

impl Aria2Status {
    fn from_json(v: &Value) -> Self {
        let status = v["status"].as_str().unwrap_or("error").to_string();
        let total = Self::parse_size(&v["totalLength"]);
        let completed = Self::parse_size(&v["completedLength"]);
        let download_speed = Self::parse_size(&v["downloadSpeed"]);
        let percent = if total > 0 {
            (completed * 100 / total) as i32
        } else {
            0
        };
        let error = v["errorMessage"].as_str().map(|s| s.to_string());
        let filename = v["files"]
            .get(0)
            .and_then(|f| f["path"].as_str())
            .and_then(|p| std::path::Path::new(p).file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string());

        Self {
            status,
            progress_percent: percent,
            downloaded_size: completed,
            total_size: total,
            speed: download_speed,
            error_message: error,
            filename,
        }
    }

    fn parse_size(v: &Value) -> i64 {
        v.as_str().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::rpc_endpoint;

    #[test]
    fn rpc_endpoint_uses_https_for_remote_hosts() {
        assert_eq!(
            rpc_endpoint("aria.example.com", 6800).unwrap(),
            "https://aria.example.com:6800/jsonrpc"
        );
        assert_eq!(
            rpc_endpoint("127.0.0.1", 6800).unwrap(),
            "http://127.0.0.1:6800/jsonrpc"
        );
        assert!(rpc_endpoint("http://aria.example.com/path", 6800).is_err());
        assert_eq!(
            rpc_endpoint("2001:db8::1", 6800).unwrap(),
            "https://[2001:db8::1]:6800/jsonrpc"
        );
    }
}
