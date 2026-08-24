//! 审计日志服务：统一记录所有写操作的来源、目标、版本与结果。
//!
//! 设计要点：
//! - **不抛错**：审计日志写入失败只记 tracing::warn，绝不阻塞业务流程（业务已成功就别因审计失败回滚）
//! - **同步接口**：`record` 是 async 且在调用方上下文内同步落盘（不额外 spawn）；
//!   写入失败仅记 warn，不会让业务拿到错误（见"不抛错"）
//! - **查询接口**：`list` / `by_target` 供 `ctl audit` 子命令调用，支持过滤
//!
//! 与 `conflict_guard.rs` 配合：ConflictGuard 在校验版本前后调用 `record`，
//! 业务 handler 也可直接调用 `record` 记录不经过 ConflictGuard 的操作（如 Cookie 保存）。

use crate::error::AppResult;
use crate::models::operation_log::{
    now_utc_iso8601, Model as OperationLogModel, OperationOutcome, OperationSource, OperationTarget,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, Statement,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

use crate::models::operation_log::Entity as OperationLogEntity;

/// 审计事件广播通道：每次 `record` 成功后向所有订阅者推送一份快照。
/// `ctl events --watch` 与前端 WS 都从此通道消费。
///
/// 容量 256：高并发下载场景下短暂堆积用；订阅者跟不上时丢最旧的事件（审计日志仍以 DB 为准）。
pub type AuditEventSender = broadcast::Sender<AuditEvent>;

/// 审计事件（广播给订阅者用）。比 DB 模型精简，只含前端/AI 关心的字段。
#[derive(Clone, Debug, serde::Serialize)]
pub struct AuditEvent {
    pub at: String,
    pub source: &'static str,
    pub caller_id: String,
    pub route_or_command: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub action: String,
    pub outcome: &'static str,
    pub new_version: Option<i32>,
    pub request_id: String,
}

/// 一次写操作的审计上下文：调用方在执行操作前构造，操作完成后调用 `record` 落盘。
#[derive(Clone, Debug)]
pub struct AuditContext {
    pub source: OperationSource,
    /// 调用方标识：前端 session_id / TUI 与 AI 的 ctl_pid / system
    pub caller_id: String,
    pub route_or_command: String,
    pub target: OperationTarget,
    pub target_id: Option<String>,
    pub action: String,
    pub expected_version: Option<i32>,
    pub request_id: String,
}

impl AuditContext {
    /// 为本机 ctl 调用构造上下文：caller_id 使用进程 PID，便于审计追溯。
    pub fn for_ctl(
        command: &str,
        target: OperationTarget,
        target_id: Option<String>,
        action: &str,
        expected_version: Option<i32>,
        request_id: &str,
    ) -> Self {
        Self {
            source: OperationSource::AiSkill,
            caller_id: format!("ctl_pid:{}", std::process::id()),
            route_or_command: command.to_string(),
            target,
            target_id,
            action: action.to_string(),
            expected_version,
            request_id: request_id.to_string(),
        }
    }
}

#[derive(Clone)]
pub struct AuditLogService {
    db: DatabaseConnection,
    event_sender: AuditEventSender,
}

impl AuditLogService {
    pub fn new(db: DatabaseConnection) -> Self {
        let (event_sender, _) = broadcast::channel(256);
        Self { db, event_sender }
    }

    /// 取事件订阅句柄：`ctl events --watch` 与前端 WS 桥接都从这里订阅。
    pub fn subscribe(&self) -> broadcast::Receiver<AuditEvent> {
        self.event_sender.subscribe()
    }

    /// 记录一条审计日志。**永不返回错误**：DB 写失败仅 warn，业务流程不被阻塞。
    ///
    /// `new_version` / `outcome` / `error_code` 由调用方在操作完成后填入：
    /// - 成功：`outcome=Success`, `new_version=Some(bumped)`, `error_code=None`
    /// - 冲突：`outcome=Conflict`, `new_version=None`, `error_code=Some("CONFLICT")`
    /// - 错误：`outcome=Error`, `new_version=None`, `error_code=Some(code)`
    pub async fn record(
        &self,
        ctx: &AuditContext,
        outcome: OperationOutcome,
        new_version: Option<i32>,
        error_code: Option<&str>,
        detail: Option<serde_json::Value>,
    ) {
        self.record_impl(ctx, outcome, new_version, error_code, detail, true)
            .await;
    }

    /// 与 `record` 相同，但**不广播事件**。用于敏感操作（Cookie 保存等）：
    /// 审计日志仍写 DB 便于追溯，但事件流不暴露“Cookie 已保存”这类信息。
    pub async fn record_silent(
        &self,
        ctx: &AuditContext,
        outcome: OperationOutcome,
        new_version: Option<i32>,
        error_code: Option<&str>,
        detail: Option<serde_json::Value>,
    ) {
        self.record_impl(ctx, outcome, new_version, error_code, detail, false)
            .await;
    }

    async fn record_impl(
        &self,
        ctx: &AuditContext,
        outcome: OperationOutcome,
        new_version: Option<i32>,
        error_code: Option<&str>,
        detail: Option<serde_json::Value>,
        broadcast: bool,
    ) {
        let at = now_utc_iso8601();
        let detail_str = detail
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());
        let model = crate::models::operation_log::ActiveModel {
            at: Set(at.clone()),
            source: Set(ctx.source.as_str().to_string()),
            caller_id: Set(ctx.caller_id.clone()),
            route_or_command: Set(ctx.route_or_command.clone()),
            target_type: Set(ctx.target.as_str().to_string()),
            target_id: Set(ctx.target_id.clone()),
            action: Set(ctx.action.clone()),
            expected_version: Set(ctx.expected_version),
            new_version: Set(new_version),
            outcome: Set(outcome.as_str().to_string()),
            error_code: Set(error_code.map(str::to_string)),
            request_id: Set(ctx.request_id.clone()),
            detail: Set(detail_str),
            ..Default::default()
        };
        match model.insert(&self.db).await {
            Ok(inserted) => {
                // 仅在 broadcast=true 时推送事件给订阅者（敏感操作走 record_silent）
                if broadcast {
                    let event = AuditEvent {
                        at,
                        source: ctx.source.as_str(),
                        caller_id: ctx.caller_id.clone(),
                        route_or_command: ctx.route_or_command.clone(),
                        target_type: ctx.target.as_str().to_string(),
                        target_id: ctx.target_id.clone(),
                        action: ctx.action.clone(),
                        outcome: outcome.as_str(),
                        new_version,
                        request_id: ctx.request_id.clone(),
                    };
                    let _ = self.event_sender.send(event);
                }
                tracing::debug!(audit_id = inserted.id, broadcast, "audit log recorded");
            }
            Err(error) => {
                tracing::warn!(%error, "审计日志写入失败（业务流程不阻塞）");
            }
        }
    }

    /// 列出审计日志，按时间倒序。`source` / `since` 可选过滤。
    ///
    /// `since` 接受 ISO8601 持续时间（如 "1h" / "24h" / "7d"），由 `parse_since` 解析。
    pub async fn list(
        &self,
        source: Option<OperationSource>,
        since: Option<&str>,
        limit: u64,
    ) -> AppResult<Vec<OperationLogModel>> {
        let mut query = OperationLogEntity::find();
        if let Some(src) = source {
            query = query.filter(crate::models::operation_log::Column::Source.eq(src.as_str()));
        }
        if let Some(since_iso) = since.and_then(parse_since_to_iso) {
            query = query.filter(crate::models::operation_log::Column::At.gte(since_iso));
        }
        let rows = query
            .order_by_desc(crate::models::operation_log::Column::At)
            .limit(limit)
            .all(&self.db)
            .await?;
        Ok(rows)
    }

    /// 按目标资源查所有操作历史（不限时间），按时间倒序。
    pub async fn by_target(
        &self,
        target: OperationTarget,
        target_id: &str,
        limit: u64,
    ) -> AppResult<Vec<OperationLogModel>> {
        let rows = OperationLogEntity::find()
            .filter(crate::models::operation_log::Column::TargetType.eq(target.as_str()))
            .filter(crate::models::operation_log::Column::TargetId.eq(target_id))
            .order_by_desc(crate::models::operation_log::Column::At)
            .limit(limit)
            .all(&self.db)
            .await?;
        Ok(rows)
    }

    /// 清理 N 天前的审计日志。返回被删除的行数。供 30 天清理任务调用。
    pub async fn prune_older_than_days(&self, days: i64) -> AppResult<u64> {
        let cutoff = (Utc::now() - ChronoDuration::days(days)).to_rfc3339();
        let backend = self.db.get_database_backend();
        let result = self
            .db
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM operation_log WHERE at < ?",
                [cutoff.into()],
            ))
            .await?;
        Ok(result.rows_affected())
    }

    /// 启动 30 天审计日志清理任务：每 24 小时清理一次 30 天前的记录。
    /// 在 `main.rs` 启动时 spawn 一次；任务随 `cancellation` 取消退出。
    pub fn start_cleanup_task(
        self: Arc<Self>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // 启动后先等 5 分钟再首次清理，避免与启动峰值重叠
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(300)) => {}
                _ = cancellation.cancelled() => return,
            }
            loop {
                match self.prune_older_than_days(30).await {
                    Ok(deleted) if deleted > 0 => {
                        tracing::info!(deleted, "审计日志清理完成（保留 30 天）");
                    }
                    Ok(_) => {
                        tracing::debug!("审计日志清理完成，无过期记录");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "审计日志清理失败（下次重试）");
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(86400)) => {}
                    _ = cancellation.cancelled() => {
                        tracing::info!("审计日志清理任务退出");
                        return;
                    }
                }
            }
        })
    }
}

/// 解析 "1h" / "24h" / "7d" / "30m" 风格的时间段为 ISO8601 UTC 时间点（now - duration）。
/// 返回 None 表示输入无法解析（调用方按"不过滤"处理）。
fn parse_since_to_iso(since: &str) -> Option<String> {
    let since_trimmed = since.trim();
    if since_trimmed.is_empty() {
        return None;
    }
    // strip_suffix 按字符边界切分：多字节输入（如 `1小时`）不会像字节切片那样 panic。
    let (num, unit) = ["h", "d", "m"].iter().find_map(|unit| {
        since_trimmed
            .strip_suffix(unit)
            .and_then(|num| num.parse::<i64>().ok())
            .map(|num| (num, *unit))
    })?;
    let duration = match unit {
        "h" => ChronoDuration::hours(num),
        "d" => ChronoDuration::days(num),
        "m" => ChronoDuration::minutes(num),
        _ => return None,
    };
    let now: DateTime<Utc> = Utc::now();
    Some((now - duration).to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, DatabaseConnection};
    use sea_orm_migration::MigratorTrait;

    async fn setup_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::migration::Migrator::up(&db, None)
            .await
            .expect("migrate");
        db
    }

    #[tokio::test]
    async fn record_inserts_row_and_broadcasts_event() {
        let db = setup_db().await;
        let service = AuditLogService::new(db);
        let mut rx = service.subscribe();
        let ctx = AuditContext::for_ctl(
            "dl pause 1",
            OperationTarget::Task,
            Some("1".to_string()),
            "pause",
            Some(5),
            "req-1",
        );
        service
            .record(&ctx, OperationOutcome::Success, Some(6), None, None)
            .await;
        let event = rx.try_recv().expect("should broadcast event");
        assert_eq!(event.source, "ai_skill");
        assert_eq!(event.outcome, "success");
        assert_eq!(event.new_version, Some(6));

        let rows = service.list(None, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, "pause");
        assert_eq!(rows[0].expected_version, Some(5));
    }

    #[tokio::test]
    async fn list_filters_by_source_and_since() {
        let db = setup_db().await;
        let service = AuditLogService::new(db);
        // 写两条：一条 AI、一条 system
        let ai_ctx =
            AuditContext::for_ctl("dl add", OperationTarget::Task, None, "add", None, "r1");
        let sys_ctx = AuditContext {
            source: OperationSource::System,
            caller_id: "system:monitor".to_string(),
            route_or_command: "monitor".to_string(),
            target: OperationTarget::Task,
            target_id: Some("9".to_string()),
            action: "scan".to_string(),
            expected_version: None,
            request_id: "r-system".to_string(),
        };
        service
            .record(&ai_ctx, OperationOutcome::Success, None, None, None)
            .await;
        service
            .record(&sys_ctx, OperationOutcome::Success, None, None, None)
            .await;

        let ai_only = service
            .list(Some(OperationSource::AiSkill), None, 10)
            .await
            .unwrap();
        assert_eq!(ai_only.len(), 1);
        assert_eq!(ai_only[0].source, "ai_skill");

        let all = service.list(None, None, 10).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn by_target_returns_history() {
        let db = setup_db().await;
        let service = AuditLogService::new(db);
        for action in ["add", "pause", "resume"] {
            let ctx = AuditContext::for_ctl(
                "dl",
                OperationTarget::Task,
                Some("42".into()),
                action,
                None,
                "r",
            );
            service
                .record(&ctx, OperationOutcome::Success, None, None, None)
                .await;
        }
        // 另一个 task 的记录
        let other = AuditContext::for_ctl(
            "dl",
            OperationTarget::Task,
            Some("99".into()),
            "add",
            None,
            "r",
        );
        service
            .record(&other, OperationOutcome::Success, None, None, None)
            .await;

        let rows = service
            .by_target(OperationTarget::Task, "42", 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.target_id.as_deref() == Some("42")));
    }

    #[test]
    fn parse_since_handles_common_units() {
        assert!(parse_since_to_iso("1h").is_some());
        assert!(parse_since_to_iso("24h").is_some());
        assert!(parse_since_to_iso("7d").is_some());
        assert!(parse_since_to_iso("30m").is_some());
        assert!(parse_since_to_iso("").is_none());
        assert!(parse_since_to_iso("abc").is_none());
        assert!(parse_since_to_iso("5x").is_none());
    }

    #[test]
    fn parse_since_multibyte_input_does_not_panic() {
        // 尾字符为多字节时按字节切片会 panic；必须按字符边界解析并返回 None。
        assert!(parse_since_to_iso("1小时").is_none());
        assert!(parse_since_to_iso("小时").is_none());
        assert!(parse_since_to_iso("小时h").is_none());
    }
}
