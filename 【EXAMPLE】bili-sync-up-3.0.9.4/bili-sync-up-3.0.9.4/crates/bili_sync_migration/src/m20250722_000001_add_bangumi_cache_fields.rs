use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 为video_source表添加cached_episodes字段，存储番剧剧集缓存数据
        manager
            .alter_table(
                Table::alter()
                    .table(VideoSource::Table)
                    .add_column(
                        ColumnDef::new(VideoSource::CachedEpisodes)
                            .text()
                            .null() // 允许为空，向后兼容
                            .comment("番剧剧集缓存数据，JSON格式"),
                    )
                    .to_owned(),
            )
            .await?;

        // 为video_source表添加cache_updated_at字段，记录缓存更新时间
        manager
            .alter_table(
                Table::alter()
                    .table(VideoSource::Table)
                    .add_column(
                        ColumnDef::new(VideoSource::CacheUpdatedAt)
                            .timestamp()
                            .null() // 允许为空，向后兼容
                            .comment("缓存最后更新时间"),
                    )
                    .to_owned(),
            )
            .await?;

        // 添加索引以提高缓存查询性能
        manager
            .create_index(
                Index::create()
                    .name("idx_video_source_cache_updated_at")
                    .table(VideoSource::Table)
                    .col(VideoSource::CacheUpdatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 删除索引
        manager
            .drop_index(Index::drop().name("idx_video_source_cache_updated_at").to_owned())
            .await?;

        // 移除cache_updated_at字段
        manager
            .alter_table(
                Table::alter()
                    .table(VideoSource::Table)
                    .drop_column(VideoSource::CacheUpdatedAt)
                    .to_owned(),
            )
            .await?;

        // 移除cached_episodes字段
        manager
            .alter_table(
                Table::alter()
                    .table(VideoSource::Table)
                    .drop_column(VideoSource::CachedEpisodes)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum VideoSource {
    Table,
    CachedEpisodes,
    CacheUpdatedAt,
}
