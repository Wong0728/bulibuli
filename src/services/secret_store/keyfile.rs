//! Unix（非 macOS）主密钥来源：环境变量 `BILI__MASTER_KEY`（hex 或 base64 编码的
//! 32 字节，存在时优先且不落盘）与 `data/.secret-store.key` 明文密钥文件（0o600）。

use crate::error::{AppError, AppResult};
use rand::RngCore;
use std::path::Path;

/// 从环境变量 `BILI__MASTER_KEY` 读取主密钥（hex 或 base64 编码的 32 字节）。
/// 返回 `Ok(None)` 表示未设置；编码非法或长度不是 32 字节时返回 Err，
/// 由调用方直接报错退出——绝不静默回退到文件密钥方案。
pub(super) fn env_master_key() -> AppResult<Option<Vec<u8>>> {
    let raw = match std::env::var("BILI__MASTER_KEY") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(error) => {
            return Err(AppError::Internal(format!(
                "读取环境变量 BILI__MASTER_KEY 失败: {error}"
            )))
        }
    };
    let trimmed = raw.trim();
    let key = decode_master_key(trimmed).ok_or_else(|| {
        AppError::Config(
            "环境变量 BILI__MASTER_KEY 格式无效：需为 32 字节密钥的 hex（64 字符）或 base64 编码"
                .to_string(),
        )
    })?;
    if key.len() != 32 {
        return Err(AppError::Config(format!(
            "环境变量 BILI__MASTER_KEY 解码后长度为 {} 字节，必须为 32 字节",
            key.len()
        )));
    }
    Ok(Some(key))
}

/// 解码主密钥：优先按 hex（64 字符）解析，失败则按标准 base64，
/// 再退回 URL-safe base64（无 padding，与 macOS Keychain 存储格式一致）。
/// 空字符串明确返回 None（base64 自身会把空串解为空字节，但作为主密钥无效）。
/// 解码结果必须正好 32 字节，避免"看起来像 base64 的非 32 字节串"被误识别。
pub(super) fn decode_master_key(raw: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    if raw.is_empty() {
        return None;
    }
    if raw.len() == 64 {
        if let Ok(key) = hex::decode(raw) {
            if key.len() == 32 {
                return Some(key);
            }
        }
    }
    if let Ok(key) = base64::engine::general_purpose::STANDARD.decode(raw) {
        if key.len() == 32 {
            return Some(key);
        }
    }
    if let Ok(key) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw) {
        if key.len() == 32 {
            return Some(key);
        }
    }
    None
}

pub(super) fn ensure_master_key(path: &Path) -> AppResult<()> {
    if path.exists() {
        let metadata = std::fs::metadata(path)?;
        if metadata.len() != 32 {
            return Err(AppError::Config("SecretStore 主密钥长度无效".to_string()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        return Ok(());
    }
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    let temp = path.with_extension("new");
    std::fs::write(&temp, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(temp, path)?;
    Ok(())
}
