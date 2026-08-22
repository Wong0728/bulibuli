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

use crate::error::{AppError, AppResult};
#[cfg(not(windows))]
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
#[cfg(not(windows))]
use rand::RngCore;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
#[cfg(target_os = "macos")]
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

#[cfg(all(not(windows), not(target_os = "macos")))]
/// 从环境变量 `BILI__MASTER_KEY` 读取主密钥（hex 或 base64 编码的 32 字节）。
/// 返回 `Ok(None)` 表示未设置；编码非法或长度不是 32 字节时返回 Err，
/// 由调用方直接报错退出——绝不静默回退到文件密钥方案。
fn env_master_key() -> AppResult<Option<Vec<u8>>> {
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

#[cfg(all(not(windows), not(target_os = "macos")))]
/// 解码主密钥：优先按 hex（64 字符）解析，失败则按标准 base64，
/// 再退回 URL-safe base64（无 padding，与 macOS Keychain 存储格式一致）。
/// 空字符串明确返回 None（base64 自身会把空串解为空字节，但作为主密钥无效）。
/// 解码结果必须正好 32 字节，避免"看起来像 base64 的非 32 字节串"被误识别。
fn decode_master_key(raw: &str) -> Option<Vec<u8>> {
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

#[cfg(all(test, not(target_os = "macos")))]
// macOS 的 SecretStore::new 会向真实 Keychain 写入条目，单测不做该平台。
mod tests {
    use super::*;
    use sea_orm_migration::MigratorTrait;

    /// 每个用例独立的临时 data_dir（内含主密钥文件），避免并行测试互踩。
    fn temp_data_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bulibuli-secret-test-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    async fn setup(name: &str) -> (SecretStore, PathBuf) {
        let db = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        crate::migration::Migrator::up(&db, None)
            .await
            .expect("migrate");
        let dir = temp_data_dir(name);
        let store = SecretStore::new(db, &dir).expect("create SecretStore");
        (store, dir)
    }

    #[tokio::test]
    async fn round_trip_set_get_delete() {
        let (store, dir) = setup("roundtrip").await;
        assert_eq!(store.get("cookie").await.unwrap(), None);
        store
            .set("cookie", "SESSDATA=abc; bili_jct=xyz")
            .await
            .unwrap();
        assert_eq!(
            store.get("cookie").await.unwrap().as_deref(),
            Some("SESSDATA=abc; bili_jct=xyz")
        );
        store.delete("cookie").await.unwrap();
        assert_eq!(store.get("cookie").await.unwrap(), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn set_empty_value_deletes() {
        let (store, dir) = setup("empty").await;
        store.set("cookie", "v").await.unwrap();
        store.set("cookie", "").await.unwrap();
        assert_eq!(store.get("cookie").await.unwrap(), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn overwrite_updates_value() {
        let (store, dir) = setup("overwrite").await;
        store.set("cookie", "old").await.unwrap();
        store.set("cookie", "new").await.unwrap();
        assert_eq!(store.get("cookie").await.unwrap().as_deref(), Some("new"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn stored_value_is_ciphertext_not_plaintext() {
        let (store, dir) = setup("ciphertext").await;
        let secret = "SESSDATA=super-secret-value";
        store.set("cookie", secret).await.unwrap();
        let row = store
            .db
            .query_one_raw(Statement::from_sql_and_values(
                store.db.get_database_backend(),
                "SELECT value FROM protected_secrets WHERE key = ?".to_string(),
                [sea_orm::Value::from("cookie")],
            ))
            .await
            .unwrap()
            .expect("row exists");
        let stored: Vec<u8> = row.try_get("", "value").unwrap();
        // 密文必须带格式前缀且不包含明文
        #[cfg(windows)]
        assert_eq!(stored.first(), Some(&FORMAT_DPAPI));
        #[cfg(not(windows))]
        assert_eq!(stored.first(), Some(&FORMAT_AES_GCM));
        let as_text = String::from_utf8_lossy(&stored);
        assert!(!as_text.contains(secret), "密文中不得出现明文凭据");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn tampered_ciphertext_fails_to_decrypt() {
        let (store, dir) = setup("tamper").await;
        store.set("cookie", "SESSDATA=xyz").await.unwrap();
        store
            .db
            .execute_raw(Statement::from_sql_and_values(
                store.db.get_database_backend(),
                "UPDATE protected_secrets SET value = ? WHERE key = ?".to_string(),
                [
                    // 翻转密文中段一个字节（保留格式前缀）
                    sea_orm::Value::Bytes(Some(vec![
                        stored_prefix(),
                        0u8,
                        1,
                        2,
                        3,
                        4,
                        5,
                        6,
                        7,
                        8,
                        9,
                        10,
                        11,
                        12,
                        13,
                    ])),
                    sea_orm::Value::from("cookie"),
                ],
            ))
            .await
            .unwrap();
        assert!(
            store.get("cookie").await.is_err(),
            "篡改后的密文必须解密失败"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(windows)]
    fn stored_prefix() -> u8 {
        FORMAT_DPAPI
    }
    #[cfg(not(windows))]
    fn stored_prefix() -> u8 {
        FORMAT_AES_GCM
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn decode_master_key_accepts_hex_base64_and_urlsafe() {
        let key32 = [0x42u8; 32];
        let hex = hex::encode(key32);
        assert_eq!(decode_master_key(&hex), Some(key32.to_vec()));
        use base64::{
            engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
            Engine,
        };
        let std_b64 = STANDARD.encode(key32);
        assert_eq!(decode_master_key(&std_b64), Some(key32.to_vec()));
        let urlsafe = URL_SAFE_NO_PAD.encode(key32);
        assert_eq!(decode_master_key(&urlsafe), Some(key32.to_vec()));
        // 非法输入返回 None（调用方据此报配置错误而非静默回退）
        assert_eq!(decode_master_key("not-a-key"), None);
        assert_eq!(decode_master_key(""), None);
        // 长度不对的 hex 不接受
        assert_eq!(decode_master_key(&hex[..63]), None);
    }
}
