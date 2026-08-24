//! 用户级备份：POST /api/backup（数据库快照）和 POST /api/backup/full（完整恢复目录）。
//!
//! 执行 SQLite `VACUUM INTO` 把当前数据库完整导出到
//! `data/backups/bulibuli-backup-<timestamp>.db`，并轮转保留最近 5 份。
//! VACUUM INTO 产出的是紧凑化的一致性数据库快照；完整恢复还需要
//! `security.toml`、onboarding 状态、密钥材料和下载文件，因此另建完整恢复目录。

use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use std::path::Path;
use tracing::error;

/// 备份保留份数（含最新一份）。
const BACKUP_KEEP: usize = 5;
const BACKUP_PREFIX: &str = "bulibuli-backup-";
const FULL_BACKUP_KEEP: usize = 3;
const FULL_BACKUP_PREFIX: &str = "bulibuli-full-";

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/backup", post(backup))
        .route("/api/backup/full", post(full_backup))
}

async fn backup(State(state): State<SharedState>) -> Result<Json<ApiResponse<Value>>, AppError> {
    let dir = state.infra.paths.data_dir.join("backups");
    tokio::fs::create_dir_all(&dir).await?;
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let target = dir.join(format!("{BACKUP_PREFIX}{timestamp}.db"));
    // SQLite 要求 VACUUM INTO 的目标文件不存在；同秒重复请求时提前给出可读错误。
    if target.exists() {
        return Err(AppError::Conflict(
            "同一秒内已创建过备份，请稍后重试".to_string(),
        ));
    }
    // VACUUM INTO 不能在事务内执行；execute_unprepared 以独立语句运行。
    // 单引号转义防路径注入（路径来自本地 data_dir，仍按惯例转义）。
    let sql = format!(
        "VACUUM INTO '{}'",
        target.display().to_string().replace('\'', "''")
    );
    if let Err(e) = state.infra.db.execute_unprepared(&sql).await {
        error!(error = %e, "VACUUM INTO 备份失败");
        return Err(AppError::Internal(format!("创建备份失败: {e}")));
    }
    let kept = rotate_backups(&dir, BACKUP_KEEP).await.map_err(|e| {
        error!(error = %e, "备份轮转失败");
        AppError::Internal(format!("备份轮转失败: {e}"))
    })?;
    Ok(Json(ApiResponse::with_message(
        json!({
            "type": "database_snapshot",
            "file": target.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
            "kept": kept,
        }),
        "数据库快照已创建（不包含密钥、设置文件和下载目录）",
    )))
}

/// 创建可用于恢复用户状态的完整 data 快照目录。
async fn full_backup(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let dir = state.infra.paths.data_dir.join("backups");
    tokio::fs::create_dir_all(&dir).await?;
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let target = dir.join(format!("{FULL_BACKUP_PREFIX}{timestamp}"));
    if target.exists() {
        return Err(AppError::Conflict(
            "同一秒内已创建过完整恢复备份，请稍后重试".to_string(),
        ));
    }
    let database_target = target.join("database").join("app.db");
    tokio::fs::create_dir_all(
        database_target
            .parent()
            .expect("database target has parent"),
    )
    .await?;

    if let Err(error) = vacuum_into(&state, &database_target).await {
        let _ = tokio::fs::remove_dir_all(&target).await;
        error!(%error, "完整恢复备份数据库快照失败");
        return Err(error);
    }

    let source = state.infra.paths.data_dir.clone();
    let copy_target = target.clone();
    let skipped = match tokio::task::spawn_blocking(move || {
        copy_data_snapshot(&source, &copy_target)
    })
    .await
    {
        Ok(Ok(skipped)) => skipped,
        Ok(Err(error)) => {
            let _ = tokio::fs::remove_dir_all(&target).await;
            return Err(AppError::Internal(format!("复制完整恢复备份失败: {error}")));
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&target).await;
            return Err(AppError::Internal(format!(
                "复制完整恢复备份任务失败: {error}"
            )));
        }
    };

    let manifest = json!({
        "format": "bulibuli-full-data-v1",
        "database": "database/app.db",
        "restore_requires_stopped_process": true,
        "includes": ["database/app.db", "security.toml", "startup_state.json", ".secret-store.key", "downloads", "logs", "other data files"],
        "skipped": skipped,
        "note": "数据库是 VACUUM INTO 一致性快照；下载文件按复制时状态保存，恢复前必须停止程序并校验文件。"
    });
    tokio::fs::write(
        target.join("BACKUP-MANIFEST.json"),
        serde_json::to_vec_pretty(&manifest)
            .map_err(|error| AppError::Internal(error.to_string()))?,
    )
    .await?;

    rotate_full_backups(&dir, FULL_BACKUP_KEEP)
        .await
        .map_err(|error| {
            error!(%error, "完整恢复备份轮转失败");
            AppError::Internal(format!("完整恢复备份轮转失败: {error}"))
        })?;
    let relative = target
        .strip_prefix(&state.infra.paths.data_dir)
        .unwrap_or(&target)
        .to_string_lossy()
        .replace('\\', "/");
    Ok(Json(ApiResponse::with_message(
        json!({
            "type": "full_data_snapshot",
            "directory": relative,
            "skipped": manifest["skipped"].clone(),
        }),
        "完整恢复备份已创建；恢复前必须停止程序，并同时恢复密钥材料",
    )))
}

async fn vacuum_into(state: &SharedState, target: &Path) -> Result<(), AppError> {
    let sql = format!(
        "VACUUM INTO '{}'",
        target.display().to_string().replace('\'', "''")
    );
    state
        .infra
        .db
        .execute_unprepared(&sql)
        .await
        .map(|_| ())
        .map_err(|error| AppError::Internal(format!("创建数据库快照失败: {error}")))
}

fn copy_data_snapshot(source: &Path, target: &Path) -> anyhow::Result<Vec<String>> {
    let mut skipped = Vec::new();
    copy_data_entries(source, target, &mut skipped)?;
    Ok(skipped)
}

fn copy_data_entries(
    source: &Path,
    target: &Path,
    skipped: &mut Vec<String>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy().to_string();
        if matches!(
            name.as_str(),
            "backups" | "database" | "bulibuli.lock" | "actual_port.txt"
        ) {
            continue;
        }
        let source_path = entry.path();
        let target_path = target.join(&name);
        let metadata = std::fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            skipped.push(name);
        } else if metadata.is_dir() {
            std::fs::create_dir_all(&target_path)?;
            copy_data_entries(&source_path, &target_path, skipped)?;
        } else if metadata.is_file() {
            std::fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

async fn rotate_full_backups(dir: &Path, keep: usize) -> anyhow::Result<()> {
    let mut backups = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(FULL_BACKUP_PREFIX) || !entry.file_type().await?.is_dir() {
            continue;
        }
        backups.push((entry.metadata().await?.modified()?, name));
    }
    backups.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, name) in backups.into_iter().skip(keep) {
        if let Err(error) = tokio::fs::remove_dir_all(dir.join(&name)).await {
            tracing::warn!(%error, backup = %name, "删除过期完整恢复备份失败");
        }
    }
    Ok(())
}

/// 轮转：只保留目录内最新 `keep` 份 `{BACKUP_PREFIX}*.db`，删除其余。
/// 返回保留的文件名列表（按新→旧排序）。非备份命名规则的文件不动。
async fn rotate_backups(dir: &Path, keep: usize) -> anyhow::Result<Vec<String>> {
    let mut backups: Vec<(std::time::SystemTime, String)> = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(BACKUP_PREFIX) || !name.ends_with(".db") {
            continue;
        }
        let modified = entry.metadata().await?.modified()?;
        backups.push((modified, name));
    }
    backups.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let mut kept = Vec::new();
    for (index, (_, name)) in backups.into_iter().enumerate() {
        if index < keep {
            kept.push(name);
        } else {
            let path = dir.join(&name);
            if let Err(e) = tokio::fs::remove_file(&path).await {
                // 单个文件删除失败不阻塞整体轮转（Windows 上可能被占用）。
                tracing::warn!(file = %name, "删除过期备份失败: {e}");
                kept.push(name);
            }
        }
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{Database, Statement};

    #[tokio::test]
    async fn vacuum_into_produces_readable_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        // 用文件库作为源库：与生产路径一致（VACUUM INTO 从 :memory: 源导出
        // 在部分 SQLite 构建上不产出文件，故测试不走内存库）。
        let source_path = temp.path().join("source.db");
        let db = Database::connect(format!(
            "sqlite://{}?mode=rwc",
            crate::config::encode_path_for_url(&source_path.to_string_lossy())
        ))
        .await
        .expect("connect source sqlite");
        db.execute_unprepared("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
            .await
            .expect("create table");
        db.execute_unprepared("INSERT INTO t (v) VALUES ('hello')")
            .await
            .expect("insert");
        let target = temp.path().join("backup.db");
        let sql = format!(
            "VACUUM INTO '{}'",
            target.display().to_string().replace('\'', "''")
        );
        let result = db.execute_unprepared(&sql).await;
        assert!(result.is_ok(), "vacuum into 失败: {:?}", result.err());
        assert!(target.is_file(), "备份文件未生成: {}", target.display());
        assert!(target.metadata().expect("metadata").len() > 0);

        // 快照可读：用独立连接打开备份文件验证内容。
        let snapshot = Database::connect(format!(
            "sqlite://{}?mode=ro",
            crate::config::encode_path_for_url(&target.to_string_lossy())
        ))
        .await
        .expect("open snapshot");
        let row = snapshot
            .query_one_raw(Statement::from_string(
                snapshot.get_database_backend(),
                "SELECT COUNT(*) AS count FROM t".to_string(),
            ))
            .await
            .expect("query snapshot")
            .expect("row");
        let count: i64 = row.try_get("", "count").expect("count");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn rotate_keeps_newest_and_removes_older() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        for i in 0..7 {
            let name = format!("{BACKUP_PREFIX}2026010{i}-000000.db");
            let path = dir.join(&name);
            std::fs::write(&path, b"x").expect("write");
            // 交错设置修改时间，保证新旧可区分。
            let time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(i * 100);
            let file = std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("open");
            file.set_modified(time).expect("set_modified");
        }
        // 非备份命名规则的文件不应被动。
        std::fs::write(dir.join("other.db"), b"x").expect("write");

        let kept = rotate_backups(dir, BACKUP_KEEP).await.expect("rotate");
        assert_eq!(kept.len(), BACKUP_KEEP);
        // 保留的是最新的 5 份（时间戳 2..6），最旧的两份被删除。
        for i in 0..2 {
            assert!(!dir
                .join(format!("{BACKUP_PREFIX}2026010{i}-000000.db"))
                .exists());
        }
        for i in 2..7 {
            assert!(dir
                .join(format!("{BACKUP_PREFIX}2026010{i}-000000.db"))
                .exists());
        }
        assert!(dir.join("other.db").exists());
    }

    #[test]
    fn full_snapshot_skips_live_and_backup_roots() {
        let source = tempfile::tempdir().expect("source tempdir");
        let target = tempfile::tempdir().expect("target tempdir");
        std::fs::write(source.path().join("startup_state.json"), b"{}").expect("state");
        std::fs::create_dir(source.path().join("database")).expect("database");
        std::fs::write(source.path().join("database").join("app.db"), b"live").expect("db");
        std::fs::create_dir(source.path().join("backups")).expect("backups");
        std::fs::write(source.path().join("backups").join("old.db"), b"old").expect("old");
        let skipped = copy_data_snapshot(source.path(), target.path()).expect("copy");

        assert!(target.path().join("startup_state.json").is_file());
        assert!(!target.path().join("database").exists());
        assert!(!target.path().join("backups").exists());
        assert!(skipped.is_empty());
    }
}
