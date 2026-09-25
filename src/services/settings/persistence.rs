//! 设置的数据库持久化与旧格式迁移：`runtime_config` 单行 JSON 文档的加载/落盘，
//! 旧版逐 key 设置的合并升级，以及明文 Cookie 到受保护凭据存储的一次性迁移。

use super::model::{RuntimeSettings, CONFIG_VERSION};
use crate::error::{AppError, AppResult};
use crate::models::setting;
use crate::services::secret_store::SecretStore;
use chrono::Local;
use sea_orm::{ActiveModelTrait, ConnectionTrait, DatabaseConnection, EntityTrait, Set};
use serde_json::Value;

pub(super) async fn load_runtime_settings(db: &DatabaseConnection) -> AppResult<RuntimeSettings> {
    if let Some(row) = setting::Entity::find_by_id("runtime_config")
        .one(db)
        .await?
    {
        if let Some(value) = row.value {
            let parsed = serde_json::from_str::<Value>(&value).and_then(|mut raw| {
                migrate_legacy_path_display_mode(&mut raw);
                serde_json::from_value::<RuntimeSettings>(raw)
            });
            match parsed {
                Ok(mut settings) => {
                    settings.config_version = CONFIG_VERSION;
                    return Ok(settings);
                }
                Err(error) => {
                    // runtime_config 损坏不再阻断启动：记醒目日志后回退默认配置，
                    // 由下方合并路径用默认值重建并持久化，自愈损坏行。
                    tracing::error!(
                        %error,
                        "runtime_config 解析失败，已回退默认配置；原始配置内容已省略"
                    );
                }
            }
        }
    }
    let mut merged = serde_json::to_value(RuntimeSettings::default())?;
    let rows = setting::Entity::find().all(db).await?;
    for row in rows {
        if matches!(
            row.key.as_str(),
            "cookies" | "device_cookies" | "runtime_config"
        ) {
            continue;
        }
        let Some(value) = row.value else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&value) else {
            tracing::warn!(key = %row.key, "忽略无法解析的旧设置");
            continue;
        };
        if let Some(object) = merged.as_object_mut() {
            object.insert(row.key, parsed);
        }
    }
    migrate_legacy_path_display_mode(&mut merged);
    let mut settings: RuntimeSettings = serde_json::from_value(merged)
        .map_err(|error| AppError::Config(format!("迁移旧设置失败: {error}")))?;
    settings.config_version = CONFIG_VERSION;
    settings.validate()?;
    persist_runtime_settings(db, &settings).await?;
    Ok(settings)
}

pub(super) fn migrate_legacy_path_display_mode(value: &mut Value) {
    let Some(board) = value.get_mut("board").and_then(Value::as_object_mut) else {
        return;
    };
    if board.get("path_display_mode").is_none()
        && board
            .get("show_relative_path")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        board.insert(
            "path_display_mode".to_string(),
            Value::String("relative".to_string()),
        );
    }
}

pub(super) async fn persist_runtime_settings(
    db: &impl ConnectionTrait,
    settings: &RuntimeSettings,
) -> AppResult<()> {
    let mut protected = settings.clone();
    protected.aria2_rpc.secret.clear();
    let value = serde_json::to_value(protected)?;
    // 直接在给定连接上 upsert：保存路径传入的是已开启的事务（与 protected_secrets
    // 同事务提交/回滚）；启动迁移路径传入普通连接，单条 upsert 本身原子。
    let serialized = serde_json::to_string(&value)?;
    if let Some(existing) = setting::Entity::find_by_id("runtime_config")
        .one(db)
        .await?
    {
        let mut model: setting::ActiveModel = existing.into();
        model.value = Set(Some(serialized));
        model.updated_at = Set(Some(Local::now()));
        model.update(db).await?;
    } else {
        setting::ActiveModel {
            key: Set("runtime_config".to_string()),
            value: Set(Some(serialized)),
            updated_at: Set(Some(Local::now())),
        }
        .insert(db)
        .await?;
    }
    Ok(())
}

pub(super) async fn migrate_legacy_cookie(
    db: &DatabaseConnection,
    secret_store: &SecretStore,
) -> AppResult<bool> {
    let mut migrated = false;
    for (legacy_key, protected_key) in [
        ("cookies", "bili_cookie"),
        ("device_cookies", "bili_device_cookies"),
    ] {
        let row = setting::Entity::find_by_id(legacy_key).one(db).await?;
        let Some(value) = row.and_then(|row| row.value) else {
            continue;
        };
        if !value.is_empty() {
            secret_store.set(protected_key, &value).await?;
        }
        db.execute_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            "DELETE FROM settings WHERE key = ?".to_string(),
            [sea_orm::Value::from(legacy_key)],
        ))
        .await?;
        migrated = true;
    }
    Ok(migrated)
}
