use crate::models::history;
use crate::services::settings::SettingsService;
use anyhow::{Context, Result};
use chrono::Local;
use futures::{stream, StreamExt};
use sea_orm::sea_query::Condition;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// verify worker 扫描间隔（60s）。
const VERIFY_SCAN_INTERVAL: StdDuration = StdDuration::from_secs(60);
/// on_completion 兼容旧数据时的回填批量。
const BACKFILL_BATCH: u64 = 50;

/// SHA-256 校验 worker。
/// - `off`：不跑。
/// - `on_completion`：下载完成时已由 `add_to_history` 立即计算；worker 只补齐 sha256 为空的旧数据。
/// - `periodic`：按 `periodic_days` 选最久未校验的 N 条，读本地 → 算 SHA-256 → 不一致则标 `tampered`。
pub struct VerifyService {
    db: DatabaseConnection,
    settings: Arc<SettingsService>,
    cancellation: CancellationToken,
    handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl VerifyService {
    pub fn new(
        db: DatabaseConnection,
        settings: Arc<SettingsService>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            db,
            settings,
            cancellation,
            handle: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub async fn start(&self) {
        if self.handle.lock().await.is_some() {
            return;
        }
        info!("[verify] SHA-256 校验 worker 已启动");
        let db = self.db.clone();
        let settings = self.settings.clone();
        let cancellation = self.cancellation.child_token();
        let handle = tokio::spawn(async move {
            verify_loop(db, settings, cancellation).await;
        });
        *self.handle.lock().await = Some(handle);
    }

    pub async fn stop(&self) {
        self.cancellation.cancel();
        if let Some(h) = self.handle.lock().await.take() {
            match tokio::time::timeout(StdDuration::from_secs(10), h).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => error!("[verify] worker 退出异常: {error}"),
                Err(_) => error!("[verify] worker 未在 10 秒内退出"),
            }
        }
        info!("[verify] SHA-256 校验 worker 已停止");
    }

    /// 手动触发一次校验（POST /api/refresh?kind=verify 用）。
    /// 返回校验的条数。
    pub async fn trigger_once(&self) -> Result<usize> {
        run_one_cycle(&self.db, &self.settings).await
    }
}

/// worker 主循环：每 60s 扫描一次。
async fn verify_loop(
    db: DatabaseConnection,
    settings: Arc<SettingsService>,
    cancellation: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = tokio::time::sleep(VERIFY_SCAN_INTERVAL) => {}
        }
        if let Err(e) = run_one_cycle(&db, &settings).await {
            error!("[verify] 出错: {e}");
        }
    }
}

/// 单次扫描：读 SettingsService 的 ArcSwap 快照 → off 则跳过；
/// on_completion 则回填空 sha256；periodic 则按天数选条。
async fn run_one_cycle(db: &DatabaseConnection, settings: &SettingsService) -> Result<usize> {
    let verify = settings.current().download.verify.clone();
    let concurrency = verify.concurrency.max(1) as usize;
    match verify.mode.as_str() {
        "off" => Ok(0),
        // 兼容旧数据：回填 sha256 为空的记录
        "on_completion" => backfill_null_sha256(db, concurrency).await,
        _ => {
            // periodic（validate 已保证 periodic_days/periodic_batch/concurrency 在合法区间）
            verify_periodic(
                db,
                i64::from(verify.periodic_days),
                u64::try_from(verify.periodic_batch).unwrap_or(0),
                concurrency,
            )
            .await
        }
    }
}

/// on_completion 兼容旧数据：选 sha256 为空且有 file_path 的记录，回填 SHA-256。
async fn backfill_null_sha256(db: &DatabaseConnection, concurrency: usize) -> Result<usize> {
    let rows = history::Entity::find()
        .filter(history::Column::Sha256.is_null())
        .filter(history::Column::FilePath.is_not_null())
        .limit(BACKFILL_BATCH)
        .all(db)
        .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    info!(
        "[verify on_completion] 回填 {} 条空 SHA-256 记录",
        rows.len()
    );
    let success = stream::iter(rows)
        .map(|h| async move {
            let Some(p) = h.file_path.as_deref() else {
                return 0usize;
            };
            let path = PathBuf::from(p);
            if !path.exists() {
                warn!("[verify] 文件不存在 {}: {}", h.bvid, path.display());
                return 0usize;
            }
            match compute_sha256_blocking(&path).await {
                Ok(digest) => {
                    let mut model: history::ActiveModel = h.clone().into();
                    model.sha256 = Set(Some(digest));
                    model.sha256_last_checked_at = Set(Some(Local::now()));
                    if let Err(e) = model.update(db).await {
                        warn!("[verify] 回填 {} 失败: {e}", h.bvid);
                        0usize
                    } else {
                        1usize
                    }
                }
                Err(e) => {
                    warn!("[verify] 计算 SHA-256 失败 {}: {e}", h.bvid);
                    0usize
                }
            }
        })
        .buffer_unordered(concurrency)
        .fold(0usize, |acc, n| async move { acc + n })
        .await;
    Ok(success)
}

/// periodic 模式：选最久未校验的 N 条，读本地 → 算 SHA-256 → 不一致标 tampered。
async fn verify_periodic(
    db: &DatabaseConnection,
    days: i64,
    batch: u64,
    concurrency: usize,
) -> Result<usize> {
    let cutoff = Local::now() - chrono::Duration::days(days);
    let rows = history::Entity::find()
        .filter(history::Column::FilePath.is_not_null())
        .filter(
            Condition::any()
                .add(history::Column::Sha256LastCheckedAt.lt(cutoff))
                .add(history::Column::Sha256LastCheckedAt.is_null()),
        )
        .order_by_asc(history::Column::Sha256LastCheckedAt)
        .limit(batch)
        .all(db)
        .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    info!(
        "[verify periodic] 校验 {} 条（间隔 {} 天，批量 {}，并发 {}）",
        rows.len(),
        days,
        batch,
        concurrency
    );
    let success = stream::iter(rows)
        .map(|h| async move {
            let Some(p) = h.file_path.as_deref() else {
                return 0usize;
            };
            let path = PathBuf::from(p);
            if !path.exists() {
                // 文件丢失：标 tampered
                warn!(
                    "[verify] 文件丢失，标记 tampered: {} ({})",
                    h.bvid,
                    path.display()
                );
                mark_tampered(db, h.id).await;
                return 0usize;
            }
            match compute_sha256_blocking(&path).await {
                Ok(digest) => {
                    let tampered = h.sha256.as_deref().is_some_and(|old| old != digest);
                    let mut model: history::ActiveModel = h.clone().into();
                    if tampered {
                        warn!(
                            "[verify] SHA-256 不一致，标记 tampered: {} (旧={}, 新={})",
                            h.bvid,
                            h.sha256.as_deref().unwrap_or(""),
                            digest
                        );
                        model.state = Set(Some("tampered".to_string()));
                    }
                    model.sha256 = Set(Some(digest));
                    model.sha256_last_checked_at = Set(Some(Local::now()));
                    if let Err(e) = model.update(db).await {
                        warn!("[verify] 更新 {} 失败: {e}", h.bvid);
                        0usize
                    } else {
                        1usize
                    }
                }
                Err(e) => {
                    warn!("[verify] 计算 SHA-256 失败 {}: {e}", h.bvid);
                    0usize
                }
            }
        })
        .buffer_unordered(concurrency)
        .fold(0usize, |acc, n| async move { acc + n })
        .await;
    Ok(success)
}

/// 标记某条 history 为 tampered。
async fn mark_tampered(db: &DatabaseConnection, id: i32) {
    if let Ok(Some(h)) = history::Entity::find_by_id(id).one(db).await {
        let mut model: history::ActiveModel = h.into();
        model.state = Set(Some("tampered".to_string()));
        model.sha256_last_checked_at = Set(Some(Local::now()));
        if let Err(error) = model.update(db).await {
            warn!("[verify] 持久化校验结果失败: {error}");
        }
    }
}

/// 在 spawn_blocking 中执行同步 SHA-256，避免批量校验阻塞 async executor。
async fn compute_sha256_blocking(path: &std::path::Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || sync_sha256(&path))
        .await
        .context("SHA-256 计算任务被中断")?
}

fn sync_sha256(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).context("打开文件失败")?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 512 * 1024];
    loop {
        let read = file.read(&mut buffer).context("读取文件失败")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
