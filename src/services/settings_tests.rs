use super::*;
use sea_orm::{ConnectionTrait, Database, Statement};

#[test]
fn defaults_are_valid_and_versioned() {
    let settings = RuntimeSettings::default();
    settings.validate().expect("default settings are valid");
    assert_eq!(settings.config_version, CONFIG_VERSION);
}

#[test]
fn invalid_parallelism_is_rejected() {
    let mut settings = RuntimeSettings::default();
    settings.parallel_download.max_parallel = 0;
    assert!(settings.validate().is_err());
}

#[test]
fn legacy_settings_use_overwrite_archive_defaults() {
    let settings: RuntimeSettings =
        serde_json::from_value(serde_json::json!({})).expect("deserialize legacy settings");
    assert_eq!(settings.danmaku_comment.sidecar_archive_mode, "overwrite");
    assert_eq!(settings.danmaku_comment.sidecar_archive_limit, 3);
}

#[test]
fn invalid_archive_settings_are_rejected() {
    let mut settings = RuntimeSettings::default();
    settings.danmaku_comment.sidecar_archive_mode = "unknown".to_string();
    assert!(settings.validate().is_err());

    settings.danmaku_comment.sidecar_archive_mode = "keep_latest_n".to_string();
    settings.danmaku_comment.sidecar_archive_limit = 51;
    assert!(settings.validate().is_err());
}

async fn settings_database() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        "CREATE TABLE settings (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT,
            updated_at TEXT
        )"
        .to_string(),
    ))
    .await
    .expect("create settings table");
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        "CREATE TABLE protected_secrets (
            key TEXT PRIMARY KEY NOT NULL,
            value BLOB NOT NULL,
            updated_at INTEGER NOT NULL
        )"
        .to_string(),
    ))
    .await
    .expect("create protected secrets table");
    db
}

fn test_secret_store(db: DatabaseConnection) -> Arc<SecretStore> {
    let directory = tempfile::tempdir().expect("temp secret directory");
    let path = directory.keep();
    Arc::new(SecretStore::new(db, &path).expect("secret store"))
}

#[tokio::test]
async fn save_atomically_updates_the_shared_snapshot() {
    let db = settings_database().await;
    let service = SettingsService::new(db.clone(), test_secret_store(db.clone()))
        .await
        .expect("create settings service");
    let mut next = (*service.current()).clone();
    next.parallel_download.max_parallel = 6;

    let saved = service.save(next).await.expect("save settings");
    assert_eq!(saved.parallel_download.max_parallel, 6);
    assert_eq!(service.current().parallel_download.max_parallel, 6);

    let reloaded = SettingsService::new(db.clone(), test_secret_store(db))
        .await
        .expect("reload settings service");
    assert_eq!(reloaded.current().parallel_download.max_parallel, 6);
}

#[tokio::test]
async fn migrates_legacy_individual_settings_into_versioned_bundle() {
    let db = settings_database().await;
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        "INSERT INTO settings (key, value) VALUES (
            'parallel_download',
            '{\"max_parallel\":5,\"wait_slot_timeout_secs\":300}'
        )"
        .to_string(),
    ))
    .await
    .expect("insert legacy setting");

    let service = SettingsService::new(db.clone(), test_secret_store(db.clone()))
        .await
        .expect("migrate legacy settings");
    assert_eq!(service.current().config_version, CONFIG_VERSION);
    assert_eq!(service.current().parallel_download.max_parallel, 5);
    assert!(setting::Entity::find_by_id("runtime_config")
        .one(&db)
        .await
        .expect("query runtime config")
        .is_some());
}
