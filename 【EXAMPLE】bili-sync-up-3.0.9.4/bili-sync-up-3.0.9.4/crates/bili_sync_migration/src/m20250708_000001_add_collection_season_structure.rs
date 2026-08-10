use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 插入新的配置项：collection_use_season_structure
        // 默认值为 false，保持向后兼容性
        let mut query = Query::insert()
            .into_table(ConfigItem::Table)
            .columns([ConfigItem::KeyName, ConfigItem::ValueJson])
            .to_owned();

        query.values_panic(["collection_use_season_structure".into(), "true".into()]);

        query.on_conflict(OnConflict::column(ConfigItem::KeyName).do_nothing().to_owned());

        manager.exec_stmt(query).await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 删除配置项
        manager
            .exec_stmt(
                Query::delete()
                    .from_table(ConfigItem::Table)
                    .and_where(Expr::col(ConfigItem::KeyName).eq("collection_use_season_structure"))
                    .to_owned(),
            )
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
}
