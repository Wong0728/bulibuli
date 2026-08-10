use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// P2 多端冲突基础设施：
/// 1. 为 `download_tasks` / `bloggers` / `history` 增加 `version` 列（乐观锁版本号，每次状态变更 +1）
/// 2. 新增 `operation_log` 表（审计日志，记录 source / caller_id / 操作 / 结果 / 版本）
///
/// `version` 列采用 `INTEGER NOT NULL DEFAULT 0`：存量数据全部置 0，
/// 调用方首次操作时 `expected_version=0` 即可成功，等价于"首次写入无冲突"。
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        // ── 1. 乐观锁 version 列（SQLite ALTER TABLE 一次只能加一列）──
        for table in ["download_tasks", "bloggers", "history"] {
            // IF NOT EXISTS 在 SQLite 的 ALTER TABLE ADD COLUMN 不被支持，需要先查 PRAGMA
            let columns = conn
                .query_all_raw(Statement::from_string(
                    conn.get_database_backend(),
                    format!("PRAGMA table_info({table})"),
                ))
                .await?;
            let exists = columns
                .iter()
                .any(|row| row.try_get::<String>("", "name").unwrap_or_default() == "version");
            if !exists {
                conn.execute_unprepared(&format!(
                    "ALTER TABLE {table} ADD COLUMN version INTEGER NOT NULL DEFAULT 0"
                ))
                .await?;
            }
        }

        // ── 2. operation_log 审计表 ──
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS operation_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at TEXT NOT NULL,
                source TEXT NOT NULL,
                caller_id TEXT NOT NULL,
                route_or_command TEXT NOT NULL,
                target_type TEXT NOT NULL,
                target_id TEXT,
                action TEXT NOT NULL,
                expected_version INTEGER,
                new_version INTEGER,
                outcome TEXT NOT NULL,
                error_code TEXT,
                request_id TEXT NOT NULL,
                detail TEXT
            )",
        )
        .await?;

        // 审计查询索引：按时间倒序、按来源、按目标
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_operation_log_at ON operation_log(at DESC)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_operation_log_source_at ON operation_log(source, at DESC)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_operation_log_target ON operation_log(target_type, target_id, at DESC)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "add_version_and_operation_log migration cannot be rolled back".to_string(),
        ))
    }
}
