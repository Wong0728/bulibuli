use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde::Serialize;
use std::collections::HashMap;

/// 终态烧录任务在内存表中的保留时长（秒）。
pub const BURN_TASK_TTL_SECONDS: i64 = 60 * 60;
/// 内存烧录任务表容量上限。
pub const MAX_BURN_TASKS: usize = 200;

/// 烧录任务的非终态状态。download 与 live 两个模块写同一张
/// `state.media.burn_tasks` 表，词表统一在此，防止两侧漂移
/// （此前一侧写 "processing"、另一侧按 "running" 清理）。
pub fn burn_status_active(status: &str) -> bool {
    matches!(status, "queued" | "processing" | "running")
}

/// 统一的烧录任务表清理：保留非终态任务与 TTL 内的任务，
/// 超出容量时按最旧 updated_at 淘汰终态条目。
pub fn prune_burn_tasks(tasks: &mut HashMap<String, BurnTask>) {
    let now = chrono::Utc::now().timestamp();
    tasks.retain(|_, task| {
        burn_status_active(&task.status)
            || now.saturating_sub(task.updated_at.max(task.created_at)) <= BURN_TASK_TTL_SECONDS
    });
    if tasks.len() <= MAX_BURN_TASKS {
        return;
    }
    let mut terminal = tasks
        .iter()
        .filter(|(_, task)| !burn_status_active(&task.status))
        .map(|(id, task)| (id.clone(), task.updated_at))
        .collect::<Vec<_>>();
    terminal.sort_by_key(|(_, updated_at)| *updated_at);
    for (id, _) in terminal.into_iter().take(tasks.len() - MAX_BURN_TASKS) {
        tasks.remove(&id);
    }
}

/// 字幕或弹幕烧录接口返回的可序列化状态。
#[derive(Clone, Serialize)]
pub struct BurnTask {
    pub bvid: String,
    pub status: String,
    pub message: String,
    pub output_path: Option<String>,
    #[serde(skip_serializing)]
    pub created_at: i64,
    #[serde(skip_serializing)]
    pub updated_at: i64,
}

pub async fn persist_burn_task(
    db: &DatabaseConnection,
    id: &str,
    task: &BurnTask,
) -> Result<(), sea_orm::DbErr> {
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO burn_tasks (id, bvid, status, message, output_path, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET bvid=excluded.bvid, status=excluded.status,
           message=excluded.message, output_path=excluded.output_path,
           created_at=excluded.created_at, updated_at=excluded.updated_at",
        [
            id.to_owned().into(),
            task.bvid.clone().into(),
            task.status.clone().into(),
            task.message.clone().into(),
            task.output_path.clone().into(),
            task.created_at.into(),
            task.updated_at.into(),
        ],
    ))
    .await?;
    Ok(())
}

pub async fn find_burn_task(
    db: &DatabaseConnection,
    id: &str,
) -> Result<Option<BurnTask>, sea_orm::DbErr> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT bvid, status, message, output_path, created_at, updated_at
             FROM burn_tasks WHERE id = ?",
            [id.to_owned().into()],
        ))
        .await?;
    row.map(|row| {
        Ok(BurnTask {
            bvid: row.try_get("", "bvid")?,
            status: row.try_get("", "status")?,
            message: row.try_get("", "message")?,
            output_path: row.try_get("", "output_path")?,
            created_at: row.try_get("", "created_at")?,
            updated_at: row.try_get("", "updated_at")?,
        })
    })
    .transpose()
}

pub async fn restore_burn_tasks(
    db: &DatabaseConnection,
) -> Result<HashMap<String, BurnTask>, sea_orm::DbErr> {
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, bvid, status, message, output_path, created_at, updated_at
             FROM burn_tasks ORDER BY updated_at DESC LIMIT 200"
                .to_owned(),
        ))
        .await?;
    let mut tasks = HashMap::new();
    for row in rows {
        let id: String = row.try_get("", "id")?;
        let mut task = BurnTask {
            bvid: row.try_get("", "bvid")?,
            status: row.try_get("", "status")?,
            message: row.try_get("", "message")?,
            output_path: row.try_get("", "output_path")?,
            created_at: row.try_get("", "created_at")?,
            updated_at: row.try_get("", "updated_at")?,
        };
        if burn_status_active(&task.status) {
            task.status = "failed".to_owned();
            task.message = "程序重启前烧录未完成，请重新发起烧录".to_owned();
            task.updated_at = chrono::Utc::now().timestamp();
            persist_burn_task(db, &id, &task).await?;
        }
        tasks.insert(id, task);
    }
    Ok(tasks)
}
