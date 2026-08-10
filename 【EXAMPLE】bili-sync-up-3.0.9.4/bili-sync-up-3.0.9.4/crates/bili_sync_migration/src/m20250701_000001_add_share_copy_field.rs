use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 添加 share_copy 字段到 video 表
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .add_column(ColumnDef::new(Video::ShareCopy).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 删除 share_copy 字段
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .drop_column(Video::ShareCopy)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Video {
    Table,
    ShareCopy,
}
