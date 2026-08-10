use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 为 video_source 表的 type 字段添加索引
        manager
            .create_index(
                Index::create()
                    .name("idx_video_source_type")
                    .table(VideoSource::Table)
                    .col(VideoSource::Type)
                    .to_owned(),
            )
            .await?;

        // 为 video_source 表的 latest_row_at 字段添加索引（用于排序查询）
        manager
            .create_index(
                Index::create()
                    .name("idx_video_source_latest_row_at")
                    .table(VideoSource::Table)
                    .col(VideoSource::LatestRowAt)
                    .to_owned(),
            )
            .await?;

        // 为 video 表的 bvid 字段添加索引（频繁查询字段）
        manager
            .create_index(
                Index::create()
                    .name("idx_video_bvid")
                    .table(Video::Table)
                    .col(Video::Bvid)
                    .to_owned(),
            )
            .await?;

        // 为 page 表的 video_id 字段添加索引（外键查询）
        manager
            .create_index(
                Index::create()
                    .name("idx_page_video_id")
                    .table(Page::Table)
                    .col(Page::VideoId)
                    .to_owned(),
            )
            .await?;

        // 为 favorite 表添加复合索引
        manager
            .create_index(
                Index::create()
                    .name("idx_favorite_fid_path")
                    .table(Favorite::Table)
                    .col(Favorite::FId)
                    .col(Favorite::Path)
                    .to_owned(),
            )
            .await?;

        // 为 collection 表添加复合索引
        manager
            .create_index(
                Index::create()
                    .name("idx_collection_mid_sid")
                    .table(Collection::Table)
                    .col(Collection::MId)
                    .col(Collection::SId)
                    .to_owned(),
            )
            .await?;

        // 为 submission 表的 upper_id 添加索引
        manager
            .create_index(
                Index::create()
                    .name("idx_submission_upper_id")
                    .table(Submission::Table)
                    .col(Submission::UpperId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(Index::drop().name("idx_video_source_type").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_video_source_latest_row_at").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_video_bvid").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_page_video_id").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_favorite_fid_path").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_collection_mid_sid").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_submission_upper_id").to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum VideoSource {
    Table,
    Type,
    LatestRowAt,
}

#[derive(DeriveIden)]
enum Video {
    Table,
    Bvid,
}

#[derive(DeriveIden)]
enum Page {
    Table,
    VideoId,
}

#[derive(DeriveIden)]
enum Favorite {
    Table,
    FId,
    Path,
}

#[derive(DeriveIden)]
enum Collection {
    Table,
    MId,
    SId,
}

#[derive(DeriveIden)]
enum Submission {
    Table,
    UpperId,
}
