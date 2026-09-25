//! macOS 主密钥来源：存于系统 Keychain（`security` 命令行工具），
//! 首次运行生成随机 32 字节密钥并以 stdin 写入，避免密钥暴露于进程列表。

use crate::error::{AppError, AppResult};
use rand::RngCore;
use std::io::Write;

pub(super) fn macos_master_key() -> AppResult<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use std::process::Command;
    const ACCOUNT: &str = "bulibuli";
    const SERVICE: &str = "bulibuli-secret-store";
    let existing = Command::new("security")
        .args(["find-generic-password", "-a", ACCOUNT, "-s", SERVICE, "-w"])
        .output()?;
    if existing.status.success() {
        let encoded = String::from_utf8(existing.stdout)
            .map_err(|error| AppError::Internal(format!("Keychain 输出不是 UTF-8: {error}")))?;
        let key = URL_SAFE_NO_PAD
            .decode(encoded.trim())
            .map_err(|_| AppError::Internal("Keychain 主密钥格式无效".to_string()))?;
        if key.len() != 32 {
            return Err(AppError::Internal("Keychain 主密钥长度无效".to_string()));
        }
        return Ok(key);
    }
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    let encoded = URL_SAFE_NO_PAD.encode(key);
    // 使用 stdin 传入密钥而非 argv：`security -w` 从 stdin 读取可避免密钥短暂暴露于
    // `ps` 进程列表（虽然密钥是随机值而非用户口令，且写入仅一次）。
    let mut child = Command::new("security")
        .args([
            "add-generic-password",
            "-a",
            ACCOUNT,
            "-s",
            SERVICE,
            "-w",
            "-",
            "-U",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(encoded.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(AppError::Internal(
            "无法在 macOS Keychain 中创建 SecretStore 主密钥".to_string(),
        ));
    }
    Ok(key.to_vec())
}
