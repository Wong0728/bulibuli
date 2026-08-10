//! 数据库迁移入口：统一使用 SeaORM 迁移轨道。
//! 所有历史迁移已合并为单一 `m20260801_000001_initial_schema` 迁移。

use crate::migration::Migrator;
use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;
use sea_orm_migration::MigratorTrait;
use tracing::info;

pub(super) async fn run_migrations(db: &DatabaseConnection) -> Result<()> {
    info!("执行 SeaORM 数据库迁移");
    Migrator::up(db, None)
        .await
        .context("执行 SeaORM migration 失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppPaths;
    use crate::services::blogger::{BloggerService, BloggerUpdate, NewBlogger};
    use sea_orm::{ConnectionTrait, Database, Statement, TransactionTrait};
    use std::sync::Arc;

    #[tokio::test]
    async fn fresh_migrations_create_complete_schema() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");

        run_migrations(&db).await.expect("first migration pass");
        run_migrations(&db)
            .await
            .expect("idempotent migration pass");

        // 验证 download_tasks 表包含所有关键字段
        let columns = db
            .query_all_raw(Statement::from_string(
                db.get_database_backend(),
                "PRAGMA table_info(download_tasks)".to_string(),
            ))
            .await
            .expect("read download task columns");
        let names = columns
            .iter()
            .map(|row| row.try_get::<String>("", "name").expect("column name"))
            .collect::<Vec<_>>();
        for required in [
            "generation",
            "completion_triggered",
            "stage",
            "priority",
            "attempts",
            "next_retry_at",
            "error_kind",
            "selected_quality",
            "selected_codec",
            "fallback_reason",
            "face_url",
            "version",
        ] {
            assert!(names.iter().any(|name| name == required), "{required}");
        }
        // cookies 列已被安全迁移移除
        assert!(!names.iter().any(|name| name == "cookies"));

        // 验证 operation_log 审计表存在且包含核心字段
        let op_log_columns = db
            .query_all_raw(Statement::from_string(
                db.get_database_backend(),
                "PRAGMA table_info(operation_log)".to_string(),
            ))
            .await
            .expect("read operation_log columns")
            .iter()
            .map(|row| row.try_get::<String>("", "name").expect("column name"))
            .collect::<Vec<_>>();
        for required in [
            "at",
            "source",
            "caller_id",
            "route_or_command",
            "target_type",
            "target_id",
            "action",
            "expected_version",
            "new_version",
            "outcome",
            "error_code",
            "request_id",
        ] {
            assert!(
                op_log_columns.iter().any(|name| name == required),
                "operation_log missing {required}"
            );
        }

        // 验证 history 表包含所有关键字段
        let history_columns = db
            .query_all_raw(Statement::from_string(
                db.get_database_backend(),
                "PRAGMA table_info(history)".to_string(),
            ))
            .await
            .expect("read history columns")
            .iter()
            .map(|row| row.try_get::<String>("", "name").expect("column name"))
            .collect::<Vec<_>>();
        for required in [
            "source",
            "auto_burn_status",
            "auto_burn_attempts",
            "auto_burn_next_retry_at",
            "sidecar_attempts",
            "next_sidecar_at",
            "version",
        ] {
            assert!(
                history_columns.iter().any(|name| name == required),
                "{required}"
            );
        }

        // 验证 bloggers 表包含所有关键字段
        let blogger_columns = db
            .query_all_raw(Statement::from_string(
                db.get_database_backend(),
                "PRAGMA table_info(bloggers)".to_string(),
            ))
            .await
            .expect("read blogger columns")
            .iter()
            .map(|row| row.try_get::<String>("", "name").expect("column name"))
            .collect::<Vec<_>>();
        assert!(!blogger_columns.iter().any(|name| name == "retain_max"));
        assert!(blogger_columns.iter().any(|name| name == "is_saved"));
        assert!(blogger_columns.iter().any(|name| name == "has_auto_task"));
        assert!(blogger_columns.iter().any(|name| name == "version"));
        assert!(!blogger_columns.iter().any(|name| name == "monitor_enabled"));

        // 验证安全相关表存在
        for table in ["auth_sessions", "protected_secrets", "security_meta"] {
            let exists = db
                .query_one_raw(Statement::from_string(
                    db.get_database_backend(),
                    format!(
                        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
                    ),
                ))
                .await
                .expect("query table");
            assert!(exists.is_some(), "{table} should exist");
        }

        // 验证 submission_checkpoints 表存在
        let checkpoint = db.query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'submission_checkpoints'".to_string(),
        )).await.expect("query checkpoint table");
        assert!(checkpoint.is_some());

        // 验证 SeaORM 迁移记录数等于已注册迁移数（无重复、无遗漏）
        let applied = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT COUNT(*) AS count FROM seaql_migrations".to_string(),
            ))
            .await
            .expect("count migrations")
            .expect("migration count row")
            .try_get::<i64>("", "count")
            .expect("migration count");
        assert_eq!(applied, Migrator::migrations().len() as i64);
    }

    #[tokio::test]
    async fn transaction_rollback_does_not_persist_partial_writes() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        run_migrations(&db).await.expect("migrate");

        let transaction = db.begin().await.expect("begin transaction");
        transaction
            .execute_raw(Statement::from_string(
                transaction.get_database_backend(),
                "INSERT INTO settings (key, value) VALUES ('rollback-test', 'value')".to_string(),
            ))
            .await
            .expect("insert in transaction");
        transaction.rollback().await.expect("rollback");

        let row = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT value FROM settings WHERE key = 'rollback-test'".to_string(),
            ))
            .await
            .expect("query rolled-back row");
        assert!(row.is_none());
    }

    #[tokio::test]
    async fn saved_bloggers_and_auto_tasks_have_independent_lifecycles() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        run_migrations(&db).await.expect("migrate");
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = Arc::new(AppPaths {
            app_root: temp.path().to_path_buf(),
            data_dir: temp.path().to_path_buf(),
            database_dir: temp.path().join("database"),
            download_dir: temp.path().join("downloads"),
        });
        let service = BloggerService::new(db, paths);
        let saved = service
            .add_blogger(NewBlogger {
                uid: "123456".to_string(),
                name: Some("测试博主".to_string()),
                min_interval: 60,
                max_interval: 300,
                face: None,
                sign: None,
                level: None,
                fans: None,
                download_video: true,
                download_danmaku: true,
                download_comments: true,
                download_cover: true,
                burn_danmaku: false,
                burn_subtitle: false,
                series_filter_regex: None,
                active_windows: None,
                monitor_enabled: false,
                is_saved: true,
                has_auto_task: false,
            })
            .await
            .expect("add saved blogger");

        assert_eq!(service.list_saved().await.expect("saved list").len(), 1);
        assert!(service
            .list_auto_tasks()
            .await
            .expect("auto list")
            .is_empty());

        service
            .apply_update(
                saved.clone(),
                BloggerUpdate {
                    has_auto_task: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("create auto task");
        assert_eq!(
            service
                .list_auto_tasks()
                .await
                .expect("auto list after create")
                .len(),
            1
        );

        service
            .remove_auto_task(saved.id)
            .await
            .expect("remove auto task");
        assert!(service
            .list_auto_tasks()
            .await
            .expect("auto list after delete")
            .is_empty());
        assert_eq!(
            service
                .list_saved()
                .await
                .expect("saved list after auto delete")
                .len(),
            1
        );

        let current = service
            .find_by_id(saved.id)
            .await
            .expect("find blogger")
            .expect("blogger exists");
        service
            .apply_update(
                current,
                BloggerUpdate {
                    has_auto_task: Some(true),
                    ..Default::default()
                },
            )
            .await
            .expect("restore auto task");
        service
            .remove_saved(saved.id)
            .await
            .expect("remove saved blogger");
        assert!(service
            .list_saved()
            .await
            .expect("saved list after delete")
            .is_empty());
        assert_eq!(
            service
                .list_auto_tasks()
                .await
                .expect("auto list after saved delete")
                .len(),
            1
        );
    }
}
