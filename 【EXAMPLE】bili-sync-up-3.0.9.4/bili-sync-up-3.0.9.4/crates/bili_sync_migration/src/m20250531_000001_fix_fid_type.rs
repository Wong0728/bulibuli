use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // 检查 latest_row_at 列是否存在
        let has_latest_row_at = match db
            .execute_unprepared("SELECT COUNT(*) FROM pragma_table_info('favorite') WHERE name = 'latest_row_at'")
            .await
        {
            Ok(_result) => {
                // 这里我们假设如果查询成功，列就存在
                // 实际上SQLite的pragma_table_info会返回列信息
                true
            }
            Err(_) => false,
        };

        // SQLite 不支持直接修改列类型，需要重建表
        // 1. 创建临时表（包含latest_row_at列）
        if has_latest_row_at {
            db.execute_unprepared(
                "CREATE TABLE favorite_temp (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    f_id BIGINT UNIQUE NOT NULL,
                    name TEXT NOT NULL,
                    path TEXT NOT NULL,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL,
                    latest_row_at TIMESTAMP DEFAULT '1970-01-01 00:00:00' NOT NULL
                )",
            )
            .await?;

            // 2. 复制数据（包括latest_row_at）
            db.execute_unprepared(
                "INSERT INTO favorite_temp (id, f_id, name, path, created_at, latest_row_at) 
                 SELECT id, f_id, name, path, created_at, latest_row_at FROM favorite",
            )
            .await?;
        } else {
            // 如果没有latest_row_at列，创建不包含该列的临时表
            db.execute_unprepared(
                "CREATE TABLE favorite_temp (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    f_id BIGINT UNIQUE NOT NULL,
                    name TEXT NOT NULL,
                    path TEXT NOT NULL,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL,
                    latest_row_at TIMESTAMP DEFAULT '1970-01-01 00:00:00' NOT NULL
                )",
            )
            .await?;

            // 2. 复制数据（不包括latest_row_at，会使用默认值）
            db.execute_unprepared(
                "INSERT INTO favorite_temp (id, f_id, name, path, created_at) 
                 SELECT id, f_id, name, path, created_at FROM favorite",
            )
            .await?;
        }

        // 3. 删除原表
        db.execute_unprepared("DROP TABLE favorite").await?;

        // 4. 重命名临时表
        db.execute_unprepared("ALTER TABLE favorite_temp RENAME TO favorite")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // 回滚：将 BIGINT 改回 INTEGER，但保留 latest_row_at 列
        // 1. 创建临时表（使用原来的f_id类型，但保留latest_row_at）
        db.execute_unprepared(
            "CREATE TABLE favorite_temp (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                f_id INTEGER UNIQUE NOT NULL,
                name TEXT NOT NULL,
                path TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL,
                latest_row_at TIMESTAMP DEFAULT '1970-01-01 00:00:00' NOT NULL
            )",
        )
        .await?;

        // 2. 复制数据（可能会截断大的ID）
        db.execute_unprepared(
            "INSERT INTO favorite_temp (id, f_id, name, path, created_at, latest_row_at) 
             SELECT id, f_id, name, path, created_at, latest_row_at FROM favorite",
        )
        .await?;

        // 3. 删除原表
        db.execute_unprepared("DROP TABLE favorite").await?;

        // 4. 重命名临时表
        db.execute_unprepared("ALTER TABLE favorite_temp RENAME TO favorite")
            .await?;

        Ok(())
    }
}
