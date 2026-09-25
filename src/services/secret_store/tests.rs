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
    use super::keyfile::decode_master_key;
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
