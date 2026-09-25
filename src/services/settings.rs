//! 运行时设置：单一 `runtime_config` 版本化 JSON 文档 + 受保护凭据分离存储，
//! 提供 ArcSwap 快照读取与乐观并发（revision）保存。
//!
//! 子模块分工：`model` 设置结构体与默认值，`validate` 设置合法性校验，
//! `persistence` 数据库加载/持久化与旧格式迁移。

mod model;
mod persistence;
mod validate;

use crate::error::{AppError, AppResult};
use crate::services::secret_store::SecretStore;
use arc_swap::ArcSwap;
use persistence::{load_runtime_settings, migrate_legacy_cookie, persist_runtime_settings};
use sea_orm::{DatabaseConnection, TransactionTrait};
use std::sync::Arc;
use tokio::sync::Mutex;

pub use model::*;

#[derive(Clone)]
pub struct SettingsService {
    db: DatabaseConnection,
    current: Arc<ArcSwap<RuntimeSettings>>,
    secret_store: Arc<SecretStore>,
    save_lock: Arc<Mutex<()>>,
}

impl SettingsService {
    pub async fn new(db: DatabaseConnection, secret_store: Arc<SecretStore>) -> AppResult<Self> {
        let mut migrated_plaintext = migrate_legacy_cookie(&db, &secret_store).await?;
        let mut current = load_runtime_settings(&db).await?;
        if !current.aria2_rpc.secret.is_empty() {
            secret_store
                .set("aria2_rpc_secret", &current.aria2_rpc.secret)
                .await?;
            current.aria2_rpc.secret.clear();
            persist_runtime_settings(&db, &current).await?;
            migrated_plaintext = true;
        }
        current.aria2_rpc.secret = secret_store
            .get("aria2_rpc_secret")
            .await?
            .unwrap_or_default();
        current.validate()?;
        if migrated_plaintext {
            secret_store.cleanup_legacy_plaintext().await?;
        }
        Ok(Self {
            db,
            current: Arc::new(ArcSwap::from_pointee(current)),
            secret_store,
            save_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn current(&self) -> Arc<RuntimeSettings> {
        self.current.load_full()
    }

    pub async fn save(&self, settings: RuntimeSettings) -> AppResult<Arc<RuntimeSettings>> {
        self.save_inner(settings, true).await
    }

    async fn save_inner(
        &self,
        mut settings: RuntimeSettings,
        check_revision: bool,
    ) -> AppResult<Arc<RuntimeSettings>> {
        let _save_guard = self.save_lock.lock().await;
        let before = self.current.load_full();
        if check_revision && settings.revision != before.revision {
            return Err(AppError::Conflict(format!(
                "settings revision conflict: expected {}, current {}",
                settings.revision, before.revision
            )));
        }
        let requested_secret = settings.aria2_rpc.secret.clone();
        settings.aria2_rpc.secret = if requested_secret == model::SECRET_MASK {
            before.aria2_rpc.secret.clone()
        } else {
            requested_secret
        };
        settings.validate()?;
        settings.revision = before.revision.saturating_add(1);
        // protected_secrets 与 settings 两表写入放进同一事务：任一失败整体回滚，
        // 不再依赖“先写 secret 再补偿回滚”的跨表补偿路径。
        let transaction = self.db.begin().await?;
        if let Err(error) = self
            .secret_store
            .set_with_conn(&transaction, "aria2_rpc_secret", &settings.aria2_rpc.secret)
            .await
        {
            // 回滚失败通常意味着连接已失效；留痕即可，主错误原样返回。
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::warn!(%rollback_error, "settings 事务回滚失败");
            }
            return Err(error);
        }
        if let Err(error) = persist_runtime_settings(&transaction, &settings).await {
            if let Err(rollback_error) = transaction.rollback().await {
                tracing::warn!(%rollback_error, "settings 事务回滚失败");
            }
            return Err(error);
        }
        transaction.commit().await?;
        // 未提交数据对其他池化连接不可见，回读校验必须放在 commit 之后。
        if !settings.aria2_rpc.secret.is_empty() {
            let verified = self.secret_store.get("aria2_rpc_secret").await?;
            if verified.as_deref() != Some(settings.aria2_rpc.secret.as_str()) {
                return Err(AppError::Internal(
                    "受保护凭据写入后的回读校验失败".to_string(),
                ));
            }
        }
        let settings = Arc::new(settings);
        self.current.store(settings.clone());
        Ok(settings)
    }

    pub async fn reset(&self, expected_revision: Option<u64>) -> AppResult<Arc<RuntimeSettings>> {
        let mut settings = RuntimeSettings::default();
        if let Some(revision) = expected_revision {
            settings.revision = revision;
        }
        self.save_inner(settings, expected_revision.is_some()).await
    }

    pub async fn cookie_header(&self) -> AppResult<String> {
        Ok(self
            .secret_store
            .get("bili_cookie")
            .await?
            .unwrap_or_default())
    }

    pub async fn save_cookie_header(&self, cookies: &str) -> AppResult<()> {
        self.secret_store.set("bili_cookie", cookies).await
    }
}

pub async fn all_settings(db: &DatabaseConnection) -> AppResult<serde_json::Value> {
    Ok(serde_json::to_value(load_runtime_settings(db).await?)?)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
