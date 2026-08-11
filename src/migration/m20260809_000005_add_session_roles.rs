use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

/// 为现有安装添加 RBAC。已配对设备会被设为 Owner，避免升级后管理员被锁在系统外。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite 没有可移植的 `ADD COLUMN IF NOT EXISTS`。`table_info` 在支持的 SQLite
        // 版本中保持稳定，可让此迁移对开发版本创建的数据库保持幂等。
        let rows = manager
            .get_connection()
            .query_all_raw(Statement::from_string(
                manager.get_database_backend(),
                "PRAGMA table_info(auth_sessions)".to_owned(),
            ))
            .await?;
        let exists = rows.iter().any(|row| {
            row.try_get::<String>("", "name")
                .map(|name| name == "role")
                .unwrap_or(false)
        });
        if !exists {
            manager
                .get_connection()
                .execute_unprepared(
                    "ALTER TABLE auth_sessions ADD COLUMN role TEXT NOT NULL DEFAULT 'owner'",
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "session roles cannot be safely removed".to_owned(),
        ))
    }
}
