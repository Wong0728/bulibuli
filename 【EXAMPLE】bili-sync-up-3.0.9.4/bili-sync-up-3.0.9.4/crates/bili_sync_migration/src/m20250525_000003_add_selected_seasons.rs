use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 为 video_source 表添加 selected_seasons 字段，用于存储选中的季度ID列表
        manager
            .alter_table(
                Table::alter()
                    .table(VideoSource::Table)
                    .add_column(
                        ColumnDef::new(VideoSource::SelectedSeasons)
                            .text()
                            .null()
                            .comment("选中的季度ID列表，JSON格式"),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(VideoSource::Table)
                    .drop_column(VideoSource::SelectedSeasons)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum VideoSource {
    Table,
    SelectedSeasons,
}
