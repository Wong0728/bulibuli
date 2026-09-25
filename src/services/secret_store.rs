//! 受保护凭据存储。
//!
//! # 威胁模型（主密钥来源）
//!
//! - **Windows**：DPAPI 按用户账户加密，密钥由系统管理，不落盘于应用目录。
//! - **macOS**：主密钥存于 Keychain，受系统访问控制保护。
//! - **Linux/其他 Unix**：无系统级密钥服务可用时，主密钥只能以 0o600 权限
//!   明文存放在 `data/.secret-store.key`。这只能防御"其他用户读取"与
//!   "备份/同步误带库文件"两类风险，**无法防御同 UID 进程或拥有 root/
//!   磁盘访问权的攻击者**——该场景超出本模块的防护边界。
//! - 生产/服务器部署应通过环境变量 `BILI__MASTER_KEY` 注入主密钥
//!   （hex 或 base64 编码的 32 字节）：存在时优先使用且**不落盘**；
//!   格式非法时直接报错退出而非回退到文件方案——静默回退会导致同一份
//!   密文在不同密钥下产生不可解的分裂状态，比启动失败更难排查。
//!
//! 子模块分工：`keyfile` Unix 文件密钥与 `BILI__MASTER_KEY` 环境变量，
//! `macos_keychain` Keychain 主密钥，`dpapi` Windows DPAPI 加解密。

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

#[cfg(windows)]
mod dpapi;
#[cfg(all(not(windows), not(target_os = "macos")))]
mod keyfile;
#[cfg(target_os = "macos")]
mod macos_keychain;

#[cfg(windows)]
use dpapi::{dpapi_protect, dpapi_unprotect};
#[cfg(all(not(windows), not(target_os = "macos")))]
use keyfile::{ensure_master_key, env_master_key};
#[cfg(target_os = "macos")]
use macos_keychain::macos_master_key;

#[cfg(not(windows))]
const FORMAT_AES_GCM: u8 = 1;
#[cfg(windows)]
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
        {
            // 环境变量存在时优先使用且不落盘；格式非法直接报错退出，
            // 不回退到明文文件方案（见模块头威胁模型说明）。
            if env_master_key()?.is_none() {
                ensure_master_key(&key_path)?;
            }
        }
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
        self.set_with_conn(&self.db, key, value).await?;
        let verified = self.get(key).await?;
        if verified.as_deref() != Some(value) {
            return Err(AppError::Internal(
                "受保护凭据写入后的回读校验失败".to_string(),
            ));
        }
        Ok(())
    }

    /// 在指定连接（含未提交事务）上写入凭据：空值等价删除。
    /// 供需要与业务表同事务原子写入的调用方使用；回读校验由调用方在事务提交后完成
    /// （未提交数据对其他池化连接不可见，事务内校验必然失败）。
    pub async fn set_with_conn(
        &self,
        conn: &impl ConnectionTrait,
        key: &str,
        value: &str,
    ) -> AppResult<()> {
        if value.is_empty() {
            conn.execute_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "DELETE FROM protected_secrets WHERE key = ?".to_string(),
                [sea_orm::Value::from(key)],
            ))
            .await?;
            return Ok(());
        }
        let encrypted = self.protect(value.as_bytes())?;
        conn.execute_raw(Statement::from_sql_and_values(
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
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> AppResult<()> {
        self.set_with_conn(&self.db, key, "").await
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
        if let Some(key) = env_master_key()? {
            return Ok(key);
        }
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

#[cfg(all(test, not(target_os = "macos")))]
// macOS 的 SecretStore::new 会向真实 Keychain 写入条目，单测不做该平台。
mod tests;
