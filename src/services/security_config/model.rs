//! 安全配置模型：访问模式（Local/Lan/Proxy）、访问规则与 `SecurityConfig` 结构体，
//! 以及客户端放行判定（最长前缀优先）、认证白名单与回环地址识别。

use crate::error::{AppError, AppResult};
use chrono::Utc;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    #[default]
    Local,
    Lan,
    Proxy,
}

/// 客户端是否与服务同机：环回地址视为同机（IPv4-mapped IPv6 归一化后再判断，
/// Windows 双栈监听下 127.0.0.1 连接可能呈现为 ::ffff:127.0.0.1）。
pub fn is_local_client(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// 打开所在目录/显示绝对路径只对与服务同机的客户端有意义；远程访问只能复制/显示安全路径。
/// Local 模式只监听环回，天然同机；Lan/Proxy 模式下按实际客户端 IP 判断。
///
/// `ip_is_client` 为假时（Proxy 模式未显式信任 x-forwarded-for），`client_ip` 拿到的是
/// 本机反向代理的回环地址而不是真实客户端，一律拒绝——否则任意远程设备都能让服务器
/// 弹出资源管理器窗口，"仅本机支持打开所在目录"的承诺就不成立了。
pub fn can_open_directory(mode: &AccessMode, client_ip: IpAddr, ip_is_client: bool) -> bool {
    matches!(mode, AccessMode::Local) || (ip_is_client && is_local_client(client_ip))
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccessAction {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AccessRule {
    pub id: String,
    pub network: IpNet,
    pub action: AccessAction,
    pub expires_at: Option<i64>,
}

impl AccessRule {
    pub(super) fn active(&self, now: i64) -> bool {
        self.expires_at.is_none_or(|expires| expires > now)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub version: u32,
    pub mode: AccessMode,
    pub proxy_domain: Option<String>,
    pub access_default: AccessAction,
    pub access_rules: Vec<AccessRule>,
    pub geo_cn: bool,
    pub geo_db: Option<PathBuf>,
    pub trusted_aria2_endpoint: Option<String>,
    pub trusted_ffmpeg_paths: Vec<PathBuf>,
    /// 明确列出可跳过认证的 IP 地址。默认不跳过任何 IP 的认证。
    /// 仅当部署架构确实需要时才添加（如反向代理健康检查）。
    pub auth_bypass_ips: Vec<IpAddr>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            version: 1,
            mode: AccessMode::Local,
            proxy_domain: None,
            access_default: AccessAction::Deny,
            access_rules: Vec::new(),
            geo_cn: false,
            geo_db: None,
            trusted_aria2_endpoint: None,
            trusted_ffmpeg_paths: Vec::new(),
            auth_bypass_ips: Vec::new(),
        }
    }
}

/// 全面检测回环地址：覆盖 127.0.0.0/8、::1、以及 ::ffff:127.x.x.x（IPv4-mapped IPv6）。
/// Windows 双栈 socket 会将 IPv4 连接报告为 ::ffff:127.0.0.1，
/// 标准库 is_loopback() 不识别此形式，需额外处理。
pub fn is_effectively_loopback(ip: IpAddr) -> bool {
    if ip.is_loopback() {
        return true;
    }
    // IPv4 映射的 IPv6 地址（::ffff:127.x.x.x）。
    if let IpAddr::V6(v6) = ip {
        if let Some(mapped_v4) = v6.to_ipv4() {
            return mapped_v4.is_loopback();
        }
    }
    false
}

impl SecurityConfig {
    pub fn validate(&self) -> AppResult<()> {
        if self.mode == AccessMode::Proxy {
            let domain = self
                .proxy_domain
                .as_deref()
                .ok_or_else(|| AppError::Config("proxy 模式缺少公开域名".to_string()))?;
            super::persistence::validate_domain(domain)?;
        }
        if self.geo_db.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(AppError::Config(
                "GeoIP 数据库路径必须是绝对路径".to_string(),
            ));
        }
        if self
            .trusted_ffmpeg_paths
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(AppError::Config(
                "受信任 FFmpeg 路径必须是绝对路径".to_string(),
            ));
        }
        if self.auth_bypass_ips.iter().any(|ip| ip.is_unspecified()) {
            return Err(AppError::Config(
                "auth_bypass_ips 不能包含未指定地址".to_string(),
            ));
        }
        Ok(())
    }

    pub fn should_bypass_auth(&self, ip: IpAddr, ip_is_client: bool) -> bool {
        // 免凭证白名单只按真实客户端地址命中：Proxy 模式不信任 x-forwarded-for 时
        // ip 是本机反代的回环地址，据此放行等于任意远程设备免凭证拿到 Owner。
        ip_is_client && self.auth_bypass_ips.contains(&ip)
    }

    /// 是否存在「按 IP 的限制性配置」——即一旦拿不到真实客户端地址就无法兑现的操作者意图。
    ///
    /// 注意 `access_default == Deny` **不**计入：它是出厂默认值（首次运行自动生成的
    /// security.toml 就写入 deny），无法与「操作者显式选择 deny」区分，计入会让所有未设置
    /// BILI__TRUST_PROXY_HEADERS 的反代部署直接不可用。
    pub fn ip_policy_configured(&self) -> bool {
        let now = Utc::now().timestamp();
        self.geo_cn || self.access_rules.iter().any(|rule| rule.active(now))
    }

    pub fn client_allowed(&self, ip: IpAddr) -> (bool, bool) {
        // 回环地址始终放行：本机访问不受访问策略限制。
        // 使用 is_effectively_loopback 覆盖 IPv4-mapped IPv6 形式（::ffff:127.x.x.x）。
        if is_effectively_loopback(ip) {
            return (true, false);
        }
        let now = Utc::now().timestamp();
        let mut matches = self
            .access_rules
            .iter()
            .filter(|rule| rule.active(now) && rule.network.contains(&ip))
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .network
                .prefix_len()
                .cmp(&left.network.prefix_len())
                .then_with(|| match (left.action, right.action) {
                    (AccessAction::Deny, AccessAction::Allow) => std::cmp::Ordering::Less,
                    (AccessAction::Allow, AccessAction::Deny) => std::cmp::Ordering::Greater,
                    _ => std::cmp::Ordering::Equal,
                })
        });
        match matches.first() {
            Some(rule) => (
                rule.action == AccessAction::Allow,
                rule.action == AccessAction::Allow,
            ),
            None => (self.access_default == AccessAction::Allow, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_open_follows_client_ip() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let loopback_v6: IpAddr = "::1".parse().unwrap();
        let loopback_mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let remote: IpAddr = "192.168.1.10".parse().unwrap();
        assert!(can_open_directory(&AccessMode::Local, remote, true));
        assert!(can_open_directory(&AccessMode::Lan, loopback, true));
        assert!(can_open_directory(&AccessMode::Lan, loopback_v6, true));
        assert!(can_open_directory(&AccessMode::Lan, loopback_mapped, true));
        assert!(!can_open_directory(&AccessMode::Lan, remote, true));
        assert!(!can_open_directory(&AccessMode::Proxy, remote, true));
        // 回归：Proxy 模式不信任 x-forwarded-for 时，解析出的"客户端地址"其实是
        // 本机反代的回环地址，不能据此放行打开目录（任意远程设备都能弹窗）。
        assert!(!can_open_directory(&AccessMode::Proxy, loopback, false));
        assert!(!can_open_directory(
            &AccessMode::Proxy,
            loopback_mapped,
            false
        ));
        // 显式信任 XFF 且客户端确在同机，才恢复"仅本机可打开"的语义。
        assert!(can_open_directory(&AccessMode::Proxy, loopback, true));
    }

    #[test]
    fn most_specific_rule_wins() {
        let config = SecurityConfig {
            access_rules: vec![
                AccessRule {
                    id: "wide".to_string(),
                    network: "10.0.0.0/8".parse().expect("wide net"),
                    action: AccessAction::Deny,
                    expires_at: None,
                },
                AccessRule {
                    id: "host".to_string(),
                    network: "10.2.3.4/32".parse().expect("host net"),
                    action: AccessAction::Allow,
                    expires_at: None,
                },
            ],
            ..SecurityConfig::default()
        };
        assert_eq!(
            config.client_allowed("10.2.3.4".parse().expect("ip")),
            (true, true)
        );
        assert_eq!(
            config.client_allowed("10.9.9.9".parse().expect("ip")),
            (false, false)
        );
    }

    #[test]
    fn auth_bypass_ips_reject_unspecified_addresses() {
        let mut config = SecurityConfig::default();
        assert!(config.auth_bypass_ips.is_empty());
        assert!(config.validate().is_ok());

        config.auth_bypass_ips = vec!["192.0.2.10".parse().expect("ip")];
        assert!(config.validate().is_ok());
        assert!(config.should_bypass_auth("192.0.2.10".parse().expect("ip"), true));
        // 地址不代表真实客户端（Proxy 模式未信任 XFF）时白名单一律不命中。
        assert!(!config.should_bypass_auth("192.0.2.10".parse().expect("ip"), false));

        config.auth_bypass_ips = vec!["0.0.0.0".parse().expect("ip")];
        assert!(config.validate().is_err());
    }

    #[test]
    fn ip_policy_configured_only_counts_explicit_restrictions() {
        let config = SecurityConfig::default();
        // 出厂默认只有 access_default=Deny，无法与"操作者显式选择"区分，不算已配策略：
        // 计入会让未设 BILI__TRUST_PROXY_HEADERS 的反代部署全部 403。
        assert!(!config.ip_policy_configured());

        let with_rule = SecurityConfig {
            access_rules: vec![AccessRule {
                id: "allow-lan".to_string(),
                network: "192.168.1.0/24".parse().expect("lan"),
                action: AccessAction::Allow,
                expires_at: None,
            }],
            ..Default::default()
        };
        assert!(with_rule.ip_policy_configured());

        let expired = SecurityConfig {
            access_rules: vec![AccessRule {
                id: "gone".to_string(),
                network: "192.168.1.0/24".parse().expect("lan"),
                action: AccessAction::Allow,
                expires_at: Some(Utc::now().timestamp() - 60),
            }],
            ..Default::default()
        };
        assert!(!expired.ip_policy_configured());

        let with_geo = SecurityConfig {
            geo_cn: true,
            ..Default::default()
        };
        assert!(with_geo.ip_policy_configured());
    }
}
