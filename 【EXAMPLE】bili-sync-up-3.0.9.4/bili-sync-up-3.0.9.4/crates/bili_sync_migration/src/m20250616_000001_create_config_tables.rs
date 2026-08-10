use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 创建配置项表
        manager
            .create_table(
                Table::create()
                    .table(ConfigItem::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ConfigItem::KeyName)
                            .string_len(100)
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ConfigItem::ValueJson).text().not_null())
                    .col(
                        ColumnDef::new(ConfigItem::UpdatedAt)
                            .timestamp()
                            .default(Expr::current_timestamp())
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // 创建配置变更历史表（可选，用于审计）
        manager
            .create_table(
                Table::create()
                    .table(ConfigChange::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ConfigChange::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ConfigChange::KeyName).string_len(100).not_null())
                    .col(ColumnDef::new(ConfigChange::OldValue).text())
                    .col(ColumnDef::new(ConfigChange::NewValue).text().not_null())
                    .col(
                        ColumnDef::new(ConfigChange::ChangedAt)
                            .timestamp()
                            .default(Expr::current_timestamp())
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // 创建索引优化查询性能
        manager
            .create_index(
                Index::create()
                    .table(ConfigChange::Table)
                    .name("idx_config_change_key_time")
                    .col(ConfigChange::KeyName)
                    .col(ConfigChange::ChangedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ConfigChange::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ConfigItem::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum ConfigItem {
    #[sea_orm(iden = "config_items")]
    Table,
    KeyName,
    ValueJson,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum ConfigChange {
    #[sea_orm(iden = "config_changes")]
    Table,
    Id,
    KeyName,
    OldValue,
    NewValue,
    ChangedAt,
}
