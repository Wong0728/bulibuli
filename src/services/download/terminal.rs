//! 任务失败终态收敛：批量登记失败行、同步看板 history 记录、清理下载残片。

use crate::models::{download_task, history};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::path::PathBuf;
use tracing::{info, warn};

use super::{task_cache_key, DownloadManager};

impl DownloadManager {
    pub(super) async fn queue_terminal_failure(
        &self,
        task: &download_task::Model,
        error: String,
        updates: &mut Vec<(String, i32, i64, download_task::ActiveModel)>,
    ) {
        let mut model: download_task::ActiveModel = task.clone().into();
        model.status = Set("failed".to_string());
        model.error = Set(Some(error.clone()));
        model.speed = Set(0);
        updates.push((task.bvid.clone(), task.id, task.generation, model));
        self.progress_cache
            .lock()
            .await
            .remove(&task_cache_key(&task.bvid, task.cid));
        self.cleanup_failed_transfer_leftovers(task).await;
        self.mark_history_failed(&task.bvid, task.cid, &task.task_type)
            .await;
    }

    /// 任务进入失败终态时同步看板记录。
    ///
    /// 看板"失败"Tab 按 `history.state = 'failed'` 过滤，而此前没有任何代码写过该值，
    /// 失败的手动任务会从三个 Tab 同时消失（记录被删掉，产物也无人认领）。
    /// 只接管下载中的占位行：已完成/已下架/付费受限/校验异常不能被一次下载失败改写。
    /// 例外：DASH 音视频先完成的一方会把行写成 completed，但 file_path 指向
    /// .m4s/.m4a 中间产物（成品要等合并回写）；video 侧终态失败意味着合并
    /// 不会再发生，这条"假完成"行回收为 failed，避免看板承诺不可播放的半成品。
    /// audio 侧不回收——音频重试耗尽后有纯视频降级合并兜底。
    pub(super) async fn mark_history_failed(
        &self,
        bvid: &str,
        cid: Option<i64>,
        failed_task_type: &str,
    ) {
        let mut query = history::Entity::find().filter(history::Column::Bvid.eq(bvid));
        query = match cid {
            Some(c) => query.filter(history::Column::Cid.eq(c)),
            None => query.filter(history::Column::Cid.is_null()),
        };
        let row = match query.one(&self.db).await {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(e) => {
                warn!("查询待标记失败的历史记录失败 {bvid}: {e}");
                return;
            }
        };
        if !matches!(
            row.state.as_deref(),
            None | Some("pending") | Some("downloading")
        ) {
            let reclaimable = failed_task_type == "video"
                && row.state.as_deref() == Some("completed")
                && row
                    .file_path
                    .as_deref()
                    .is_some_and(|p| p.ends_with(".m4s") || p.ends_with(".m4a"));
            if !reclaimable {
                return;
            }
            let mut model: history::ActiveModel = row.into();
            model.state = Set(Some("failed".to_string()));
            // 清掉完成时间，让后续重试成功的 add_to_history 能重新接管此行。
            model.download_time = Set(None);
            if let Err(e) = model.update(&self.db).await {
                warn!("回收假完成历史记录失败 {bvid}: {e}");
            }
            return;
        }
        let mut model: history::ActiveModel = row.into();
        model.state = Set(Some("failed".to_string()));
        if let Err(e) = model.update(&self.db).await {
            warn!("同步失败状态到历史记录失败 {bvid}: {e}");
        }
    }

    /// aria2 任务判失败后清理 `.downloading` 临时文件与对应 `.aria2` 控制文件，
    /// 与 native 路径"失败即删残片"的行为对齐，避免反复失败在下载目录累积垃圾文件。
    pub(super) async fn cleanup_failed_transfer_leftovers(&self, task: &download_task::Model) {
        let Some(filename) = task
            .filename
            .as_deref()
            .filter(|name| name.ends_with(".downloading"))
        else {
            return;
        };
        let Some(dir) = task.download_dir.as_deref() else {
            return;
        };
        let dir = PathBuf::from(dir);
        for leftover in [dir.join(filename), dir.join(format!("{filename}.aria2"))] {
            // 路径来自 DB（旧库可能残留越界值），删除前确认仍位于下载根目录内
            if crate::services::file_safety::ensure_existing_within_root(
                &self.paths.download_dir,
                &leftover,
            )
            .await
            .is_err()
            {
                continue;
            }
            match tokio::fs::remove_file(&leftover).await {
                Ok(()) => info!("已清理下载残片: {}", leftover.display()),
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    warn!("清理下载残片失败 {}: {e}", leftover.display());
                }
                Err(_) => {}
            }
        }
    }
}
