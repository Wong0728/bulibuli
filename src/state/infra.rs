//! 基础设施领域状态：配置、路径、数据库、WebSocket、设置服务、取消令牌、审计与冲突守卫。

use crate::config::{AppConfig, AppPaths};
use crate::services::audit_log::AuditLogService;
use crate::services::conflict_guard::ConflictGuardService;
use crate::services::secret_store::SecretStore;
use crate::services::settings::SettingsService;
use crate::ws::WebSocketManager;
use anyhow::Context;
use sea_orm::DatabaseConnection;
use std::sync::atomic::{AtomicBool, AtomicU16};
use std::sync::Arc;
use std::time::Instant;

/// 基础设施领域：承载应用启动所依赖的底层资源。
#[derive(Clone)]
pub struct InfraState {
    pub(crate) config: Arc<AppConfig>,
    pub(crate) paths: Arc<AppPaths>,
    pub(crate) db: DatabaseConnection,
    pub(crate) ws: Arc<WebSocketManager>,
    pub(crate) settings_service: Arc<SettingsService>,
    pub(crate) cancellation: tokio_util::sync::CancellationToken,
    /// AI Skill 模式开关（来自 onboarding / `ai on|off`）。ctl 命令门控以此为准。
    pub(crate) ai_skill_enabled: Arc<AtomicBool>,
    /// 进程启动时刻，供 `sys status` 计算运行时长。
    pub(crate) started_at: Instant,
    /// 审计日志服务：所有写操作经此记录，供 `ctl audit` 查询与前端追溯。
    pub(crate) audit_log: Arc<AuditLogService>,
    /// 乐观锁冲突守卫：写操作前校验 `version` 字段，匹配才能执行。
    pub(crate) conflict_guard: Arc<ConflictGuardService>,
    /// 主服务器实际绑定端口（绑定后写入，供 API 查询）。
    pub(crate) actual_main_port: Arc<AtomicU16>,
    /// Setup 服务器实际绑定端口（启动后写入，供 API 查询）。
    pub(crate) actual_setup_port: Arc<AtomicU16>,
}

impl InfraState {
    /// 构建基础设施领域。
    ///
    /// `ai_skill_enabled` 由启动流程从 `startup_state.json` 传入，作为 ctl 命令门控的初始值。
    /// 返回 `(InfraState, SecretStore)` —— `SecretStore` 需要传递给后续领域。
    pub(crate) async fn build(
        config: AppConfig,
        paths: AppPaths,
        db: DatabaseConnection,
        ai_skill_enabled: bool,
    ) -> anyhow::Result<(Self, Arc<SecretStore>)> {
        let config = Arc::new(config);
        let paths = Arc::new(paths);
        let ws = Arc::new(WebSocketManager::new());
        let cancellation = tokio_util::sync::CancellationToken::new();

        let secret_store = Arc::new(
            SecretStore::new(db.clone(), &paths.data_dir).context("初始化 SecretStore 失败")?,
        );
        let settings_service = Arc::new(
            SettingsService::new(db.clone(), secret_store.clone())
                .await
                .context("初始化 SettingsService 失败")?,
        );
        let audit_log = Arc::new(AuditLogService::new(db.clone()));
        let conflict_guard = Arc::new(ConflictGuardService::new(db.clone()));

        let state = Self {
            config: config.clone(),
            paths,
            db,
            ws,
            settings_service,
            cancellation,
            ai_skill_enabled: Arc::new(AtomicBool::new(ai_skill_enabled)),
            started_at: Instant::now(),
            audit_log,
            conflict_guard,
            actual_main_port: Arc::new(AtomicU16::new(config.port)),
            actual_setup_port: Arc::new(AtomicU16::new(config.port + 1)),
        };
        Ok((state, secret_store))
    }
}
