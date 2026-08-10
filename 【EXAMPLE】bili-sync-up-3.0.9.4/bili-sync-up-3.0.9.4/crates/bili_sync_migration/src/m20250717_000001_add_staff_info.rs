use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 为video表添加staff_info字段，存储视频创作团队信息
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .add_column(
                        ColumnDef::new(Video::StaffInfo)
                            .json()
                            .null() // 允许为空，向后兼容
                            .comment("视频创作团队信息，JSON格式"),
                    )
                    .to_owned(),
            )
            .await?;

        // 为video表添加source_submission_id字段，记录视频来源于哪个UP主订阅
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .add_column(
                        ColumnDef::new(Video::SourceSubmissionId)
                            .integer()
                            .null() // 允许为空，向后兼容
                            .comment("视频来源的UP主订阅ID"),
                    )
                    .to_owned(),
            )
            .await?;

        // 添加索引以提高查询性能
        manager
            .create_index(
                Index::create()
                    .name("idx_video_source_submission_id")
                    .table(Video::Table)
                    .col(Video::SourceSubmissionId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 删除索引
        manager
            .drop_index(Index::drop().name("idx_video_source_submission_id").to_owned())
            .await?;

        // 移除source_submission_id字段
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .drop_column(Video::SourceSubmissionId)
                    .to_owned(),
            )
            .await?;

        // 移除staff_info字段
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .drop_column(Video::StaffInfo)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Video {
    Table,
    StaffInfo,
    SourceSubmissionId,
}
