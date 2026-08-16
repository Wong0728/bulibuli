use crate::error::{AppError, AppResult};
use chrono::Utc;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

fn config_update_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

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
pub fn can_open_directory(mode: &AccessMode, client_ip: IpAddr) -> bool {
    matches!(mode, AccessMode::Local) || is_local_client(client_ip)
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
    fn active(&self, now: i64) -> bool {
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
            validate_domain(domain)?;
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

    pub fn should_bypass_auth(&self, ip: IpAddr) -> bool {
        self.auth_bypass_ips.contains(&ip)
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

#[derive(Clone)]
pub struct SecurityConfigService {
    path: PathBuf,
    inner: Arc<RwLock<SecurityConfig>>,
    /// 内置 GeoIP 数据库路径（位于 `resources/geo/` 下，未在 security.toml 显式配置时使用）。
    /// 不写入配置文件，避免跨机器路径不可移植。
    builtin_geo_db: Option<PathBuf>,
}

impl SecurityConfigService {
    pub fn load(data_dir: &Path, app_root: &Path) -> AppResult<Self> {
        let path = data_dir.join("security.toml");
        let config = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            toml::from_str::<SecurityConfig>(&raw)
                .map_err(|error| AppError::Config(format!("解析 security.toml 失败: {error}")))?
        } else {
            let config = SecurityConfig::default();
            write_config(&path, &config)?;
            tracing::warn!(
                "未找到 security.toml，已在 data/security.toml 自动生成默认安全配置文件。所有安全特性已启用。"
            );
            config
        };
        config.validate()?;
        if !config.auth_bypass_ips.is_empty() {
            tracing::warn!(
                count = config.auth_bypass_ips.len(),
                "auth_bypass_ips 已启用：仅明确列出的客户端 IP 跳过认证，不等同于可信网络"
            );
        }
        let builtin_geo_db = locate_builtin_geo_db(app_root);
        if let Some(builtin) = builtin_geo_db.as_ref() {
            tracing::info!(
                "已发现内置 GeoIP 数据库 resources/geo/{}（geo cn on 时无需再执行 geo db 即可直接使用）",
                builtin.file_name().and_then(|name| name.to_str()).unwrap_or("database.mmdb")
            );
        }
        Ok(Self {
            path,
            inner: Arc::new(RwLock::new(config)),
            builtin_geo_db,
        })
    }

    /// 返回当前生效的 GeoIP 数据库路径：优先用户在 security.toml 中显式配置的 `geo_db`，
    /// 未配置时回退到内置数据库（`resources/geo/`）。两者均无则返回 `None`。
    pub fn effective_geo_db(&self) -> Option<PathBuf> {
        let config = self.current();
        config.geo_db.or_else(|| self.builtin_geo_db.clone())
    }

    pub fn current(&self) -> SecurityConfig {
        self.read().clone()
    }

    /// 更新已持久化配置对应的当前内存快照。只用于无需重启即可生效的字段；
    /// 访问模式切换仍由调用方保留 `restart_required` 语义。
    pub fn replace_current(&self, config: SecurityConfig) {
        *self.write() = config;
    }

    pub async fn update(
        &self,
        mutate: impl FnOnce(&mut SecurityConfig) -> AppResult<()>,
    ) -> AppResult<()> {
        // 所有 SecurityConfigService 实例共享同一把进程级锁，覆盖“读当前配置 →
        // 校验 → 写临时文件 → 原子替换”的完整事务，避免 Setup 与设置页互相覆盖。
        let _guard = config_update_lock().lock().await;
        let mut next = self.current();
        mutate(&mut next)?;
        next.access_rules
            .retain(|rule| rule.active(Utc::now().timestamp()));
        next.validate()?;
        // TOML 落盘（临时文件 + 备份 + 重命名）是多次同步文件 IO，
        // 移到阻塞线程池，避免在请求路径的异步上下文中阻塞 executor
        let path = self.path.clone();
        let snapshot = next.clone();
        tokio::task::spawn_blocking(move || write_config(&path, &snapshot))
            .await
            .map_err(|error| {
                AppError::Internal(format!("保存 security.toml 任务失败: {error}"))
            })??;
        *self.write() = next;
        Ok(())
    }

    pub async fn add_rule(
        &self,
        action: AccessAction,
        network: IpNet,
        minutes: Option<u64>,
    ) -> AppResult<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let expires_at = minutes.map(|value| {
            Utc::now()
                .timestamp()
                .saturating_add((value.min(525_600) * 60) as i64)
        });
        let rule_id = id.clone();
        self.update(move |config| {
            config.access_rules.push(AccessRule {
                id: rule_id,
                network,
                action,
                expires_at,
            });
            Ok(())
        })
        .await?;
        Ok(id)
    }

    pub async fn remove_rule(&self, id: &str) -> AppResult<bool> {
        let mut removed = false;
        self.update(|config| {
            let before = config.access_rules.len();
            config.access_rules.retain(|rule| rule.id != id);
            removed = before != config.access_rules.len();
            Ok(())
        })
        .await?;
        Ok(removed)
    }

    fn read(&self) -> RwLockReadGuard<'_, SecurityConfig> {
        self.inner.read().unwrap_or_else(|error| error.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, SecurityConfig> {
        self.inner
            .write()
            .unwrap_or_else(|error| error.into_inner())
    }
}

/// 在 `app_root/resources/geo/` 下查找内置 GeoIP 数据库。
///
/// 优先匹配 `GeoLite2-Country.mmdb`，其次回退到目录中首个 `.mmdb` 文件。
/// 找到的文件必须存在且可读，否则返回 `None`（让上层用 `geo_db` 配置兜底）。
fn locate_builtin_geo_db(app_root: &Path) -> Option<PathBuf> {
    let geo_dir = app_root.join("resources").join("geo");
    let preferred = geo_dir.join("GeoLite2-Country.mmdb");
    if preferred.is_file() {
        return Some(preferred);
    }
    if !geo_dir.is_dir() {
        return None;
    }
    std::fs::read_dir(&geo_dir)
        .ok()?
        .flatten()
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "mmdb"))
        .map(|entry| entry.path())
}

fn validate_domain(domain: &str) -> AppResult<()> {
    let valid = !domain.is_empty()
        && domain == domain.to_ascii_lowercase()
        && !domain.contains(['/', ':', '*', ' '])
        && domain.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequest("公开域名格式无效".to_string()))
    }
}

fn write_config(path: &Path, config: &SecurityConfig) -> AppResult<()> {
    let raw = toml::to_string_pretty(config)
        .map_err(|error| AppError::Config(format!("序列化 security.toml 失败: {error}")))?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Config("安全配置路径缺少父目录".to_string()))?;
    std::fs::create_dir_all(parent)?;
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let temp = parent.join(format!(".security.toml.{unique}.tmp"));
    let backup = parent.join(format!(".security.toml.{unique}.bak"));
    let write_result = (|| -> AppResult<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(raw.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_file(&temp);
            return Err(error.into());
        }
    }
    if path.exists() {
        std::fs::rename(path, &backup)?;
        if let Err(error) = std::fs::rename(&temp, path) {
            let rollback_result = std::fs::rename(&backup, path);
            if let Err(rollback_error) = rollback_result {
                tracing::error!(%rollback_error, "security.toml 回滚失败");
            }
            let _ = std::fs::remove_file(&temp);
            return Err(error.into());
        }
        std::fs::remove_file(backup)?;
    } else {
        if let Err(error) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(error.into());
        }
    }
    Ok(())
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
        assert!(can_open_directory(&AccessMode::Local, remote));
        assert!(can_open_directory(&AccessMode::Lan, loopback));
        assert!(can_open_directory(&AccessMode::Lan, loopback_v6));
        assert!(can_open_directory(&AccessMode::Lan, loopback_mapped));
        assert!(!can_open_directory(&AccessMode::Lan, remote));
        assert!(!can_open_directory(&AccessMode::Proxy, remote));
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
        assert!(config.should_bypass_auth("192.0.2.10".parse().expect("ip")));

        config.auth_bypass_ips = vec!["0.0.0.0".parse().expect("ip")];
        assert!(config.validate().is_err());
    }
}
