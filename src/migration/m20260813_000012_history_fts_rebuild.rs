use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // FTS5 is optional. Existing databases need a rebuild, but a build without
        // FTS5 must continue to use the LIKE fallback in HistoryService.
        let _ = manager
            .get_connection()
            .execute_unprepared("INSERT INTO history_fts(history_fts) VALUES('rebuild')")
            .await;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "history FTS rebuild cannot be rolled back".to_string(),
        ))
    }
}
