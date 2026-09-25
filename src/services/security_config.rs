//! 安全配置服务：`data/security.toml` 的加载/事务化更新（临时文件 + 备份 + 原子替换），
//! 以及 active/configured 双快照——网络模式切换需重启生效，其余字段立即热更新。
//!
//! 子模块分工：`model` 配置结构体与访问策略判定，`persistence` TOML 落盘与
//! 内置 GeoIP 数据库定位。

mod model;
mod persistence;

use crate::error::{AppError, AppResult};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub use model::{
    can_open_directory, is_effectively_loopback, AccessAction, AccessMode, AccessRule,
    SecurityConfig,
};
use persistence::write_config;

fn config_update_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Clone)]
pub struct SecurityConfigService {
    path: PathBuf,
    /// 当前 listener/认证链实际使用的快照。
    inner: Arc<RwLock<SecurityConfig>>,
    /// 磁盘上的目标配置。模式切换写入这里，但在重启前不替换 active mode。
    configured: Arc<RwLock<SecurityConfig>>,
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
        let builtin_geo_db = persistence::locate_builtin_geo_db(app_root);
        if let Some(builtin) = builtin_geo_db.as_ref() {
            tracing::info!(
                "已发现内置 GeoIP 数据库 resources/geo/{}（geo cn on 时无需再执行 geo db 即可直接使用）",
                builtin.file_name().and_then(|name| name.to_str()).unwrap_or("database.mmdb")
            );
        }
        Ok(Self {
            path,
            inner: Arc::new(RwLock::new(config.clone())),
            configured: Arc::new(RwLock::new(config)),
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

    /// 返回已写入 `security.toml`、但可能需要重启才能让 listener 使用的配置。
    pub fn configured(&self) -> SecurityConfig {
        self.configured_read().clone()
    }

    /// 更新已持久化配置对应的当前内存快照。只用于无需重启即可生效的字段；
    /// 访问模式切换仍由调用方保留 `restart_required` 语义。
    pub fn replace_current(&self, config: SecurityConfig) {
        *self.write() = config.clone();
        *self.configured_write() = config;
    }

    pub async fn update(
        &self,
        mutate: impl FnOnce(&mut SecurityConfig) -> AppResult<()>,
    ) -> AppResult<()> {
        // 所有 SecurityConfigService 实例共享同一把进程级锁，覆盖“读当前配置 →
        // 校验 → 写临时文件 → 原子替换”的完整事务，避免 Setup 与设置页互相覆盖。
        let _guard = config_update_lock().lock().await;
        let mut next = self.configured();
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
        *self.configured_write() = next.clone();

        // 网络模式决定已绑定 listener，不能在进程内偷偷切换。其余安全字段
        // （访问规则、Geo、可信路径等）仍立即热更新；这样 `ctl mode` 返回
        // restart_required=true 时，active/configured 两个状态不会互相撒谎。
        let active = self.current();
        let mut active_next = next;
        if active.mode != active_next.mode {
            active_next.mode = active.mode;
            active_next.proxy_domain = active.proxy_domain;
        }
        *self.write() = active_next;
        Ok(())
    }

    pub async fn add_rule(
        &self,
        action: AccessAction,
        network: ipnet::IpNet,
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

    fn configured_read(&self) -> RwLockReadGuard<'_, SecurityConfig> {
        self.configured
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn configured_write(&self) -> RwLockWriteGuard<'_, SecurityConfig> {
        self.configured
            .write()
            .unwrap_or_else(|error| error.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mode_update_keeps_active_listener_until_restart() {
        let data_dir = tempfile::tempdir().expect("data dir");
        let service = SecurityConfigService::load(data_dir.path(), data_dir.path())
            .expect("load security config");

        service
            .update(|config| {
                config.mode = model::AccessMode::Lan;
                Ok(())
            })
            .await
            .expect("persist mode");

        assert_eq!(service.current().mode, model::AccessMode::Local);
        assert_eq!(service.configured().mode, model::AccessMode::Lan);
        assert_ne!(service.current().mode, service.configured().mode);

        service.replace_current(service.configured());
        assert_eq!(service.current().mode, model::AccessMode::Lan);
    }
}
