use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_APP_NAME: &str = "补哩补哩";
const DEFAULT_APP_VERSION: &str = env!("CARGO_PKG_VERSION");
// 默认仅回环：实际监听地址由 security.toml 的模式决定（local/lan/proxy），
// 此处默认值与 SECURITY.md 承诺保持一致，更开放的监听必须显式配置。
const DEFAULT_PORT: u16 = 5000;
const DEFAULT_BILI_API_TIMEOUT: u64 = 10;
const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const DEFAULT_REFERER: &str = "https://www.bilibili.com/";
/// 全局每分钟最大登录/配对尝试次数（防暴力破解）
const DEFAULT_LOGIN_RATE_LIMIT_GLOBAL: usize = 20;

fn default_app_name() -> String {
    DEFAULT_APP_NAME.to_string()
}

fn default_app_version() -> String {
    DEFAULT_APP_VERSION.to_string()
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_bili_api_timeout() -> u64 {
    DEFAULT_BILI_API_TIMEOUT
}

fn default_user_agent() -> String {
    DEFAULT_USER_AGENT.to_string()
}

fn default_referer() -> String {
    DEFAULT_REFERER.to_string()
}

fn default_login_rate_limit_global() -> usize {
    DEFAULT_LOGIN_RATE_LIMIT_GLOBAL
}

fn default_tls_verify() -> bool {
    true
}

fn default_history_limit() -> i64 {
    1000
}

fn default_log_limit() -> i64 {
    100
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default = "default_app_name")]
    pub app_name: String,
    #[serde(default = "default_app_version")]
    pub app_version: String,
    #[serde(default)]
    pub debug: bool,

    #[serde(default = "default_port")]
    pub port: u16,

    /// 显式数据目录。未设置时固定使用可执行文件旁的 data 目录。
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    #[serde(default = "default_bili_api_timeout")]
    pub bili_api_timeout: u64,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    #[serde(default = "default_referer")]
    pub referer: String,

    /// 是否验证 HTTPS 证书。生产环境强烈建议保持 true。
    #[serde(default = "default_tls_verify")]
    pub tls_verify: bool,

    /// 历史记录保留上限（超过后自动清理）
    #[serde(default = "default_history_limit")]
    pub history_limit: i64,
    /// 日志保留上限
    #[serde(default = "default_log_limit")]
    pub log_limit: i64,
    /// 全局每分钟最大登录/配对尝试次数（防暴力破解）
    #[serde(default = "default_login_rate_limit_global")]
    pub login_rate_limit_global: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app_name: default_app_name(),
            app_version: default_app_version(),
            debug: false,
            port: default_port(),
            data_dir: None,
            bili_api_timeout: default_bili_api_timeout(),
            user_agent: default_user_agent(),
            referer: default_referer(),
            tls_verify: default_tls_verify(),
            history_limit: default_history_limit(),
            log_limit: default_log_limit(),
            login_rate_limit_global: default_login_rate_limit_global(),
        }
    }
}

impl AppConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (1..=65534).contains(&self.port),
            "监听端口必须在 1..=65534 范围内（Setup 需要占用下一个端口）"
        );
        anyhow::ensure!(self.bili_api_timeout > 0, "B站 API 超时时间必须大于 0 秒");
        anyhow::ensure!(self.history_limit > 0, "历史记录上限必须大于 0");
        anyhow::ensure!(self.log_limit > 0, "日志上限必须大于 0");
        if let Some(data_dir) = &self.data_dir {
            anyhow::ensure!(data_dir.is_absolute(), "BILI__DATA_DIR 必须是绝对路径");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub app_root: PathBuf,
    pub data_dir: PathBuf,
    pub database_dir: PathBuf,
    pub download_dir: PathBuf,
}

impl AppPaths {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(e) => {
                let cwd = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "未知".to_string());
                tracing::error!(
                    "获取当前可执行文件路径失败: error={}, cwd={}, 将尝试使用工作目录",
                    e,
                    cwd
                );
                std::env::current_dir().context("获取当前工作目录也失败")?
            }
        };
        let executable_dir = executable.parent().context("可执行文件路径没有父目录")?;
        let app_root = executable_dir
            .ancestors()
            .find(|dir| dir.join("static").join("app").join("index.html").is_file())
            .unwrap_or(executable_dir)
            .to_path_buf();
        let data_dir = match &config.data_dir {
            Some(explicit) => explicit.clone(),
            None => app_root.join("data"),
        };
        Self::ensure_writable(&data_dir)?;
        let database_dir = data_dir.join("database");
        let download_dir = data_dir.join("downloads");
        Ok(Self {
            app_root,
            data_dir,
            database_dir,
            download_dir,
        })
    }

    fn ensure_writable(path: &Path) -> Result<()> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("创建便携数据目录失败: {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("设置数据目录权限失败: {}", path.display()))?;
        }
        let test = path.join(".write_test");
        std::fs::write(&test, b"test")
            .with_context(|| format!("便携数据目录不可写: {}", path.display()))?;
        std::fs::remove_file(&test)
            .with_context(|| format!("便携数据目录无法清理测试文件: {}", path.display()))?;
        Ok(())
    }

    pub fn database_url(&self) -> String {
        let path = self.database_dir.join("app.db");
        // 路径必须百分号编码后拼入连接 URL：Unix 上 data 目录含 `?`、`#` 或空格时，
        // 未编码的路径会被 sqlx 的 URL 解析截断，mode=rwc 甚至会在错误位置新建数据库。
        format!(
            "sqlite://{}?mode=rwc",
            encode_path_for_url(&path.to_string_lossy())
        )
    }

    pub fn static_dir(&self) -> PathBuf {
        // 开发环境使用工作目录中的 static。
        let own = self.app_root.join("static");
        if own.join("app").join("index.html").exists() {
            return own;
        }
        // 发布目录无法推导时使用工作目录。
        self.app_root.clone()
    }
}

/// 按 URL path 规则百分号编码（保留 `/`）。sqlx 的 sqlite 解析器对 database 部分
/// 做 percent-decode，因此编码后的 `\`、`:`、`?`、`#`、空格等都能正确还原。
pub(crate) fn encode_path_for_url(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn load_config() -> Result<(AppConfig, AppPaths)> {
    let mut app_config = AppConfig::default();
    apply_environment_overrides(&mut app_config)?;
    app_config.validate().context("配置校验失败")?;

    let paths = AppPaths::new(&app_config)?;
    std::fs::create_dir_all(&paths.database_dir).context("创建数据库目录失败")?;
    std::fs::create_dir_all(&paths.download_dir).context("创建下载目录失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for directory in [&paths.database_dir, &paths.download_dir] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("设置目录权限失败: {}", directory.display()))?;
        }
    }
    Ok((app_config, paths))
}

fn apply_environment_overrides(config: &mut AppConfig) -> Result<()> {
    for (key, value) in std::env::vars() {
        let Some(name) = key.strip_prefix("BILI__") else {
            continue;
        };
        match name {
            "APP_NAME" => config.app_name = value,
            "DEBUG" => config.debug = parse_env(&key, &value)?,
            "PORT" => config.port = parse_env(&key, &value)?,
            "DATA_DIR" => config.data_dir = Some(value.into()),
            "BILI_API_TIMEOUT" => config.bili_api_timeout = parse_env(&key, &value)?,
            "USER_AGENT" => config.user_agent = value,
            "REFERER" => config.referer = value,
            "TLS_VERIFY" => config.tls_verify = parse_env(&key, &value)?,
            "HISTORY_LIMIT" => config.history_limit = parse_env(&key, &value)?,
            "LOG_LIMIT" => config.log_limit = parse_env(&key, &value)?,
            "LOGIN_RATE_LIMIT_GLOBAL" => config.login_rate_limit_global = parse_env(&key, &value)?,
            _ => tracing::debug!(%key, "忽略未知 BILI 环境变量"),
        }
    }
    Ok(())
}

fn parse_env<T: std::str::FromStr>(key: &str, value: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|error| anyhow::anyhow!("环境变量 {key} 的值无效: {error}"))
}

#[cfg(test)]
mod tests {
    use super::AppConfig;

    #[test]
    fn tls_verification_defaults_to_enabled() {
        assert!(AppConfig::default().tls_verify);
    }
}
