use crate::domain::TaskKey;
use crate::error::{AppError, AppResult};
use crate::models::download_task;
use sea_orm::{
    sea_query::Expr, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, TransactionTrait,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{self, Duration, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct ProgressSnapshot {
    pub task_id: i32,
    pub generation: i64,
    pub progress_percent: i32,
    pub downloaded_size: i64,
    pub total_size: i64,
    pub speed: i64,
}

enum Command {
    Flush(oneshot::Sender<AppResult<()>>),
}

#[derive(Clone)]
pub struct ProgressWriter {
    pending: Arc<Mutex<HashMap<TaskKey, ProgressSnapshot>>>,
    sender: mpsc::Sender<Command>,
    cancellation: CancellationToken,
    handle: Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl ProgressWriter {
    pub fn start(db: DatabaseConnection, cancellation: CancellationToken) -> Self {
        let (sender, receiver) = mpsc::channel(256);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let writer = Self {
            pending: pending.clone(),
            sender,
            cancellation: cancellation.clone(),
            handle: Arc::new(std::sync::Mutex::new(None)),
        };
        let task = tokio::spawn(run_writer(db, pending, receiver, cancellation));
        *writer
            .handle
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(task);
        writer
    }

    pub async fn submit(&self, key: TaskKey, snapshot: ProgressSnapshot) {
        self.pending.lock().await.insert(key, snapshot);
    }

    pub async fn flush(&self) -> AppResult<()> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Command::Flush(sender))
            .await
            .map_err(|_| AppError::Internal("progress writer stopped".to_string()))?;
        receiver
            .await
            .map_err(|_| AppError::Internal("progress flush acknowledgement lost".to_string()))?
    }

    pub async fn shutdown(&self) -> AppResult<()> {
        // 先触发取消信号，让后台任务知道要退出
        self.cancellation.cancel();

        // 然后尝试 flush，如果后台任务已退出则忽略错误
        // 后台任务在收到取消信号时会自动执行一次 flush
        if let Err(_e) = self.flush().await {
            // flush 失败通常是因为后台任务已经退出，这是正常的
            // 后台任务在退出前会自动 flush，所以数据不会丢失
        }

        let handle = self
            .handle
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(handle) = handle {
            time::timeout(Duration::from_secs(5), handle)
                .await
                .map_err(|_| AppError::Internal("progress writer shutdown timed out".to_string()))?
                .map_err(|error| AppError::Internal(error.to_string()))?;
        }
        Ok(())
    }
}

async fn run_writer(
    db: DatabaseConnection,
    pending: Arc<Mutex<HashMap<TaskKey, ProgressSnapshot>>>,
    mut receiver: mpsc::Receiver<Command>,
    cancellation: CancellationToken,
) {
    let mut interval = time::interval(Duration::from_millis(500));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                if let Err(error) = flush_pending(&db, &pending).await {
                    tracing::error!(%error, "flush progress during cancellation failed");
                }
                break;
            }
            _ = interval.tick() => {
                if let Err(error) = flush_pending(&db, &pending).await {
                    tracing::error!(%error, "periodic progress flush failed");
                }
            }
            command = receiver.recv() => match command {
                Some(Command::Flush(ack)) => {
                    let result = flush_pending(&db, &pending).await;
                    // 即使发送失败也继续（接收端可能已丢弃）
                    ack.send(result).ok();
                }
                None => {
                    if let Err(error) = flush_pending(&db, &pending).await {
                        tracing::error!(%error, "final progress flush failed");
                    }
                    break;
                }
            }
        }
    }
}

async fn flush_pending(
    db: &DatabaseConnection,
    pending: &Arc<Mutex<HashMap<TaskKey, ProgressSnapshot>>>,
) -> AppResult<()> {
    let snapshots = {
        let mut pending = pending.lock().await;
        if pending.is_empty() {
            return Ok(());
        }
        pending
            .drain()
            .map(|(_, snapshot)| snapshot)
            .collect::<Vec<_>>()
    };
    let transaction = db.begin().await?;
    for snapshot in snapshots {
        download_task::Entity::update_many()
            .col_expr(
                download_task::Column::ProgressPercent,
                Expr::value(snapshot.progress_percent.clamp(0, 100)),
            )
            .col_expr(
                download_task::Column::DownloadedSize,
                Expr::value(snapshot.downloaded_size),
            )
            .col_expr(
                download_task::Column::TotalSize,
                Expr::value(snapshot.total_size),
            )
            .col_expr(download_task::Column::Speed, Expr::value(snapshot.speed))
            .filter(download_task::Column::Id.eq(snapshot.task_id))
            .filter(download_task::Column::Generation.eq(snapshot.generation))
            .exec(&transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TaskKind;
    use sea_orm::{ConnectionTrait, Database, Statement};

    #[tokio::test]
    async fn coalesces_progress_and_flushes_latest_generation_snapshot() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        db.execute_raw(Statement::from_string(
            db.get_database_backend(),
            "CREATE TABLE download_tasks (
                id INTEGER PRIMARY KEY,
                generation INTEGER NOT NULL,
                progress_percent INTEGER NOT NULL,
                downloaded_size INTEGER NOT NULL,
                total_size INTEGER NOT NULL,
                speed INTEGER NOT NULL
            );
            INSERT INTO download_tasks VALUES (1, 3, 0, 0, 0, 0);"
                .to_string(),
        ))
        .await
        .expect("create progress table");

        let writer = ProgressWriter::start(db.clone(), CancellationToken::new());
        let key = TaskKey {
            bvid: "BV1TEST".to_string(),
            kind: TaskKind::Video,
            page: None,
        };
        writer
            .submit(
                key.clone(),
                ProgressSnapshot {
                    task_id: 1,
                    generation: 3,
                    progress_percent: 20,
                    downloaded_size: 20,
                    total_size: 100,
                    speed: 4,
                },
            )
            .await;
        writer
            .submit(
                key,
                ProgressSnapshot {
                    task_id: 1,
                    generation: 3,
                    progress_percent: 130,
                    downloaded_size: 100,
                    total_size: 100,
                    speed: 9,
                },
            )
            .await;
        writer.flush().await.expect("flush latest progress");

        let row = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT progress_percent, downloaded_size, speed FROM download_tasks WHERE id = 1"
                    .to_string(),
            ))
            .await
            .expect("query progress")
            .expect("progress row");
        assert_eq!(
            row.try_get::<i64>("", "progress_percent")
                .expect("progress percent"),
            100
        );
        assert_eq!(
            row.try_get::<i64>("", "downloaded_size")
                .expect("downloaded size"),
            100
        );
        assert_eq!(row.try_get::<i64>("", "speed").expect("speed"), 9);
        writer.shutdown().await.expect("shutdown progress writer");
    }

    #[tokio::test]
    async fn ignores_stale_generation_updates() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        db.execute_raw(Statement::from_string(
            db.get_database_backend(),
            "CREATE TABLE download_tasks (
                id INTEGER PRIMARY KEY,
                generation INTEGER NOT NULL,
                progress_percent INTEGER NOT NULL,
                downloaded_size INTEGER NOT NULL,
                total_size INTEGER NOT NULL,
                speed INTEGER NOT NULL
            );
            INSERT INTO download_tasks VALUES (1, 4, 7, 7, 100, 1);"
                .to_string(),
        ))
        .await
        .expect("create progress table");
        let writer = ProgressWriter::start(db.clone(), CancellationToken::new());
        writer
            .submit(
                TaskKey {
                    bvid: "BV1STALE".to_string(),
                    kind: TaskKind::Video,
                    page: None,
                },
                ProgressSnapshot {
                    task_id: 1,
                    generation: 3,
                    progress_percent: 80,
                    downloaded_size: 80,
                    total_size: 100,
                    speed: 10,
                },
            )
            .await;
        writer.flush().await.expect("flush stale progress");
        let row = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                "SELECT progress_percent FROM download_tasks WHERE id = 1".to_string(),
            ))
            .await
            .expect("query progress")
            .expect("progress row");
        assert_eq!(
            row.try_get::<i64>("", "progress_percent")
                .expect("progress percent"),
            7
        );
        writer.shutdown().await.expect("shutdown progress writer");
    }
}
