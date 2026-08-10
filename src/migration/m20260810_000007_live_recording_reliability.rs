use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Persist state required to distinguish recoverable recording transitions from
/// a plain failed row. Every addition has a SQLite default so existing history
/// remains readable after upgrade.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let columns = conn
            .query_all_raw(Statement::from_string(
                manager.get_database_backend(),
                "PRAGMA table_info(live_recordings)".to_owned(),
            ))
            .await?
            .iter()
            .filter_map(|row| row.try_get::<String>("", "name").ok())
            .collect::<std::collections::HashSet<_>>();
        for (name, definition) in [
            ("stop_reason", "TEXT NULL"),
            ("segment_index", "INTEGER NOT NULL DEFAULT 0"),
            ("restart_attempts", "INTEGER NOT NULL DEFAULT 0"),
            ("checkpointed_at", "TEXT NULL"),
            ("is_recoverable", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !columns.contains(name) {
                conn.execute_unprepared(&format!(
                    "ALTER TABLE live_recordings ADD COLUMN {name} {definition}"
                ))
                .await?;
            }
        }

        let source_columns = conn
            .query_all_raw(Statement::from_string(
                manager.get_database_backend(),
                "PRAGMA table_info(live_sources)".to_owned(),
            ))
            .await?
            .iter()
            .filter_map(|row| row.try_get::<String>("", "name").ok())
            .collect::<std::collections::HashSet<_>>();
        if !source_columns.contains("manual_stop_session_key") {
            conn.execute_unprepared(
                "ALTER TABLE live_sources ADD COLUMN manual_stop_session_key TEXT NULL",
            )
            .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "live recording reliability migration is irreversible".into(),
        ))
    }
}
