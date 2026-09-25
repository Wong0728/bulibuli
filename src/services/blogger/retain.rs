//! 博主保留数清理：按全局设置 `storage.per_blogger_retain_default` 保留
//! 最新历史记录，删除超出范围的历史行、关联下载任务与本地文件
//! （视频/封面/侧车），文件删除失败只告警不阻塞数据库删除。

use crate::models::{blogger, download_task, history};
use anyhow::Result;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait};
use std::path::PathBuf;
use tracing::{info, warn};

use super::BloggerService;

impl BloggerService {
    /// 按全局默认保留数清理该博主的多余 history 与本地文件。
    ///
    /// - 保留数取全局设置 `storage.per_blogger_retain_default`。
    /// - 设置为 0 或缺失时不限制。
    /// - 数据库只返回超出保留范围的记录，不加载保留范围内的数据。
    /// - 文件删除失败只 warn，不阻塞 DB 删除。
    pub async fn enforce_retain(&self, uid: &str) -> Result<()> {
        let validated_uid = crate::services::file_safety::validate_uid(uid)?;
        let uid = validated_uid.as_str();
        let b = blogger::Entity::find()
            .filter(blogger::Column::Uid.eq(uid))
            .one(&self.db)
            .await?;
        if b.is_none() {
            return Ok(());
        }
        let retain_limit = self.read_retain_default().await.unwrap_or(0);
        if retain_limit <= 0 {
            return Ok(());
        }

        let to_delete = history::Entity::find()
            .filter(history::Column::Uid.eq(uid))
            // 没有可靠发表时间的记录不参与保留数清理，也不是删除候选：它们既包括下载中的
            // pending 占位，也包括 B 站未下发 created 而落成 0/NULL 的历史行。此前用
            // `(pub_timestamp IS NULL)` 排序把这些行顶到队首"保护"，但保护仍然占用
            // retain 名额，会把真正已发布的视频挤出保留窗口并连带删除本地文件。
            .filter(history::Column::PubTimestamp.gt(0))
            .order_by_desc(history::Column::PubTimestamp)
            .offset(retain_limit as u64)
            .all(&self.db)
            .await?;
        if to_delete.is_empty() {
            return Ok(());
        }

        let download_dir = self.paths.download_dir.join(validated_uid.as_str());
        info!(
            "[enforce_retain] 博主 {uid} 保留 {} 条，删除 {} 条",
            retain_limit,
            to_delete.len()
        );

        let ids = to_delete.iter().map(|item| item.id).collect::<Vec<_>>();
        let transaction = self.db.begin().await?;
        let mut task_ids = Vec::new();
        for h in &to_delete {
            let tasks = download_task::Entity::find()
                .filter(download_task::Column::Bvid.eq(&h.bvid))
                .filter(download_task::Column::Cid.eq(h.cid))
                .filter(download_task::Column::Source.eq(&h.source))
                .filter(download_task::Column::Status.is_in(["completed", "failed", "cancelled"]))
                .all(&transaction)
                .await?;
            task_ids.extend(tasks.into_iter().map(|task| task.id));
        }
        if !task_ids.is_empty() {
            download_task::Entity::delete_many()
                .filter(download_task::Column::Id.is_in(task_ids))
                .exec(&transaction)
                .await?;
        }
        history::Entity::delete_many()
            .filter(history::Column::Id.is_in(ids))
            .exec(&transaction)
            .await?;
        transaction.commit().await?;

        for h in &to_delete {
            // 历史表可能包含旧版本写入的下载根目录外的绝对路径；删除操作必须先过
            // 边界校验，不能因为“明确记录”而绕过（与 history/records.rs 一致）。
            // 侧车/封面兜底目录同样要过边界校验；来源路径越界时回退到 UID 目录。
            let recorded_parent = h
                .file_path
                .as_deref()
                .map(std::path::Path::new)
                .and_then(|path| path.parent())
                .map(|dir| dir.to_path_buf());
            let sidecar_dir = match recorded_parent {
                Some(dir)
                    if crate::services::file_safety::ensure_existing_within_root(
                        &self.paths.download_dir,
                        &dir,
                    )
                    .await
                    .is_ok() =>
                {
                    dir
                }
                _ => download_dir.clone(),
            };
            // 删除本地视频文件
            if let Some(p) = h.file_path.as_deref() {
                let path = PathBuf::from(p);
                if crate::services::file_safety::ensure_existing_within_root(
                    &self.paths.download_dir,
                    &path,
                )
                .await
                .is_err()
                {
                    warn!(
                        "[enforce_retain] 跳过下载根目录外的历史路径 {}",
                        path.display()
                    );
                } else if path.exists() {
                    if let Err(e) = tokio::fs::remove_file(&path).await {
                        warn!(
                            "[enforce_retain] 删除视频文件失败 {} {}: {e}",
                            h.bvid,
                            path.display()
                        );
                    }
                }
            }
            // 删除本地封面文件
            if let Some(p) = h.cover_local_path.as_deref() {
                let path = PathBuf::from(p);
                if crate::services::file_safety::ensure_existing_within_root(
                    &self.paths.download_dir,
                    &path,
                )
                .await
                .is_err()
                {
                    warn!(
                        "[enforce_retain] 跳过下载根目录外的历史路径 {}",
                        path.display()
                    );
                } else if path.exists() {
                    if let Err(e) = tokio::fs::remove_file(&path).await {
                        warn!(
                            "[enforce_retain] 删除封面文件失败 {} {}: {e}",
                            h.bvid,
                            path.display()
                        );
                    }
                }
            } else {
                // 兜底：扫描 download_dir 下 {bvid}_cover.* 文件
                self.try_remove_cover_files(&sidecar_dir, &h.bvid).await;
            }
            for (path, error) in
                crate::services::file_safety::remove_bvid_sidecars(&sidecar_dir, &h.bvid).await
            {
                if let Some(error) = error {
                    warn!(
                        "[enforce_retain] 删除侧车文件失败 {} {}: {error}",
                        h.bvid,
                        path.display()
                    );
                }
            }
        }
        Ok(())
    }

    /// 扫描目录下所有 `{bvid}_cover.*` 文件并删除（保留数清理时的兜底）。
    async fn try_remove_cover_files(&self, dir: &std::path::Path, bvid: &str) {
        let prefix = format!("{bvid}_cover.");
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with(&prefix) && !name.ends_with(".downloading") {
                    if let Err(error) = tokio::fs::remove_file(entry.path()).await {
                        warn!("清理超额保留文件失败: {error}");
                    }
                }
            }
        }
    }

    /// 读取 `storage.per_blogger_retain_default` 设置值。
    async fn read_retain_default(&self) -> Option<i32> {
        let settings = crate::services::settings::all_settings(&self.db)
            .await
            .ok()?;
        settings
            .get("storage")
            .and_then(|s| s.get("per_blogger_retain_default"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
    }
}
