use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 为 video 表的常用查询条件添加复合索引
        // 用于 filter_unfilled_videos 查询: valid=true AND download_status=0 AND category IN (1,2) AND single_page IS NULL
        manager
            .create_index(
                Index::create()
                    .name("idx_video_unfilled_query")
                    .table(Video::Table)
                    .col(Video::Valid)
                    .col(Video::DownloadStatus)
                    .col(Video::Category)
                    .to_owned(),
            )
            .await?;

        // 为 video 表的下载状态查询添加复合索引
        // 用于 filter_unhandled_video_pages 查询: valid=true AND download_status<3 AND category IN (1,2) AND single_page IS NOT NULL
        manager
            .create_index(
                Index::create()
                    .name("idx_video_unhandled_query")
                    .table(Video::Table)
                    .col(Video::Valid)
                    .col(Video::DownloadStatus)
                    .col(Video::Category)
                    .col(Video::SinglePage)
                    .to_owned(),
            )
            .await?;

        // 为 video 表的删除标记添加索引（用于软删除功能）
        manager
            .create_index(
                Index::create()
                    .name("idx_video_deleted")
                    .table(Video::Table)
                    .col(Video::Deleted)
                    .to_owned(),
            )
            .await?;

        // 为 page 表的状态查询添加复合索引
        manager
            .create_index(
                Index::create()
                    .name("idx_page_status_query")
                    .table(Page::Table)
                    .col(Page::VideoId)
                    .col(Page::Status)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(Index::drop().name("idx_video_unfilled_query").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_video_unhandled_query").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_video_deleted").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_page_status_query").to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Video {
    Table,
    Valid,
    DownloadStatus,
    Category,
    SinglePage,
    Deleted,
}

#[derive(DeriveIden)]
enum Page {
    Table,
    VideoId,
    Status,
}
