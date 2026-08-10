use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 为video表添加auto_download字段
        // TRUE: 自动下载（默认行为，保持向后兼容）
        // FALSE: 仅获取信息，不自动下载
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .add_column(
                        ColumnDef::new(Video::AutoDownload).boolean().not_null().default(true), // 默认为true，保持现有行为
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 回滚：删除auto_download字段
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .drop_column(Video::AutoDownload)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Video {
    Table,
    AutoDownload,
}
