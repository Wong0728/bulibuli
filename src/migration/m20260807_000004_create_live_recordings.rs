use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// 新增 `live_recordings` 表：直播录制历史记录。
///
/// 每次录制（start → stop）产生一条记录，存储房间信息、输出路径、时长、文件大小等。
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS live_recordings (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                room_id     INTEGER NOT NULL,
                short_id    INTEGER NOT NULL DEFAULT 0,
                uid         INTEGER NOT NULL DEFAULT 0,
                title       TEXT NOT NULL DEFAULT '',
                cover       TEXT NOT NULL DEFAULT '',
                status      TEXT NOT NULL DEFAULT 'recording',
                output_path TEXT,
                danmu_path  TEXT,
                file_size   INTEGER NOT NULL DEFAULT 0,
                duration    INTEGER NOT NULL DEFAULT 0,
                started_at  TEXT NOT NULL,
                ended_at    TEXT,
                error_msg   TEXT,
                created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .await?;

        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_live_recordings_room_id ON live_recordings(room_id)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_live_recordings_status ON live_recordings(status)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "create_live_recordings migration cannot be rolled back".to_string(),
        ))
    }
}
