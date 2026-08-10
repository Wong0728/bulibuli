use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 删除旧的唯一索引
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX idx_video_unique")
            .await?;

        // 创建新的包含 ep_id 的唯一索引，使用原生SQL确保NULL值处理正确
        // 这解决了番剧同一BV号不同集数的插入问题
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX idx_video_unique ON video (
                    ifnull(collection_id, -1),
                    ifnull(favorite_id, -1),
                    ifnull(watch_later_id, -1),
                    ifnull(submission_id, -1),
                    ifnull(source_id, -1),
                    bvid,
                    ifnull(ep_id, '')
                )",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 删除新索引
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX idx_video_unique")
            .await?;

        // 恢复旧的不包含 ep_id 的唯一索引
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE UNIQUE INDEX idx_video_unique ON video (
                    ifnull(collection_id, -1),
                    ifnull(favorite_id, -1),
                    ifnull(watch_later_id, -1),
                    ifnull(submission_id, -1),
                    ifnull(source_id, -1),
                    bvid
                )",
            )
            .await?;

        Ok(())
    }
}
