use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for (name, sql) in [
            ("cid", "ALTER TABLE history ADD COLUMN cid BIGINT"),
            ("page", "ALTER TABLE history ADD COLUMN page INTEGER"),
            (
                "part_title",
                "ALTER TABLE history ADD COLUMN part_title TEXT",
            ),
        ] {
            let columns = conn
                .query_all_raw(sea_orm::Statement::from_string(
                    conn.get_database_backend(),
                    "PRAGMA table_info(history)".to_string(),
                ))
                .await?;
            if !columns
                .iter()
                .any(|row| row.try_get::<String>("", "name").unwrap_or_default() == name)
            {
                conn.execute_unprepared(sql).await?;
            }
        }
        conn.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS uix_history_bvid_cid_source \
             ON history(bvid, COALESCE(cid, -1), source)",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "history multi-page migration cannot be rolled back".to_string(),
        ))
    }
}
