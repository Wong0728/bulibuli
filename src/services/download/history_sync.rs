//! 历史记录同步：完成任务写入 history（UPSERT）、SHA-256 记录与历史/终态任务清理。

use crate::models::{download_task, history};
use crate::services::file_safety::ensure_existing_within_root;
use anyhow::Result;
use chrono::Local;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use std::path::{Path, PathBuf};
use tracing::warn;

use super::DownloadManager;

/// 终态任务状态全集：history 清理时级联回收对应 download_tasks 行，
/// 以及终态任务的保留上限判定都引用此处。
const TERMINAL_TASK_STATUSES: [&str; 3] = ["completed", "failed", "cancelled"];

impl DownloadManager {
    #[allow(clippy::too_many_arguments)]
    pub async fn add_to_history(
        &self,
        bvid: &str,
        file_path: Option<&Path>,
        uid: Option<&str>,
        pub_timestamp: Option<i64>,
        cookies: Option<&str>,
        source: &str,
        cid: Option<i64>,
        page: Option<i32>,
        part_title: Option<&str>,
    ) -> Result<()> {
        let mut existing_query = history::Entity::find().filter(history::Column::Bvid.eq(bvid));
        existing_query = match cid {
            Some(cid) => existing_query.filter(history::Column::Cid.eq(cid)),
            None => existing_query.filter(history::Column::Cid.is_null()),
        };
        let existing = existing_query.one(&self.db).await?;
        let requested_source = if source == "manual" { "manual" } else { "auto" };
        // 同一 BV 同时存在自动与手动产物时，history.source 保持 auto，
        // 防止一次手动补下载把自动侧车调度永久排除。
        let source = if requested_source == "auto"
            || existing
                .as_ref()
                .is_some_and(|history| history.source == "auto")
        {
            "auto"
        } else {
            "manual"
        };
        if let Some(existing) = existing.as_ref() {
            // 已完成记录只同步本次任务来源；其余完成字段仍由首次完成事件维护。
            if existing.download_time.is_some() {
                if existing.source != source {
                    let mut model: history::ActiveModel = existing.clone().into();
                    model.source = Set(source.to_string());
                    model.update(&self.db).await?;
                }
                return Ok(());
            }
        }

        // 新记录或未完成占位记录统一补齐视频元数据；调度游标字段由占位记录保留。
        let mut uid_to_save = existing
            .as_ref()
            .and_then(|h| h.uid.clone())
            .or_else(|| uid.map(str::to_string));
        let mut title = existing.as_ref().and_then(|h| h.title.clone());
        let mut pub_ts = existing
            .as_ref()
            .and_then(|h| h.pub_timestamp)
            .or(pub_timestamp);
        let mut pub_date = existing
            .as_ref()
            .and_then(|h| h.pub_date.clone())
            .or_else(|| pub_ts.and_then(crate::services::time_format::pub_date));
        let mut pic = existing.as_ref().and_then(|h| h.pic.clone());
        let mut duration = existing.as_ref().and_then(|h| h.duration);
        let mut view = existing.as_ref().and_then(|h| h.view);
        let mut owner_name = existing.as_ref().and_then(|h| h.owner_name.clone());
        let mut owner_face = existing.as_ref().and_then(|h| h.owner_face.clone());

        let info_result = self
            .bili_api
            .get_video_info(bvid, cookies.unwrap_or(""))
            .await;
        if let Err(e) = &info_result {
            // 静默失败会让记录以 uid/owner 为空落库，看板归入 "unknown" 分组，必须留痕。
            warn!("[add_to_history] 获取视频信息失败 {bvid}: {e}");
        }
        if let Ok(info) = info_result {
            title = Some(info.title.clone());
            if info.owner.mid != 0 {
                uid_to_save = Some(info.owner.mid.to_string());
            }
            // UP 主名字/头像快照：未监控博主的看板分组靠它显示（避免只剩纯数字 UID）
            owner_name = Some(info.owner.name.clone()).filter(|s| !s.is_empty());
            owner_face = Some(info.owner.face.clone())
                .filter(|s| !s.is_empty())
                .map(|s| {
                    // 协议相对地址补全（与 get_user_info 的 face 处理保持一致）
                    if s.starts_with("http") {
                        s
                    } else {
                        format!("https:{s}")
                    }
                });
            if let Some(ts) = Some(info.created).filter(|&ts| ts > 0).or(pub_timestamp) {
                pub_ts = Some(ts);
                pub_date = Some(crate::services::time_format::pub_date(ts).unwrap_or_default());
            }
            pic = Some(info.pic.clone());
            duration = Some(info.duration);
            view = Some(info.stat.view);
        }

        // 封面优先保存到视频实际目录，兼容 manual 与自定义路径模板。
        let cover_dir = file_path.and_then(Path::parent);
        let mut cover_local_path = existing.as_ref().and_then(|h| h.cover_local_path.clone());
        match self
            .ensure_cover_local_to(bvid, uid_to_save.as_deref(), cover_dir)
            .await
        {
            Ok(path) => {
                if let Some(p) = path {
                    cover_local_path = Some(p.to_string_lossy().to_string());
                }
            }
            Err(e) => {
                // 封面落盘失败不阻塞主记录写入：cover_local_path 留空即为可重试标记，
                // 后续 ensure_cover_local / api/cover 按需重新下载封面。
                warn!("[add_to_history] 下载封面失败 {bvid}: {e}");
            }
        }

        // on_completion 模式下立即计算 SHA-256。放在事务外执行：流式读全文件是
        // 耗时 IO，避免长时间持有 SQLite 写事务阻塞其他写入方。
        let mut sha256_digest: Option<String> = None;
        if let Some(path) = file_path {
            if path.exists() {
                match crate::services::file_safety::stream_file_sha256(path).await {
                    Ok(digest) => sha256_digest = Some(digest),
                    Err(e) => warn!("[add_to_history] 计算 SHA-256 失败 {bvid}: {e}"),
                }
            }
        }

        // 使用 UPSERT 原子写入 history：
        // - 解决 video/audio 并发完成时的插入竞态；
        // - 新唯一索引 uix_history_bvid_cid 已去掉 source，DO UPDATE 中按晋升规则处理 source；
        // - next_download_index / next_sidecar_at / sidecar_attempts 等调度游标在冲突时保留原值。
        let now = Local::now();
        let sha256_last_checked_at = sha256_digest.as_ref().map(|_| now);
        self.db
            .execute_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "INSERT INTO history (
                    uid, bvid, cid, page, part_title, source, title, pub_date, pub_timestamp,
                    download_time, file_path, next_download_index, pic, duration, view, state,
                    cover_local_path, view_source, owner_name, owner_face, sha256,
                    sha256_last_checked_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(bvid, COALESCE(cid, -1)) DO UPDATE SET
                    uid = excluded.uid,
                    cid = excluded.cid,
                    page = excluded.page,
                    part_title = excluded.part_title,
                    source = CASE WHEN history.source = 'auto' OR excluded.source = 'auto' THEN 'auto' ELSE 'manual' END,
                    title = excluded.title,
                    pub_date = excluded.pub_date,
                    pub_timestamp = excluded.pub_timestamp,
                    download_time = excluded.download_time,
                    file_path = excluded.file_path,
                    next_download_index = MAX(history.next_download_index, excluded.next_download_index),
                    pic = excluded.pic,
                    duration = excluded.duration,
                    view = excluded.view,
                    state = excluded.state,
                    cover_local_path = excluded.cover_local_path,
                    view_source = excluded.view_source,
                    owner_name = excluded.owner_name,
                    owner_face = excluded.owner_face,
                    sha256 = excluded.sha256,
                    sha256_last_checked_at = excluded.sha256_last_checked_at",
                [
                    uid_to_save.into(),
                    bvid.to_string().into(),
                    cid.into(),
                    page.into(),
                    part_title.map(str::to_string).into(),
                    source.to_string().into(),
                    title.into(),
                    pub_date.into(),
                    pub_ts.into(),
                    Some(now).into(),
                    file_path.map(|p| p.to_string_lossy().to_string()).into(),
                    0i32.into(),
                    pic.into(),
                    duration.into(),
                    view.into(),
                    "completed".to_string().into(),
                    cover_local_path.into(),
                    Some("snapshot".to_string()).into(),
                    owner_name.into(),
                    owner_face.into(),
                    sha256_digest.into(),
                    sha256_last_checked_at.into(),
                ],
            ))
            .await?;

        if let Err(e) = self.cleanup_history().await {
            warn!("清理历史记录失败: {e}");
        }

        Ok(())
    }

    async fn cleanup_history(&self) -> Result<()> {
        // 设置页 storage.history_limit 优先，环境变量 config.history_limit 兜底
        //（审计 B11：此前设置页改了不生效，只有环境变量能改）。
        let settings_limit = self.settings_service.current().storage.history_limit;
        let limit = if settings_limit > 0 {
            settings_limit
        } else {
            self.config.history_limit
        };
        let count = history::Entity::find().count(&self.db).await?;
        if count > (limit as u64 * 11 / 10) {
            let to_delete: Vec<history::Model> = history::Entity::find()
                // NULL download_time（pending 占位）按 SQLite 规则视为最小值，
                // DESC 排序会落到最旧一端、最先进入删除窗口；显式让 NULL 最后。
                .order_by_asc(history::Column::DownloadTime.is_null())
                .order_by_desc(history::Column::DownloadTime)
                .offset(limit as u64)
                .limit(10000)
                .all(&self.db)
                .await?;
            if !to_delete.is_empty() {
                // 审计修复：删除记录时同步清理本地封面文件，避免孤儿封面累积。
                // 设计意图：只删封面（派生缓存，可随时重新下载），媒体文件保留——
                // history 是展示/索引层，清理记录不代表删除用户已下载的视频内容。
                for h in &to_delete {
                    if let Some(cover) = h.cover_local_path.as_deref() {
                        let path = PathBuf::from(cover);
                        // 审计 DB3：cover_local_path 来自历史库，旧数据可能残留越界
                        // 路径；删除前必须确认仍位于下载根目录内，防止误删根外用户文件。
                        if let Err(e) =
                            ensure_existing_within_root(&self.paths.download_dir, &path).await
                        {
                            warn!(
                                "[cleanup_history] 封面路径越界，跳过删除 {} ({}): {e}",
                                h.bvid,
                                path.display()
                            );
                            continue;
                        }
                        if path.is_file() {
                            if let Err(e) = tokio::fs::remove_file(&path).await {
                                warn!(
                                    "[cleanup_history] 删除封面失败 {} ({}): {e}",
                                    h.bvid,
                                    path.display()
                                );
                            }
                        }
                    }
                }
                let ids: Vec<i32> = to_delete.iter().map(|h| h.id).collect();
                let pairs: Vec<(String, Option<i64>)> =
                    to_delete.iter().map(|h| (h.bvid.clone(), h.cid)).collect();
                history::Entity::delete_many()
                    .filter(history::Column::Id.is_in(ids))
                    .exec(&self.db)
                    .await?;
                // 审计 DB2：history 行被清理后，对应任务的终态 download_tasks 行
                // 随之级联回收，否则自动监控长期运行会每视频累积两行且永不释放。
                self.delete_terminal_tasks_for(&pairs).await?;
            }
        }
        // 审计 DB2：终态任务总量同样受 history_limit 约束（与 history 相同的
        // 10% 余量），超限时按更新时间最旧优先删除，避免任务表随运行时长无限增长、
        // 看板/统计查询持续变慢。
        let terminal_count = download_task::Entity::find()
            .filter(download_task::Column::Status.is_in(TERMINAL_TASK_STATUSES))
            .count(&self.db)
            .await?;
        if terminal_count > (limit as u64 * 11 / 10) {
            let stale_ids: Vec<i32> = download_task::Entity::find()
                .filter(download_task::Column::Status.is_in(TERMINAL_TASK_STATUSES))
                .order_by_asc(download_task::Column::UpdatedAt)
                .offset(limit as u64)
                .limit(10000)
                .all(&self.db)
                .await?
                .into_iter()
                .map(|t| t.id)
                .collect();
            if !stale_ids.is_empty() {
                download_task::Entity::delete_many()
                    .filter(download_task::Column::Id.is_in(stale_ids))
                    .exec(&self.db)
                    .await?;
            }
        }
        Ok(())
    }

    /// 删除与指定 history 记录 (bvid, cid) 对应、处于终态的 download_tasks 行。
    /// 仅删终态行：活跃/暂停中的任务（如用户手动补下载）不受历史清理影响。
    async fn delete_terminal_tasks_for(&self, pairs: &[(String, Option<i64>)]) -> Result<()> {
        if pairs.is_empty() {
            return Ok(());
        }
        let mut any_pair = Condition::any();
        for (bvid, cid) in pairs {
            let mut pair_condition =
                Condition::all().add(download_task::Column::Bvid.eq(bvid.clone()));
            pair_condition = match cid {
                Some(cid) => pair_condition.add(download_task::Column::Cid.eq(*cid)),
                None => pair_condition.add(download_task::Column::Cid.is_null()),
            };
            any_pair = any_pair.add(pair_condition);
        }
        download_task::Entity::delete_many()
            .filter(any_pair)
            .filter(download_task::Column::Status.is_in(TERMINAL_TASK_STATUSES))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    pub(super) async fn get_blogger_uid_from_history(&self, bvid: &str) -> Option<String> {
        history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await
            .ok()
            .flatten()
            .and_then(|h| h.uid)
    }
}
