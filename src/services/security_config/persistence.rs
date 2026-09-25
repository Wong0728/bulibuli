//! 安全配置的落盘与内置资源定位：security.toml 的"临时文件 + 备份 + 原子替换"写入、
//! 公开域名校验，以及 `resources/geo/` 下内置 GeoIP 数据库的查找。

use super::model::SecurityConfig;
use crate::error::{AppError, AppResult};
use std::io::Write;
use std::path::Path;

/// 在 `app_root/resources/geo/` 下查找内置 GeoIP 数据库。
///
/// 优先匹配 `GeoLite2-Country.mmdb`，其次回退到目录中首个 `.mmdb` 文件。
/// 找到的文件必须存在且可读，否则返回 `None`（让上层用 `geo_db` 配置兜底）。
pub(super) fn locate_builtin_geo_db(app_root: &Path) -> Option<std::path::PathBuf> {
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

pub(super) fn validate_domain(domain: &str) -> AppResult<()> {
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

pub(super) fn write_config(path: &Path, config: &SecurityConfig) -> AppResult<()> {
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
        // 尽力清理临时文件：失败只留下一个 .tmp 残留，不影响主错误路径。
        std::fs::remove_file(&temp).ok();
        return Err(error);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
        {
            // 尽力清理临时文件：失败只留下一个 .tmp 残留，不影响主错误路径。
            std::fs::remove_file(&temp).ok();
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
            // 尽力清理临时文件：失败只留下一个 .tmp 残留，不影响主错误路径。
            std::fs::remove_file(&temp).ok();
            return Err(error.into());
        }
        std::fs::remove_file(backup)?;
    } else {
        if let Err(error) = std::fs::rename(&temp, path) {
            // 尽力清理临时文件：失败只留下一个 .tmp 残留，不影响主错误路径。
            std::fs::remove_file(&temp).ok();
            return Err(error.into());
        }
    }
    Ok(())
}
