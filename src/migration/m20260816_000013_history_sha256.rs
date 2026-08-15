use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

/// 为本地文件完整性校验和去重增加 SHA-256，同时保留旧 MD5 列供兼容读取。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        for (name, sql) in [
            ("sha256", "ALTER TABLE history ADD COLUMN sha256 TEXT"),
            (
                "sha256_last_checked_at",
                "ALTER TABLE history ADD COLUMN sha256_last_checked_at DATETIME",
            ),
        ] {
            let columns = connection
                .query_all_raw(Statement::from_string(
                    manager.get_database_backend(),
                    "PRAGMA table_info(history)".to_owned(),
                ))
                .await?;
            if !columns.iter().any(|row| {
                row.try_get::<String>("", "name")
                    .map(|column| column == name)
                    .unwrap_or(false)
            }) {
                connection.execute_unprepared(sql).await?;
            }
        }
        connection
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_history_sha256_checked \
                 ON history(sha256_last_checked_at)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "history SHA-256 columns cannot be safely removed".to_owned(),
        ))
    }
}
