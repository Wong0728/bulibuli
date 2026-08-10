use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // 检查并修复 favorite 表的兼容性问题
        // 1. 检查 latest_row_at 列是否存在
        let has_latest_row_at_result = db
            .execute_unprepared(
                "SELECT COUNT(*) as count FROM pragma_table_info('favorite') WHERE name = 'latest_row_at'",
            )
            .await;

        // 如果查询失败或者返回结果表明列不存在，则添加该列
        match has_latest_row_at_result {
            Ok(_) => {
                // 尝试添加列，如果已存在会失败但不影响后续操作
                let _ = db
                    .execute_unprepared(
                        "ALTER TABLE favorite ADD COLUMN latest_row_at TIMESTAMP DEFAULT '1970-01-01 00:00:00'",
                    )
                    .await;
            }
            Err(_) => {
                // 表可能不存在，跳过
            }
        }

        // 2. 检查并修复其他表的兼容性
        let tables_to_check = vec![
            ("collection", "latest_row_at"),
            ("watch_later", "latest_row_at"),
            ("submission", "latest_row_at"),
        ];

        for (table_name, column_name) in tables_to_check {
            let _ = db
                .execute_unprepared(&format!(
                    "ALTER TABLE {} ADD COLUMN {} TIMESTAMP DEFAULT '1970-01-01 00:00:00'",
                    table_name, column_name
                ))
                .await;
        }

        // 3. 更新默认值（如果列刚被添加）
        let update_queries = vec![
            "UPDATE favorite SET latest_row_at = (SELECT IFNULL(MAX(favtime), '1970-01-01 00:00:00') FROM video WHERE favorite_id = favorite.id) WHERE latest_row_at = '1970-01-01 00:00:00'",
            "UPDATE collection SET latest_row_at = (SELECT IFNULL(MAX(pubtime), '1970-01-01 00:00:00') FROM video WHERE collection_id = collection.id) WHERE latest_row_at = '1970-01-01 00:00:00'",
            "UPDATE watch_later SET latest_row_at = (SELECT IFNULL(MAX(favtime), '1970-01-01 00:00:00') FROM video WHERE watch_later_id = watch_later.id) WHERE latest_row_at = '1970-01-01 00:00:00'",
            "UPDATE submission SET latest_row_at = (SELECT IFNULL(MAX(ctime), '1970-01-01 00:00:00') FROM video WHERE submission_id = submission.id) WHERE latest_row_at = '1970-01-01 00:00:00'",
        ];

        for query in update_queries {
            let _ = db.execute_unprepared(query).await;
        }

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // 兼容性迁移通常不需要回滚
        // 因为它只是确保数据结构完整，不改变现有功能
        Ok(())
    }
}
