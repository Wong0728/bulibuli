use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// 合并后的初始 schema：用单个 `up()` 从零构建最终数据库状态，
/// 替代 11 个旧手写迁移（001–011）和 13 个增量 SeaORM 迁移。
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        // --- 1. bloggers ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS bloggers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                uid TEXT NOT NULL UNIQUE,
                name TEXT,
                min_interval INTEGER NOT NULL,
                max_interval INTEGER NOT NULL,
                is_running BOOLEAN NOT NULL DEFAULT 0,
                next_check DATETIME,
                created_at DATETIME,
                updated_at DATETIME,
                face TEXT,
                sign TEXT,
                level INTEGER,
                fans BIGINT,
                last_seen_name TEXT,
                last_seen_face TEXT,
                last_seen_at DATETIME,
                download_video BOOLEAN NOT NULL DEFAULT 1,
                download_danmaku BOOLEAN NOT NULL DEFAULT 1,
                download_comments BOOLEAN NOT NULL DEFAULT 1,
                download_cover BOOLEAN NOT NULL DEFAULT 1,
                burn_danmaku BOOLEAN NOT NULL DEFAULT 0,
                burn_subtitle BOOLEAN NOT NULL DEFAULT 0,
                series_filter_regex TEXT,
                active_windows TEXT,
                is_saved BOOLEAN NOT NULL DEFAULT 1,
                has_auto_task BOOLEAN NOT NULL DEFAULT 1
            )",
        )
        .await?;

        // --- 2. history ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                uid TEXT,
                bvid TEXT NOT NULL,
                cid BIGINT,
                page INTEGER,
                part_title TEXT,
                source TEXT NOT NULL DEFAULT 'auto',
                title TEXT,
                pub_date TEXT,
                pub_timestamp BIGINT,
                download_time DATETIME,
                file_path TEXT,
                next_download_index INTEGER NOT NULL DEFAULT 0,
                pic TEXT,
                duration BIGINT,
                view BIGINT,
                state TEXT DEFAULT 'completed',
                cover_local_path TEXT,
                pay_note TEXT,
                reupload_of TEXT,
                md5 TEXT,
                md5_last_checked_at DATETIME,
                sha256 TEXT,
                sha256_last_checked_at DATETIME,
                view_refreshed_at DATETIME,
                view_source TEXT,
                burned_danmaku BOOLEAN DEFAULT 0,
                burned_subtitle BOOLEAN DEFAULT 0,
                owner_name TEXT,
                owner_face TEXT,
                auto_burn_status TEXT,
                auto_burn_attempts INTEGER NOT NULL DEFAULT 0,
                auto_burn_next_retry_at DATETIME,
                sidecar_attempts INTEGER NOT NULL DEFAULT 0,
                next_sidecar_at DATETIME
            )",
        )
        .await?;

        // --- 3. download_tasks ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS download_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                bvid TEXT NOT NULL,
                title TEXT,
                url TEXT,
                quality INTEGER NOT NULL DEFAULT 0,
                type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                error TEXT,
                progress_percent INTEGER NOT NULL DEFAULT 0,
                downloaded_size BIGINT NOT NULL DEFAULT 0,
                total_size BIGINT NOT NULL DEFAULT 0,
                speed BIGINT NOT NULL DEFAULT 0,
                filename TEXT,
                gid TEXT,
                original_url TEXT,
                download_dir TEXT,
                source TEXT,
                generation BIGINT NOT NULL DEFAULT 0,
                completion_triggered BOOLEAN NOT NULL DEFAULT 0,
                stage TEXT NOT NULL DEFAULT 'queued',
                priority INTEGER NOT NULL DEFAULT 100,
                attempts INTEGER NOT NULL DEFAULT 0,
                next_retry_at DATETIME,
                error_kind TEXT,
                selected_quality INTEGER,
                selected_codec TEXT,
                fallback_reason TEXT,
                face_url TEXT,
                created_at DATETIME,
                updated_at DATETIME
            )",
        )
        .await?;

        // --- 4. settings ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT,
                updated_at DATETIME
            )",
        )
        .await?;

        // --- 5. logs ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                level TEXT NOT NULL,
                message TEXT NOT NULL,
                uid TEXT,
                bvid TEXT,
                created_at DATETIME
            )",
        )
        .await?;

        // --- 6. auth_sessions ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS auth_sessions (
                id TEXT PRIMARY KEY NOT NULL,
                token_hash BLOB NOT NULL UNIQUE,
                previous_token_hash BLOB,
                previous_valid_until BIGINT,
                csrf_token TEXT NOT NULL,
                device_name TEXT NOT NULL,
                created_at BIGINT NOT NULL,
                expires_at BIGINT NOT NULL,
                absolute_expires_at BIGINT NOT NULL,
                last_used_at BIGINT NOT NULL,
                last_rotated_at BIGINT NOT NULL,
                last_ip TEXT NOT NULL,
                user_agent_summary TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'owner',
                revoked_at BIGINT
            )",
        )
        .await?;

        // --- 7. protected_secrets ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS protected_secrets (
                key TEXT PRIMARY KEY NOT NULL,
                value BLOB NOT NULL,
                updated_at BIGINT NOT NULL
            )",
        )
        .await?;

        // --- 8. security_meta ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS security_meta (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            )",
        )
        .await?;

        // --- 9. submission_checkpoints ---
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS submission_checkpoints (
                uid TEXT PRIMARY KEY NOT NULL,
                last_bvid TEXT,
                last_pub_timestamp INTEGER,
                last_success_at DATETIME,
                updated_at DATETIME NOT NULL
            )",
        )
        .await?;

        // --- 10. 索引 ---

        // bloggers 表。
        conn.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_blogger_uid ON bloggers(uid)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_bloggers_schedule ON bloggers(is_running, next_check)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_bloggers_saved ON bloggers(is_saved, id)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_bloggers_auto_task ON bloggers(has_auto_task, id)",
        )
        .await?;

        // history 表。
        conn.execute_unprepared("CREATE INDEX IF NOT EXISTS idx_history_uid ON history(uid)")
            .await?;
        conn.execute_unprepared("CREATE INDEX IF NOT EXISTS idx_history_bvid ON history(bvid)")
            .await?;
        conn.execute_unprepared("CREATE INDEX IF NOT EXISTS idx_history_state ON history(state)")
            .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_view_refreshed_at ON history(view_refreshed_at)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_uid_pub ON history(uid, pub_timestamp DESC)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_state_pub ON history(state, pub_timestamp DESC)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_pub ON history(pub_timestamp DESC)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_md5_checked ON history(md5_last_checked_at)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_sha256_checked ON history(sha256_last_checked_at)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_auto_burn_schedule ON history(auto_burn_status, auto_burn_next_retry_at)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_sidecar_schedule ON history(next_download_index, next_sidecar_at)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_history_source_sidecar ON history(source, next_download_index, next_sidecar_at)",
        )
        .await?;

        // logs 表。
        conn.execute_unprepared("CREATE INDEX IF NOT EXISTS idx_log_uid ON logs(uid)")
            .await?;
        conn.execute_unprepared("CREATE INDEX IF NOT EXISTS idx_logs_bvid ON logs(bvid)")
            .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_logs_uid_created ON logs(uid, created_at DESC)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_logs_bvid_created ON logs(bvid, created_at DESC)",
        )
        .await?;

        // download_tasks 表。
        conn.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS uix_bvid_type ON download_tasks(bvid, type)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_download_tasks_schedule ON download_tasks(status, priority DESC, next_retry_at, created_at)",
        )
        .await?;
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_download_tasks_type_status ON download_tasks(type, status)",
        )
        .await?;

        // auth_sessions 表。
        conn.execute_unprepared(
            "CREATE INDEX IF NOT EXISTS idx_auth_session_token ON auth_sessions(token_hash)",
        )
        .await?;

        // --- 11. history 的 FTS5 全文搜索 ---
        // 部分 SQLite 构建未编译 FTS5；失败时保留基础 history 表并降级运行。
        let fts_result = conn
            .execute_unprepared(
                "CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(title, bvid, uid, content='history', content_rowid='id')",
            )
            .await;
        if fts_result.is_ok() {
            let _ = conn
                .execute_unprepared("INSERT INTO history_fts(history_fts) VALUES('rebuild')")
                .await;
            let _ = conn.execute_unprepared(
                "CREATE TRIGGER IF NOT EXISTS history_fts_ai AFTER INSERT ON history BEGIN INSERT INTO history_fts(rowid, title, bvid, uid) VALUES (new.id, new.title, new.bvid, new.uid); END;",
            ).await;
            let _ = conn.execute_unprepared(
                "CREATE TRIGGER IF NOT EXISTS history_fts_au AFTER UPDATE ON history BEGIN INSERT INTO history_fts(history_fts, rowid, title, bvid, uid) VALUES('delete', old.id, old.title, old.bvid, old.uid); INSERT INTO history_fts(rowid, title, bvid, uid) VALUES (new.id, new.title, new.bvid, new.uid); END;",
            ).await;
            let _ = conn.execute_unprepared(
                "CREATE TRIGGER IF NOT EXISTS history_fts_ad AFTER DELETE ON history BEGIN INSERT INTO history_fts(history_fts, rowid, title, bvid, uid) VALUES('delete', old.id, old.title, old.bvid, old.uid); END;",
            ).await;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "initial schema migration cannot be rolled back".to_string(),
        ))
    }
}
