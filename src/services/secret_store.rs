use crate::error::{AppError, AppResult};
#[cfg(not(windows))]
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
#[cfg(not(windows))]
use rand::RngCore;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(not(windows))]
const FORMAT_AES_GCM: u8 = 1;
const FORMAT_DPAPI: u8 = 2;

#[derive(Clone)]
pub struct SecretStore {
    db: DatabaseConnection,
    #[cfg(all(not(windows), not(target_os = "macos")))]
    key_path: Arc<PathBuf>,
    data_dir: Arc<PathBuf>,
}

impl SecretStore {
    pub fn new(db: DatabaseConnection, data_dir: &Path) -> AppResult<Self> {
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let key_path = data_dir.join(".secret-store.key");
        #[cfg(all(not(windows), not(target_os = "macos")))]
        ensure_master_key(&key_path)?;
        #[cfg(target_os = "macos")]
        let _master_key = macos_master_key()?;
        Ok(Self {
            db,
            #[cfg(all(not(windows), not(target_os = "macos")))]
            key_path: Arc::new(key_path),
            data_dir: Arc::new(data_dir.to_path_buf()),
        })
    }

    pub async fn get(&self, key: &str) -> AppResult<Option<String>> {
        let row = self
            .db
            .query_one_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "SELECT value FROM protected_secrets WHERE key = ?".to_string(),
                [sea_orm::Value::from(key)],
            ))
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let encrypted: Vec<u8> = row.try_get("", "value")?;
        let plain = self.unprotect(&encrypted)?;
        String::from_utf8(plain)
            .map(Some)
            .map_err(|error| AppError::Internal(format!("受保护凭据不是 UTF-8: {error}")))
    }

    pub async fn set(&self, key: &str, value: &str) -> AppResult<()> {
        if value.is_empty() {
            return self.delete(key).await;
        }
        let encrypted = self.protect(value.as_bytes())?;
        self.db
            .execute_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "INSERT INTO protected_secrets (key, value, updated_at)
                 VALUES (?, ?, ?)
                 ON CONFLICT(key) DO UPDATE SET
                   value = excluded.value,
                   updated_at = excluded.updated_at"
                    .to_string(),
                [
                    sea_orm::Value::from(key),
                    sea_orm::Value::Bytes(Some(encrypted)),
                    sea_orm::Value::from(chrono::Utc::now().timestamp()),
                ],
            ))
            .await?;
        let verified = self.get(key).await?;
        if verified.as_deref() != Some(value) {
            return Err(AppError::Internal(
                "受保护凭据写入后的回读校验失败".to_string(),
            ));
        }
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> AppResult<()> {
        self.db
            .execute_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "DELETE FROM protected_secrets WHERE key = ?".to_string(),
                [sea_orm::Value::from(key)],
            ))
            .await?;
        Ok(())
    }

    pub async fn cleanup_legacy_plaintext(&self) -> AppResult<()> {
        for pragma in [
            "PRAGMA secure_delete=ON;",
            "PRAGMA wal_checkpoint(TRUNCATE);",
            "VACUUM;",
        ] {
            self.db
                .execute_raw(Statement::from_string(
                    self.db.get_database_backend(),
                    pragma.to_string(),
                ))
                .await?;
        }
        let database_dir = self.data_dir.join("database");
        let data_dir = self.data_dir.clone();
        // 整库复制 + 递归删除属重同步 IO，移到阻塞线程池避免卡住异步 executor
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            let database = database_dir.join("app.db");
            let sanitized = database_dir.join("sanitized-backup");
            std::fs::create_dir_all(&sanitized)?;
            if database.is_file() {
                std::fs::copy(&database, sanitized.join("app.db"))?;
            }
            let legacy_backups = database_dir.join("migration-backups");
            if legacy_backups.is_dir() && legacy_backups.starts_with(data_dir.as_path()) {
                std::fs::remove_dir_all(legacy_backups)?;
            }
            Ok(())
        })
        .await
        .map_err(|error| AppError::Internal(format!("脱敏备份任务失败: {error}")))??;
        Ok(())
    }

    #[cfg(not(windows))]
    fn protect(&self, plain: &[u8]) -> AppResult<Vec<u8>> {
        let key = self.master_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| AppError::Internal("SecretStore 主密钥长度无效".to_string()))?;
        let mut nonce_bytes = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let encrypted = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plain)
            .map_err(|_| AppError::Internal("凭据加密失败".to_string()))?;
        let mut result = Vec::with_capacity(1 + nonce_bytes.len() + encrypted.len());
        result.push(FORMAT_AES_GCM);
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&encrypted);
        Ok(result)
    }

    #[cfg(not(windows))]
    fn unprotect(&self, encrypted: &[u8]) -> AppResult<Vec<u8>> {
        if encrypted.len() < 14 || encrypted[0] != FORMAT_AES_GCM {
            return Err(AppError::Internal("受保护凭据格式无效".to_string()));
        }
        let key = self.master_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| AppError::Internal("SecretStore 主密钥长度无效".to_string()))?;
        cipher
            .decrypt(Nonce::from_slice(&encrypted[1..13]), &encrypted[13..])
            .map_err(|_| AppError::Internal("凭据解密失败或已损坏".to_string()))
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    fn master_key(&self) -> AppResult<Vec<u8>> {
        Ok(std::fs::read(self.key_path.as_ref())?)
    }

    #[cfg(target_os = "macos")]
    fn master_key(&self) -> AppResult<Vec<u8>> {
        macos_master_key()
    }

    #[cfg(windows)]
    fn protect(&self, plain: &[u8]) -> AppResult<Vec<u8>> {
        let protected = dpapi_protect(plain)?;
        let mut result = Vec::with_capacity(protected.len() + 1);
        result.push(FORMAT_DPAPI);
        result.extend_from_slice(&protected);
        Ok(result)
    }

    #[cfg(windows)]
    fn unprotect(&self, encrypted: &[u8]) -> AppResult<Vec<u8>> {
        if encrypted.first() != Some(&FORMAT_DPAPI) {
            return Err(AppError::Internal("受保护凭据格式无效".to_string()));
        }
        dpapi_unprotect(&encrypted[1..])
    }
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn ensure_master_key(path: &Path) -> AppResult<()> {
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

#[cfg(target_os = "macos")]
fn macos_master_key() -> AppResult<Vec<u8>> {
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
    let status = Command::new("security")
        .args([
            "add-generic-password",
            "-a",
            ACCOUNT,
            "-s",
            SERVICE,
            "-w",
            &encoded,
            "-U",
        ])
        .status()?;
    if !status.success() {
        return Err(AppError::Internal(
            "无法在 macOS Keychain 中创建 SecretStore 主密钥".to_string(),
        ));
    }
    Ok(key.to_vec())
}

#[cfg(windows)]
fn dpapi_protect(plain: &[u8]) -> AppResult<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    copy_and_free_blob(output)
}

#[cfg(windows)]
fn dpapi_unprotect(encrypted: &[u8]) -> AppResult<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted.len() as u32,
        pbData: encrypted.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    copy_and_free_blob(output)
}

#[cfg(windows)]
fn copy_and_free_blob(
    blob: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
) -> AppResult<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    if blob.pbData.is_null() {
        return Ok(Vec::new());
    }
    let result = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    unsafe {
        LocalFree(blob.pbData.cast());
    }
    Ok(result)
}
