//! 把 `live_recordings.trigger` 改名为 `record_trigger`。
//!
//! `trigger` 是 SQLite 关键字：虽然 SeaORM 会给标识符加引号所以一直能用，
//! 但任何手写 SQL 都得记得引号，属于埋雷命名。改名后 Rust 实体字段
//! `Model::trigger` 通过 `column_name` 绑定保持不变，API 序列化键也不变。

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const TABLE: &str = "live_recordings";
const OLD_COLUMN: &str = "trigger";
const NEW_COLUMN: &str = "record_trigger";

async fn column_exists(manager: &SchemaManager<'_>, name: &str) -> Result<bool, DbErr> {
    let rows = manager
        .get_connection()
        .query_all_raw(Statement::from_string(
            manager.get_database_backend(),
            format!("PRAGMA table_info({TABLE})"),
        ))
        .await?;
    Ok(rows
        .iter()
        .filter_map(|row| row.try_get::<String>("", "name").ok())
        .any(|column| column == name))
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, OLD_COLUMN).await? {
            manager
                .get_connection()
                .execute_unprepared(
                    format!("ALTER TABLE {TABLE} RENAME COLUMN {OLD_COLUMN} TO {NEW_COLUMN}")
                        .as_str(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if column_exists(manager, NEW_COLUMN).await? {
            manager
                .get_connection()
                .execute_unprepared(
                    format!("ALTER TABLE {TABLE} RENAME COLUMN {NEW_COLUMN} TO {OLD_COLUMN}")
                        .as_str(),
                )
                .await?;
        }
        Ok(())
    }
}
