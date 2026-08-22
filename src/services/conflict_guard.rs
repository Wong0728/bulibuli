//! 乐观锁冲突守卫：在执行写操作前校验目标资源的 `version` 字段，匹配才能执行。
//!
//! 设计原则（与 `startup-and-ai-skill-plan.md` 第六章一致）：
//! - **先校验再执行**：不验证不执行，避免 AI 在执行过程中"直接执行了"
//! - **未提交前不广播**：校验失败仅返回冲突错误，不广播事件（避免被其他端抢占）
//! - **冲突错误结构化**：返回带当前版本信息的 `AppError::Conflict`，调用方可上报或重试
//! - **不传 version = 最后写入胜出**：`expected_version=None` 时跳过校验，直接 bump（用于幂等操作）
//!
//! 范围：仅对状态变更类操作启用乐观锁。只读操作不锁，幂等操作（如 `dl add` 同 bvid 重复入队）不要求 version。
//!
//! 用法：
//! ```ignore
//! let guard = conflict_guard.check_and_bump(
//!     OperationTarget::Task,
//!     &task_id.to_string(),
//!     Some(expected_version),
//! ).await?;
//! // ↑ 校验通过 + version 已经 +1（原子事务）
//! // 调用方在这里执行实际的状态变更（update SQL）
//! // 失败时调用 guard.rollback()，成功时 guard.commit()
//! ```

use crate::error::{AppError, AppResult};
use crate::models::operation_log::OperationTarget;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};

/// 乐观锁守卫：持有一个已通过校验并 bump 的写操作上下文。
///
/// 调用方必须在执行实际状态变更后调用 `commit()` 或 `rollback()`：
/// - 成功：`commit()` 是 no-op（version 已在 `check_and_bump` 内 bump）
/// - 失败：`rollback()` 把 version 减回，让其他端可以重试
///
/// 实现上 version 的 bump 与回滚都是直接 SQL，不依赖 ORM ActiveModel（避免触发 ActiveModelBehavior 副作用）。
pub struct ConflictGuard {
    db: DatabaseConnection,
    target: OperationTarget,
    target_id: String,
    new_version: i32,
    committed: bool,
}

impl ConflictGuard {
    /// 返回这次操作应当使用的新版本号（写 SQL 时 SET version = this）。
    pub fn new_version(&self) -> i32 {
        self.new_version
    }

    /// 标记操作成功。version 已在 `check_and_bump` 时 bump，此处仅置标志位。
    pub fn commit(mut self) {
        self.committed = true;
    }

    /// 回滚 version：只回滚本次 bump 产生的版本。
    /// 仅在调用方执行实际状态变更失败时调用。
    pub async fn rollback(mut self) -> AppResult<()> {
        if self.committed {
            return Ok(());
        }
        let table = table_for(self.target)?;
        let sql = format!("UPDATE {table} SET version = version - 1 WHERE id = ? AND version = ?");
        let result = self
            .db
            .execute_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                sql,
                [self.target_id.clone().into(), self.new_version.into()],
            ))
            .await?;
        if result.rows_affected() != 1 {
            // 行已不存在（如 remove 失败路径中任务行已被删除）：无需回滚，视为成功，
            // 避免必然失败的回滚报 Conflict 掩盖真实失败原因。
            let exists = self
                .db
                .query_one_raw(Statement::from_sql_and_values(
                    self.db.get_database_backend(),
                    format!("SELECT 1 FROM {table} WHERE id = ?"),
                    [self.target_id.clone().into()],
                ))
                .await?;
            if exists.is_none() {
                self.committed = true;
                return Ok(());
            }
            return Err(AppError::Conflict(format!(
                "{:?} id={} 回滚时版本已被其他写入改变",
                self.target, self.target_id
            )));
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for ConflictGuard {
    fn drop(&mut self) {
        if !self.committed {
            tracing::warn!(
                target = self.target.as_str(),
                target_id = %self.target_id,
                new_version = self.new_version,
                "ConflictGuard 未提交也未回滚（建议调用方显式处理）"
            );
        }
    }
}

#[derive(Clone)]
pub struct ConflictGuardService {
    db: DatabaseConnection,
}

impl ConflictGuardService {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 校验目标资源的 version 是否匹配 `expected_version`，匹配则原子地 +1 并返回 ConflictGuard。
    ///
    /// `expected_version=None` 时跳过校验，直接 bump（"最后写入胜出"语义，用于幂等操作）。
    ///
    /// 失败返回 `AppError::Conflict`，调用方应把错误码作为 `CONFLICT` 返回给 AI。
    pub async fn check_and_bump(
        &self,
        target: OperationTarget,
        target_id: &str,
        expected_version: Option<i32>,
    ) -> AppResult<ConflictGuard> {
        let table = table_for(target)?;
        // 一次 UPDATE 完成"校验 + bump"，原子性由 SQLite 行锁保证
        // WHERE version = ? AND id = ? 命中则 version += 1；不命中说明 version 不匹配或记录不存在
        let sql = if expected_version.is_some() {
            format!("UPDATE {table} SET version = version + 1 WHERE id = ? AND version = ?")
        } else {
            format!("UPDATE {table} SET version = version + 1 WHERE id = ?")
        };
        let transaction = self.db.begin().await?;
        let backend = transaction.get_database_backend();
        let result = if let Some(ev) = expected_version {
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    sql,
                    [target_id.into(), ev.into()],
                ))
                .await?
        } else {
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    sql,
                    [target_id.into()],
                ))
                .await?
        };

        if result.rows_affected() == 0 {
            // 没命中：可能是 version 不匹配，也可能是记录不存在
            let current = fetch_current_version(&transaction, target, target_id)
                .await?
                .ok_or_else(|| AppError::NotFound(format!("{target:?} id={target_id} 不存在")))?;
            transaction.rollback().await?;
            // 注意：fetch_current_version 返回的是 bump 之前的值，即"当前版本"
            return Err(AppError::Conflict(format!(
                "{target:?} id={target_id} 版本冲突：期望 {expected_version:?}，实际 {current}"
            )));
        }

        // 命中：取 bump 后的 version 作为 new_version 返回
        let new_version = fetch_current_version(&transaction, target, target_id)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("{target:?} id={target_id} 不存在")))?;
        transaction.commit().await?;

        Ok(ConflictGuard {
            db: self.db.clone(),
            target,
            target_id: target_id.to_string(),
            new_version,
            committed: false,
        })
    }

    /// 读取目标资源当前的 version（不修改）。供 `dl status` 等只读场景返回版本号给调用方。
    #[allow(dead_code)]
    pub async fn current_version(
        &self,
        target: OperationTarget,
        target_id: &str,
    ) -> AppResult<Option<i32>> {
        self.fetch_current_version(target, target_id).await
    }

    #[allow(dead_code)]
    async fn fetch_current_version(
        &self,
        target: OperationTarget,
        target_id: &str,
    ) -> AppResult<Option<i32>> {
        fetch_current_version(&self.db, target, target_id).await
    }
}

async fn fetch_current_version<C: ConnectionTrait>(
    db: &C,
    target: OperationTarget,
    target_id: &str,
) -> AppResult<Option<i32>> {
    let table = table_for(target)?;
    let sql = format!("SELECT version FROM {table} WHERE id = ?");
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            sql,
            [target_id.into()],
        ))
        .await?;
    match row {
        Some(row) => {
            let v: i32 = row
                .try_get("", "version")
                .map_err(|e| AppError::Internal(format!("读取 version 失败: {e}")))?;
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

/// 把 OperationTarget 映射到表名。新增 target 类型时需同步更新。
///
/// Settings（KV 表，无 version 列且主键是 key）/ Cookie、Session（auth_sessions
/// 无 version 列）不支持乐观锁：显式报错而不是拼出 `no such column` 的运行时地雷。
fn table_for(target: OperationTarget) -> AppResult<&'static str> {
    match target {
        OperationTarget::Task => Ok("download_tasks"),
        OperationTarget::Blogger => Ok("bloggers"),
        OperationTarget::History => Ok("history"),
        OperationTarget::LiveSource => Ok("live_sources"),
        OperationTarget::Settings | OperationTarget::Cookie | OperationTarget::Session => {
            Err(AppError::BadRequest(format!(
                "{target:?} 不支持乐观锁校验（对应表没有 version 列），请省略 expected_version"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, DatabaseConnection};
    use sea_orm_migration::MigratorTrait;
    use std::sync::Arc;

    async fn setup_db_with_task() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::migration::Migrator::up(&db, None).await.unwrap();
        // 插入一条 download_task，version 默认 0
        db.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO download_tasks (bvid, quality, type, status, stage) VALUES (?, ?, ?, ?, ?)",
            ["BV1test".into(), 0.into(), "video".into(), "pending".into(), "queued".into()],
        ))
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn check_and_bump_with_matching_version_succeeds() {
        let db = setup_db_with_task().await;
        let service = ConflictGuardService::new(db.clone());
        let guard = service
            .check_and_bump(OperationTarget::Task, "1", Some(0))
            .await
            .expect("version 0 should match");
        assert_eq!(guard.new_version(), 1);
        guard.commit();

        // DB 中 version 应为 1
        let v = service
            .current_version(OperationTarget::Task, "1")
            .await
            .unwrap();
        assert_eq!(v, Some(1));
    }

    #[tokio::test]
    async fn check_and_bump_with_mismatched_version_returns_conflict() {
        let db = setup_db_with_task().await;
        let service = ConflictGuardService::new(db);
        let result = service
            .check_and_bump(OperationTarget::Task, "1", Some(99))
            .await;
        assert!(matches!(result, Err(AppError::Conflict(_))));
    }

    #[tokio::test]
    async fn check_and_bump_without_expected_version_always_succeeds() {
        let db = setup_db_with_task().await;
        let service = ConflictGuardService::new(db.clone());
        let guard = service
            .check_and_bump(OperationTarget::Task, "1", None)
            .await
            .expect("None should skip version check");
        assert_eq!(guard.new_version(), 1);
        guard.commit();
    }

    #[tokio::test]
    async fn rollback_decrements_version() {
        let db = setup_db_with_task().await;
        let service = ConflictGuardService::new(db.clone());
        let guard = service
            .check_and_bump(OperationTarget::Task, "1", Some(0))
            .await
            .unwrap();
        guard.rollback().await.unwrap();
        // version 应该回到 0
        let v = service
            .current_version(OperationTarget::Task, "1")
            .await
            .unwrap();
        assert_eq!(v, Some(0));
    }

    #[tokio::test]
    async fn unsupported_targets_return_explicit_error() {
        let db = setup_db_with_task().await;
        let service = ConflictGuardService::new(db);
        // Settings / Cookie / Session 的表没有 version 列，必须显式拒绝而不是拼 SQL 炸 no such column。
        for target in [
            OperationTarget::Settings,
            OperationTarget::Cookie,
            OperationTarget::Session,
        ] {
            let err = match service.check_and_bump(target, "1", Some(0)).await {
                Err(err) => err,
                Ok(_) => panic!("{target:?} 应显式拒绝乐观锁校验"),
            };
            assert!(
                matches!(err, AppError::BadRequest(ref m) if m.contains("不支持乐观锁")),
                "unexpected error for {target:?}: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_bumps_only_one_succeeds() {
        let db = setup_db_with_task().await;
        let service = Arc::new(ConflictGuardService::new(db.clone()));
        let s1 = service.clone();
        let s2 = service.clone();
        let h1 =
            tokio::spawn(
                async move { s1.check_and_bump(OperationTarget::Task, "1", Some(0)).await },
            );
        let h2 =
            tokio::spawn(
                async move { s2.check_and_bump(OperationTarget::Task, "1", Some(0)).await },
            );
        let r1 = h1.await.unwrap();
        let r2 = h2.await.unwrap();
        // 一个成功一个冲突
        assert!(r1.is_ok() || r2.is_ok(), "至少一个应该成功");
        assert!(
            r1.is_err() || r2.is_err(),
            "至少一个应该冲突（version 已被对方 bump）"
        );
        // 成功的那个提交
        if let Ok(g) = r1 {
            g.commit();
        }
        if let Ok(g) = r2 {
            g.commit();
        }
        // 最终 version 应该是 1（只允许一个 bump 成功）
        let v = service
            .current_version(OperationTarget::Task, "1")
            .await
            .unwrap();
        assert_eq!(v, Some(1));
    }
}
