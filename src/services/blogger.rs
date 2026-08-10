use crate::config::AppPaths;
use crate::models::{blogger, download_task, history};
use anyhow::Result;
use chrono::Local;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

#[path = "blogger/types.rs"]
mod types;
pub use types::{BloggerUpdate, MonitorToggle, NewBlogger};

/// 博主资料、清理策略与资料变更确认服务。
///
/// 实际清理上限统一取 `storage.per_blogger_retain_default`。
pub struct BloggerService {
    db: DatabaseConnection,
    paths: Arc<AppPaths>,
}

impl BloggerService {
    pub fn new(db: DatabaseConnection, paths: Arc<AppPaths>) -> Self {
        Self { db, paths }
    }

    /// 批量查询指定 UID，供数据库分页后的看板组装使用。
    pub async fn find_many_by_uids(&self, uids: &[String]) -> Result<Vec<blogger::Model>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(blogger::Entity::find()
            .filter(blogger::Column::Uid.is_in(uids.to_vec()))
            .all(&self.db)
            .await?)
    }

    /// 按全局默认保留数清理该博主的多余 history 与本地文件。
    ///
    /// - 保留数取全局设置 `storage.per_blogger_retain_default`。
    /// - 设置为 0 或缺失时不限制。
    /// - 数据库只返回超出保留范围的记录，不加载保留范围内的数据。
    /// - 文件删除失败只 warn，不阻塞 DB 删除。
    pub async fn enforce_retain(&self, uid: &str) -> Result<()> {
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
            .order_by_desc(history::Column::PubTimestamp)
            .offset(retain_limit as u64)
            .all(&self.db)
            .await?;
        if to_delete.is_empty() {
            return Ok(());
        }

        let download_dir = self.paths.download_dir.join(uid);
        info!(
            "[enforce_retain] 博主 {uid} 保留 {} 条，删除 {} 条",
            retain_limit,
            to_delete.len()
        );

        for h in &to_delete {
            let sidecar_dir = h
                .file_path
                .as_deref()
                .and_then(|path| std::path::Path::new(path).parent())
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| download_dir.clone());
            // 删除本地视频文件
            if let Some(p) = h.file_path.as_deref() {
                let path = PathBuf::from(p);
                if path.exists() {
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
                if path.exists() {
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
        let ids = to_delete.iter().map(|item| item.id).collect::<Vec<_>>();
        let bvids = to_delete
            .iter()
            .map(|item| item.bvid.clone())
            .collect::<Vec<_>>();
        let transaction = self.db.begin().await?;
        download_task::Entity::delete_many()
            .filter(download_task::Column::Bvid.is_in(bvids))
            .exec(&transaction)
            .await?;
        history::Entity::delete_many()
            .filter(history::Column::Id.is_in(ids))
            .exec(&transaction)
            .await?;
        transaction.commit().await?;
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

    /// 用户点击"知道了"后清空 `last_seen_*`（黄点消失）。
    pub async fn acknowledge_profile_change(&self, uid: &str) -> Result<()> {
        if let Some(b) = blogger::Entity::find()
            .filter(blogger::Column::Uid.eq(uid))
            .one(&self.db)
            .await?
        {
            let mut model: blogger::ActiveModel = b.into();
            model.last_seen_name = Set(None);
            model.last_seen_face = Set(None);
            model.last_seen_at = Set(None);
            model.updated_at = Set(Some(Local::now()));
            model.update(&self.db).await?;
        }
        Ok(())
    }

    pub async fn acknowledge_profile_changes(&self, uids: &[String]) -> Result<u64> {
        let uids = uids
            .iter()
            .map(|uid| uid.trim())
            .filter(|uid| !uid.is_empty())
            .collect::<Vec<_>>();
        if uids.is_empty() {
            return Ok(0);
        }
        let transaction = self.db.begin().await?;
        let mut affected = 0_u64;
        for uid in uids {
            if let Some(blogger) = blogger::Entity::find()
                .filter(blogger::Column::Uid.eq(uid))
                .one(&transaction)
                .await?
            {
                let mut model: blogger::ActiveModel = blogger.into();
                model.last_seen_name = Set(None);
                model.last_seen_face = Set(None);
                model.last_seen_at = Set(None);
                model.updated_at = Set(Some(Local::now()));
                model.update(&transaction).await?;
                affected += 1;
            }
        }
        transaction.commit().await?;
        Ok(affected)
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

    // ==================== API 层数据访问（handler 禁止直连 DB）====================

    /// 自动任务列表。仅在搜索页收藏、尚未创建任务的博主不会出现在这里。
    pub async fn list_auto_tasks(&self) -> Result<Vec<blogger::Model>> {
        Ok(blogger::Entity::find()
            .filter(blogger::Column::HasAutoTask.eq(true))
            .order_by_asc(blogger::Column::Id)
            .all(&self.db)
            .await?)
    }

    /// 搜索页“已添加博主”列表，与自动任务列表独立。
    pub async fn list_saved(&self) -> Result<Vec<blogger::Model>> {
        Ok(blogger::Entity::find()
            .filter(blogger::Column::IsSaved.eq(true))
            .order_by_asc(blogger::Column::Id)
            .all(&self.db)
            .await?)
    }

    pub async fn find_by_uid(&self, uid: &str) -> Result<Option<blogger::Model>> {
        Ok(blogger::Entity::find()
            .filter(blogger::Column::Uid.eq(uid))
            .one(&self.db)
            .await?)
    }

    pub async fn find_by_id(&self, id: i32) -> Result<Option<blogger::Model>> {
        Ok(blogger::Entity::find_by_id(id).one(&self.db).await?)
    }

    /// uid 是否已被其他博主（id 不同）占用。
    pub async fn uid_taken_by_other(&self, uid: &str, id: i32) -> Result<bool> {
        Ok(self
            .find_by_uid(uid)
            .await?
            .map(|existing| existing.id != id)
            .unwrap_or(false))
    }

    /// 新增博主记录，返回新记录。
    pub async fn add_blogger(&self, new: NewBlogger) -> Result<blogger::Model> {
        let next_check = if new.monitor_enabled {
            let now = Local::now();
            let windows = new
                .active_windows
                .as_deref()
                .map(crate::services::monitor::parse_windows)
                .unwrap_or_default();
            Some(
                if crate::services::monitor::is_within_active_window(now, &windows) {
                    now
                } else {
                    crate::services::monitor::next_window_start(now, &windows)
                },
            )
        } else {
            None
        };
        let model = blogger::ActiveModel {
            uid: Set(new.uid),
            name: Set(new.name),
            min_interval: Set(new.min_interval),
            max_interval: Set(new.max_interval),
            is_running: Set(new.monitor_enabled),
            next_check: Set(next_check),
            created_at: Set(Some(Local::now())),
            updated_at: Set(Some(Local::now())),
            face: Set(new.face),
            sign: Set(new.sign),
            level: Set(new.level),
            fans: Set(new.fans),
            download_video: Set(Some(new.download_video)),
            download_danmaku: Set(Some(new.download_danmaku)),
            download_comments: Set(Some(new.download_comments)),
            download_cover: Set(Some(new.download_cover)),
            burn_danmaku: Set(Some(new.burn_danmaku)),
            burn_subtitle: Set(Some(new.burn_subtitle)),
            series_filter_regex: Set(new.series_filter_regex),
            active_windows: Set(new.active_windows),
            is_saved: Set(new.is_saved),
            has_auto_task: Set(new.has_auto_task),
            ..Default::default()
        };
        Ok(model.insert(&self.db).await?)
    }

    /// 应用博主配置更新（字段级 patch；输入已由 API 层校验）。
    pub async fn apply_update(&self, current: blogger::Model, update: BloggerUpdate) -> Result<()> {
        let id = current.id;
        let current_monitor_enabled = current.is_running;
        let current_next_check = current.next_check;
        let current_active_windows = current.active_windows.clone();
        let final_monitor_enabled = update.monitor_enabled.unwrap_or(current_monitor_enabled);
        let final_active_windows = update
            .active_windows
            .clone()
            .unwrap_or(current_active_windows);
        let schedule_changed = update.monitor_enabled.is_some() || update.active_windows.is_some();
        let mut model: blogger::ActiveModel = current.into();
        if let Some(uid) = update.uid {
            model.uid = Set(uid);
        }
        if let Some(name) = update.name {
            model.name = Set(Some(name));
        }
        if let Some(min) = update.min_interval {
            model.min_interval = Set(min);
        }
        if let Some(max) = update.max_interval {
            model.max_interval = Set(max);
        }
        if let Some(v) = update.download_video {
            model.download_video = Set(Some(v));
        }
        if let Some(v) = update.download_danmaku {
            model.download_danmaku = Set(Some(v));
        }
        if let Some(v) = update.download_comments {
            model.download_comments = Set(Some(v));
        }
        if let Some(v) = update.download_cover {
            model.download_cover = Set(Some(v));
        }
        if let Some(v) = update.burn_danmaku {
            model.burn_danmaku = Set(Some(v));
        }
        if let Some(v) = update.burn_subtitle {
            model.burn_subtitle = Set(Some(v));
        }
        if let Some(v) = update.series_filter_regex {
            model.series_filter_regex = Set(Some(v));
        }
        if let Some(v) = update.active_windows {
            model.active_windows = Set(v);
        }
        if let Some(enabled) = update.monitor_enabled {
            model.is_running = Set(enabled);
        }
        if let Some(is_saved) = update.is_saved {
            model.is_saved = Set(is_saved);
        }
        if let Some(has_auto_task) = update.has_auto_task {
            model.has_auto_task = Set(has_auto_task);
        }
        if schedule_changed {
            let next = if final_monitor_enabled {
                let now = Local::now();
                let windows = final_active_windows
                    .as_deref()
                    .map(crate::services::monitor::parse_windows)
                    .unwrap_or_default();
                Some(
                    if !crate::services::monitor::is_within_active_window(now, &windows) {
                        crate::services::monitor::next_window_start(now, &windows)
                    } else {
                        match current_next_check {
                            Some(next)
                                if next > now
                                    && crate::services::monitor::is_within_active_window(
                                        next, &windows,
                                    ) =>
                            {
                                next
                            }
                            Some(next) if next > now && !windows.is_empty() => {
                                crate::services::monitor::next_window_start(next, &windows)
                            }
                            _ => now,
                        }
                    },
                )
            } else {
                None
            };
            model.next_check = Set(next);
        }
        model.updated_at = Set(Some(Local::now()));
        model.update(&self.db).await?;
        info!("更新博主配置成功: {id}");
        Ok(())
    }

    /// 删除自动任务配置。只移出自动任务集合，不删除搜索页收藏、历史或下载记录。
    pub async fn remove_auto_task(&self, id: i32) -> Result<Option<String>> {
        let Some(b) = blogger::Entity::find_by_id(id).one(&self.db).await? else {
            return Ok(None);
        };
        if !b.has_auto_task {
            return Ok(None);
        }
        let uid = b.uid.clone();
        let mut model: blogger::ActiveModel = b.into();
        model.has_auto_task = Set(false);
        model.is_running = Set(false);
        model.next_check = Set(None);
        model.updated_at = Set(Some(Local::now()));
        model.update(&self.db).await?;
        info!("删除自动任务成功: id={id} uid={uid}");
        Ok(Some(uid))
    }

    /// 从搜索页已添加列表移除；不会影响同一博主已有的自动任务。
    pub async fn remove_saved(&self, id: i32) -> Result<Option<String>> {
        let Some(b) = blogger::Entity::find_by_id(id).one(&self.db).await? else {
            return Ok(None);
        };
        if !b.is_saved {
            return Ok(None);
        }
        let uid = b.uid.clone();
        let mut model: blogger::ActiveModel = b.into();
        model.is_saved = Set(false);
        model.updated_at = Set(Some(Local::now()));
        model.update(&self.db).await?;
        info!("移除已添加博主成功: id={id} uid={uid}");
        Ok(Some(uid))
    }

    /// 启动/停止某博主的监控。启动时 next_check 置为当前时间以触发首次检查。
    pub async fn set_monitor_running(&self, uid: &str, running: bool) -> Result<MonitorToggle> {
        let Some(b) = self.find_by_uid(uid).await? else {
            return Ok(MonitorToggle::NotFound);
        };
        if !b.has_auto_task {
            return Ok(MonitorToggle::NotFound);
        }
        if b.is_running == running {
            return Ok(MonitorToggle::AlreadyInState);
        }
        let now = Local::now();
        let next_check = if running {
            let windows = b
                .active_windows
                .as_deref()
                .map(crate::services::monitor::parse_windows)
                .unwrap_or_default();
            if crate::services::monitor::is_within_active_window(now, &windows) {
                Some(now)
            } else {
                Some(crate::services::monitor::next_window_start(now, &windows))
            }
        } else {
            None
        };
        let mut model: blogger::ActiveModel = b.into();
        model.is_running = Set(running);
        model.next_check = Set(next_check);
        model.update(&self.db).await?;
        Ok(MonitorToggle::Updated)
    }
}
