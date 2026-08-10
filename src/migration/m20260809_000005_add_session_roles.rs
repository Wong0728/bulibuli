use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

/// Adds RBAC to existing installations.  Existing paired devices intentionally
/// become owners so an upgrade cannot lock the administrator out.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite has no portable `ADD COLUMN IF NOT EXISTS`.  `table_info` is
        // stable across the supported SQLite versions and keeps this migration
        // idempotent for databases created by a development build.
        let rows = manager
            .get_connection()
            .query_all_raw(Statement::from_string(
                manager.get_database_backend(),
                "PRAGMA table_info(auth_sessions)".to_owned(),
            ))
            .await?;
        let exists = rows.iter().any(|row| {
            row.try_get::<String>("", "name")
                .map(|name| name == "role")
                .unwrap_or(false)
        });
        if !exists {
            manager
                .get_connection()
                .execute_unprepared(
                    "ALTER TABLE auth_sessions ADD COLUMN role TEXT NOT NULL DEFAULT 'owner'",
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "session roles cannot be safely removed".to_owned(),
        ))
    }
}
