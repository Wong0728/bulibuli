use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE TABLE IF NOT EXISTS live_recording_segments (
                    id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
                    recording_id INTEGER NOT NULL,
                    segment_index INTEGER NOT NULL,
                    path TEXT NOT NULL,
                    started_at TEXT NULL,
                    ended_at TEXT NULL,
                    file_size INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL DEFAULT 'open',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(recording_id, segment_index)
                )"#,
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE TABLE IF NOT EXISTS live_merge_jobs (
                    id TEXT PRIMARY KEY NOT NULL,
                    recording_id INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'queued',
                    progress INTEGER NOT NULL DEFAULT 0,
                    error_msg TEXT NULL,
                    source_segment_count INTEGER NOT NULL DEFAULT 0,
                    cancel_requested INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "live merge job migration is irreversible".into(),
        ))
    }
}
