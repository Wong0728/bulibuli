use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // FTS5 is optional. Existing databases need a rebuild, but a build without
        // FTS5 must continue to use the LIKE fallback in HistoryService.
        // rebuild 失败（如 SQLite 构建未编译 FTS5）按可选特性降级，不阻断迁移。
        manager
            .get_connection()
            .execute_unprepared("INSERT INTO history_fts(history_fts) VALUES('rebuild')")
            .await
            .ok();
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "history FTS rebuild cannot be rolled back".to_string(),
        ))
    }
}
