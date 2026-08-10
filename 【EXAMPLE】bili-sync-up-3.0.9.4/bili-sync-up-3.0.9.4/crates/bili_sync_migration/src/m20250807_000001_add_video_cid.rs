use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 添加cid列到video表
        manager
            .alter_table(
                Table::alter()
                    .table(Video::Table)
                    .add_column(ColumnDef::new(Video::Cid).big_integer())
                    .to_owned(),
            )
            .await?;

        // 不在迁移中填充数据，因为需要重新调用API获取
        // 数据将在后续的扫描过程中自动填充

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(Table::alter().table(Video::Table).drop_column(Video::Cid).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Video {
    Table,
    Cid,
}
