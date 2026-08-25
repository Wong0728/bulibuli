//! 数据库连接初始化：建目录、连接、WAL 配置、迁移失败时的备份回滚。

use crate::config::{AppConfig, AppPaths};
use anyhow::{Context, Result};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::migrations::run_migrations;

pub async fn init_database(paths: &AppPaths, config: &AppConfig) -> Result<DatabaseConnection> {
    std::fs::create_dir_all(&paths.data_dir).context("创建数据目录失败")?;
    std::fs::create_dir_all(&paths.download_dir).context("创建下载目录失败")?;

    let database_path = paths.database_dir.join("app.db");
    let database_existed = database_path.exists();
    let url = paths.database_url();
    let mut opt = ConnectOptions::new(url);
    opt.max_connections(10)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(30))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .sqlx_logging(config.debug)
        // PRAGMA 必须挂到 SqliteConnectOptions 上：sqlx 对每个新建立的池化连接
        // 都会执行这些配置。若只在建池后对单个连接执行 PRAGMA，其余连接的
        // foreign_keys / busy_timeout / synchronous 均不生效（外键约束形同虚设）。
        .map_sqlx_sqlite_opts(|opts| {
            opts.journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
                .busy_timeout(Duration::from_millis(5000))
                .foreign_keys(true)
                .pragma("cache_size", "-64000")
        });

    let db = Database::connect(opt).await.context("连接数据库失败")?;

    let backup = if database_existed && migrations_pending(&db).await? {
        db.execute_raw(Statement::from_string(
            db.get_database_backend(),
            "PRAGMA wal_checkpoint(FULL);".to_string(),
        ))
        .await
        .context("迁移前执行 WAL checkpoint 失败")?;
        backup_database_files(&database_path)?
    } else {
        None
    };
    if let Err(error) = run_migrations(&db).await {
        db.close().await.context("关闭迁移失败后的数据库连接")?;
        if let Err(restore_error) = restore_database_files(&database_path, backup.as_deref()) {
            // 恢复失败详情仅记录日志，主错误仍以原始迁移错误为准。
            tracing::error!(%restore_error, %error, "迁移失败后的数据库备份恢复失败");
        }
        return Err(error);
    }
    if backup.is_some() {
        prune_migration_backups(&database_path, 5)?;
    }

    Ok(db)
}

async fn migrations_pending(db: &DatabaseConnection) -> Result<bool> {
    let table_exists = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?".to_string(),
            [sea_orm::Value::from("seaql_migrations")],
        ))
        .await?
        .is_some();
    if !table_exists {
        return Ok(true);
    }
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            // SeaORM migration table uses `version`, not a generic `name` column.
            "SELECT version FROM seaql_migrations".to_string(),
        ))
        .await?;
    let applied: std::collections::HashSet<String> = rows
        .iter()
        .filter_map(|row| row.try_get::<String>("", "version").ok())
        .collect();
    // 按迁移名比对而非 COUNT(*)：未来迁移列表被 squash 后行数恰好相等时，
    // 计数比较会误判“无待执行迁移”而跳过备份。
    Ok(crate::migration::Migrator::migrations()
        .iter()
        .any(|migration| !applied.contains(migration.name())))
}

fn backup_database_files(database_path: &Path) -> Result<Option<PathBuf>> {
    if !database_path.exists() {
        return Ok(None);
    }
    let parent = database_path.parent().context("数据库路径没有父目录")?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let backup_dir = parent.join("migration-backups").join(stamp.to_string());
    std::fs::create_dir_all(&backup_dir).context("创建数据库迁移备份目录失败")?;
    for suffix in ["", "-wal", "-shm"] {
        let source = PathBuf::from(format!("{}{}", database_path.display(), suffix));
        if source.exists() {
            let name = source.file_name().context("数据库备份源文件无文件名")?;
            std::fs::copy(&source, backup_dir.join(name))
                .with_context(|| format!("备份数据库文件失败: {}", source.display()))?;
        }
    }
    Ok(Some(backup_dir))
}

fn restore_database_files(database_path: &Path, backup_dir: Option<&Path>) -> Result<()> {
    let Some(backup_dir) = backup_dir else {
        return Ok(());
    };
    // 先删除目标侧的 -wal/-shm：旧 WAL/SHM 与恢复出的主库不匹配会导致数据损坏。
    for suffix in ["-wal", "-shm"] {
        let stale = PathBuf::from(format!("{}{}", database_path.display(), suffix));
        match std::fs::remove_file(&stale) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("删除过期数据库文件失败: {}", stale.display()));
            }
        }
    }
    for suffix in ["", "-wal", "-shm"] {
        let target = PathBuf::from(format!("{}{}", database_path.display(), suffix));
        let Some(name) = target.file_name() else {
            continue;
        };
        let source = backup_dir.join(name);
        if source.exists() {
            std::fs::copy(&source, &target)
                .with_context(|| format!("恢复数据库文件失败: {}", target.display()))?;
        }
    }
    Ok(())
}

fn prune_migration_backups(database_path: &Path, keep: usize) -> Result<()> {
    let Some(parent) = database_path.parent() else {
        return Ok(());
    };
    let root = parent.join("migration-backups");
    if !root.is_dir() {
        return Ok(());
    }
    let mut backups = std::fs::read_dir(&root)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    backups.sort_by_key(|entry| entry.file_name());
    let remove_count = backups.len().saturating_sub(keep);
    for entry in backups.into_iter().take(remove_count) {
        std::fs::remove_dir_all(entry.path())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Database;

    #[tokio::test]
    async fn migrations_pending_reads_seaorm_version_column() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        run_migrations(&db).await.expect("apply migrations");

        assert!(!migrations_pending(&db)
            .await
            .expect("check migration status"));
    }
}
