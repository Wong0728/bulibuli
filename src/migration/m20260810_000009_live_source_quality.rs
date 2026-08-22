use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 与同族迁移一致：ADD COLUMN 前先做存在性守卫，保证迁移幂等可重放。
        let conn = manager.get_connection();
        let columns = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "PRAGMA table_info(live_sources)".to_owned(),
            ))
            .await?;
        let exists = columns
            .iter()
            .any(|row| row.try_get::<String>("", "name").unwrap_or_default() == "max_qn");
        if !exists {
            conn.execute_unprepared(
                r#"ALTER TABLE live_sources ADD COLUMN max_qn INTEGER NOT NULL DEFAULT 10000"#,
            )
            .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "live source quality migration is irreversible".into(),
        ))
    }
}
