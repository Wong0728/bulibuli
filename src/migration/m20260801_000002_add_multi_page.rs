use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// 多P（分P）支持：为 download_tasks 增加 cid/page/part_title 列，
/// 并将唯一去重键从 (bvid, type) 调整为 (bvid, COALESCE(cid,-1), type)，
/// 以兼容单P任务 cid 为 NULL 的存量数据（SQLite 唯一索引默认 NULL distinct，
/// 若直接用 (bvid, cid, type) 会导致单P任务去重失效，故用 COALESCE 表达式索引）。
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        // --- 1. 新增列（SQLite 的 ALTER TABLE ADD COLUMN 每次仅能加一列，且不能带非常量默认值） ---
        // 存在性守卫（对齐 003/006/007/011/013）：旧库若已含该列（部分升级/手工补丁）直接跳过，
        // 否则 ALTER 会报 duplicate column name 导致整个迁移失败、应用无法启动。
        for (name, sql) in [
            ("cid", "ALTER TABLE download_tasks ADD COLUMN cid BIGINT"),
            ("page", "ALTER TABLE download_tasks ADD COLUMN page INTEGER"),
            (
                "part_title",
                "ALTER TABLE download_tasks ADD COLUMN part_title TEXT",
            ),
        ] {
            let columns = conn
                .query_all_raw(sea_orm::Statement::from_string(
                    conn.get_database_backend(),
                    "PRAGMA table_info(download_tasks)".to_string(),
                ))
                .await?;
            if !columns
                .iter()
                .any(|row| row.try_get::<String>("", "name").unwrap_or_default() == name)
            {
                conn.execute_unprepared(sql).await?;
            }
        }

        // --- 2. 去重键升级：先删旧唯一索引，再建 COALESCE 表达式唯一索引 ---
        conn.execute_unprepared("DROP INDEX IF EXISTS uix_bvid_type")
            .await?;
        conn.execute_unprepared(
            "CREATE UNIQUE INDEX IF NOT EXISTS uix_bvid_cid_type ON download_tasks(bvid, COALESCE(cid, -1), type)",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "add_multi_page migration cannot be rolled back".to_string(),
        ))
    }
}
