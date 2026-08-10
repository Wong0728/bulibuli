use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE TABLE IF NOT EXISTS live_sources (
                    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
                    room_id INTEGER NOT NULL UNIQUE,
                    short_id INTEGER NOT NULL DEFAULT 0,
                    uid INTEGER NOT NULL DEFAULT 0,
                    anchor_name TEXT NOT NULL DEFAULT '',
                    face TEXT NOT NULL DEFAULT '',
                    title TEXT NOT NULL DEFAULT '',
                    cover TEXT NOT NULL DEFAULT '',
                    auto_record_enabled INTEGER NOT NULL DEFAULT 0,
                    weekly_schedule TEXT NULL,
                    capture_mode TEXT NOT NULL DEFAULT 'standard',
                    manual_stop_latched INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )"#,
            )
            .await?;

        let rows = manager
            .get_connection()
            .query_all_raw(Statement::from_string(
                manager.get_database_backend(),
                "PRAGMA table_info(live_recordings)".to_owned(),
            ))
            .await?;
        let existing = rows
            .iter()
            .filter_map(|row| row.try_get::<String>("", "name").ok())
            .collect::<std::collections::HashSet<_>>();
        let additions = [
            ("trigger", "TEXT NOT NULL DEFAULT 'manual'"),
            ("event_path", "TEXT NULL"),
            ("xml_path", "TEXT NULL"),
            ("summary_path", "TEXT NULL"),
            ("capture_mode", "TEXT NOT NULL DEFAULT 'standard'"),
            ("interaction_status", "TEXT NOT NULL DEFAULT 'off'"),
            ("interaction_error", "TEXT NULL"),
            ("danmaku_count", "INTEGER NOT NULL DEFAULT 0"),
            ("unique_user_count", "INTEGER NOT NULL DEFAULT 0"),
            ("free_gift_count", "INTEGER NOT NULL DEFAULT 0"),
            ("paid_gift_count", "INTEGER NOT NULL DEFAULT 0"),
            ("sc_count", "INTEGER NOT NULL DEFAULT 0"),
            ("guard_count", "INTEGER NOT NULL DEFAULT 0"),
            ("peak_watched", "INTEGER NOT NULL DEFAULT 0"),
            ("dropped_event_count", "INTEGER NOT NULL DEFAULT 0"),
            ("estimated_paid_value", "REAL NOT NULL DEFAULT 0"),
        ];
        for (name, definition) in additions {
            if !existing.contains(name) {
                manager
                    .get_connection()
                    .execute_unprepared(&format!(
                        "ALTER TABLE live_recordings ADD COLUMN {name} {definition}"
                    ))
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "live interaction archive migration cannot be safely removed".to_owned(),
        ))
    }
}
