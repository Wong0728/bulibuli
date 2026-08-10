//! 历史记录数据访问：查询、删除、FTS5 搜索与烧录状态更新。

use crate::error::AppResult;
use crate::models::{download_task, history};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::{info, warn};

use super::{BoardPage, HistoryCounts, HistoryService};

impl HistoryService {
    /// 按 bvid 查询单条历史记录。
    pub async fn find_by_bvid(&self, bvid: &str) -> AppResult<Option<history::Model>> {
        Ok(history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await?)
    }

    /// 按 uid 查询该博主的历史记录（按下载时间倒序）。
    pub async fn list_by_uid(&self, uid: &str) -> AppResult<Vec<history::Model>> {
        Ok(history::Entity::find()
            .filter(history::Column::Uid.eq(uid))
            .order_by_desc(history::Column::DownloadTime)
            .all(&self.db)
            .await?)
    }

    /// 看板查询：状态过滤、排序、计数和分页均在数据库完成。
    pub async fn board_page(&self, tab: &str, page: u64, page_size: u64) -> AppResult<BoardPage> {
        let active_tasks = download_task::Entity::find()
            .select_only()
            .column(download_task::Column::Bvid)
            .filter(download_task::Column::Status.is_in(["pending", "downloading"]))
            .into_tuple::<String>()
            .all(&self.db)
            .await?;
        let mut active_bvids = active_tasks;
        active_bvids.sort();
        active_bvids.dedup();

        let mut query = history::Entity::find();
        query = match tab {
            "downloading" if active_bvids.is_empty() => {
                query.filter(history::Column::Id.eq(i32::MIN))
            }
            "downloading" => query.filter(history::Column::Bvid.is_in(active_bvids.clone())),
            "failed" if active_bvids.is_empty() => {
                query.filter(history::Column::State.eq("failed"))
            }
            "failed" => query.filter(
                Condition::all()
                    .add(history::Column::State.eq("failed"))
                    .add(history::Column::Bvid.is_not_in(active_bvids.clone())),
            ),
            _ if active_bvids.is_empty() => query.filter(history::Column::State.is_in([
                "completed",
                "removed",
                "pay_blocked",
                "tampered",
            ])),
            _ => query.filter(
                Condition::all()
                    .add(history::Column::State.is_in([
                        "completed",
                        "removed",
                        "pay_blocked",
                        "tampered",
                    ]))
                    .add(history::Column::Bvid.is_not_in(active_bvids.clone())),
            ),
        };

        let total = query.clone().count(&self.db).await?;
        let histories = query
            .order_by_desc(history::Column::PubTimestamp)
            .order_by_desc(history::Column::Id)
            .offset((page - 1) * page_size)
            .limit(page_size)
            .all(&self.db)
            .await?;

        let mut grouped_query = history::Entity::find()
            .select_only()
            .column(history::Column::Uid)
            .column(history::Column::State)
            .column_as(history::Column::Id.count(), "count")
            .group_by(history::Column::Uid)
            .group_by(history::Column::State);
        if !active_bvids.is_empty() {
            grouped_query =
                grouped_query.filter(history::Column::Bvid.is_not_in(active_bvids.clone()));
        }
        let grouped = grouped_query
            .into_tuple::<(Option<String>, Option<String>, i64)>()
            .all(&self.db)
            .await?;
        let mut counts_by_uid: HashMap<String, HistoryCounts> = HashMap::new();
        for (uid, state, count) in grouped {
            let counts = counts_by_uid
                .entry(uid.unwrap_or_else(|| "unknown".to_string()))
                .or_default();
            match state.as_deref().unwrap_or("completed") {
                "completed" | "tampered" => counts.completed += count,
                "failed" => counts.failed += count,
                "removed" => counts.removed += count,
                "pay_blocked" => counts.pay_blocked += count,
                _ => counts.completed += count,
            }
        }
        if !active_bvids.is_empty() {
            let active_owners = history::Entity::find()
                .select_only()
                .column(history::Column::Uid)
                .column(history::Column::Bvid)
                .filter(history::Column::Bvid.is_in(active_bvids.clone()))
                .into_tuple::<(Option<String>, String)>()
                .all(&self.db)
                .await?;
            for (uid, _) in active_owners {
                counts_by_uid
                    .entry(uid.unwrap_or_else(|| "unknown".to_string()))
                    .or_default()
                    .downloading += 1;
            }
        }

        Ok(BoardPage {
            histories,
            total,
            counts_by_uid,
        })
    }

    /// 只查询当前页视频对应的下载任务。
    pub async fn download_tasks_for_bvids(
        &self,
        bvids: &[String],
    ) -> AppResult<Vec<download_task::Model>> {
        if bvids.is_empty() {
            return Ok(Vec::new());
        }
        Ok(download_task::Entity::find()
            .filter(download_task::Column::Bvid.is_in(bvids.to_vec()))
            .all(&self.db)
            .await?)
    }

    /// 某 bvid 的全部下载任务（抽屉详情用）。
    pub async fn download_tasks_for_bvid(&self, bvid: &str) -> Vec<download_task::Model> {
        download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .all(&self.db)
            .await
            .unwrap_or_default()
    }

    /// 删除单条视频记录及其关联数据。
    ///
    /// 行为：
    /// 1. `delete_files=true` 时删除本地视频/封面/弹幕/字幕侧车文件
    /// 2. 删除 `download_task` 表中该 bvid 的所有任务
    /// 3. 删除 `history` 表中该 bvid 的记录
    ///
    /// 返回 `(removed_files, removed_tasks)`；记录不存在时返回 `Ok(None)`。
    pub async fn delete_record(
        &self,
        bvid: &str,
        delete_files: bool,
    ) -> AppResult<Option<(Vec<String>, u64)>> {
        let h = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await?;
        let Some(h) = h else {
            return Ok(None);
        };

        let mut removed_files: Vec<String> = Vec::new();
        if delete_files {
            // 删除详情页能够发现的全部重复产物，覆盖 manual、UID 和归档目录。
            let discovered = self
                .scan_files(bvid, h.uid.as_deref(), h.file_path.as_deref())
                .await;
            let mut seen = HashSet::new();
            for file in discovered {
                let Some(path) = self.resolve_download_relative_path(&file.path) else {
                    continue;
                };
                if seen.insert(path.clone()) {
                    Self::remove_file_logged(bvid, &path, "产物", &mut removed_files).await;
                }
            }
            // 兼容 history 中指向下载根目录外的旧绝对路径，仅删除明确记录的两个文件。
            for path in [
                h.file_path.as_deref().map(Path::new),
                h.cover_local_path.as_deref().map(Path::new),
            ]
            .into_iter()
            .flatten()
            {
                if seen.insert(path.to_path_buf()) {
                    Self::remove_file_logged(bvid, path, "历史记录", &mut removed_files).await;
                }
            }
        }

        // 4. 删除 download_task
        let deleted_tasks = download_task::Entity::delete_many()
            .filter(download_task::Column::Bvid.eq(bvid))
            .exec(&self.db)
            .await?;
        info!(
            "[delete_history] {bvid} 级联清理 download_task: {} 条",
            deleted_tasks.rows_affected
        );

        // 5. 删除 history 记录
        history::Entity::delete_by_id(h.id).exec(&self.db).await?;
        info!(
            "[delete_history] {bvid} 已删除记录，文件 {} 个",
            removed_files.len()
        );
        Ok(Some((removed_files, deleted_tasks.rows_affected)))
    }

    async fn remove_file_logged(
        bvid: &str,
        path: &Path,
        label: &str,
        removed_files: &mut Vec<String>,
    ) {
        if !path.exists() {
            return;
        }
        if let Err(e) = tokio::fs::remove_file(path).await {
            warn!(
                "[delete_history] 删除{label}文件失败 {} {}: {e}",
                bvid,
                path.display()
            );
        } else {
            removed_files.push(path.to_string_lossy().to_string());
        }
    }

    /// 使用 FTS5 分页搜索；无结果直接返回空，不再全表载入后做内存扫描。
    pub async fn search(
        &self,
        keyword: &str,
        page: u64,
        page_size: u64,
    ) -> AppResult<(Vec<history::Model>, u64)> {
        let keyword = keyword.trim().to_lowercase();
        if keyword.is_empty() {
            return Ok((Vec::new(), 0));
        }

        let sanitized = sanitize_fts5_query(&keyword);
        if sanitized.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let fts_match = format!("\"{sanitized}\"");
        let backend = self.db.get_database_backend();
        let count_row = self
            .db
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) AS count FROM history_fts WHERE history_fts MATCH ?".to_string(),
                [sea_orm::Value::from(fts_match.clone())],
            ))
            .await?;
        let total = count_row
            .and_then(|row| row.try_get::<i64>("", "count").ok())
            .unwrap_or(0)
            .max(0) as u64;
        let rows = self
            .db
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                backend,
                "SELECT h.* FROM history h JOIN history_fts fts ON h.id = fts.rowid \
                 WHERE history_fts MATCH ? ORDER BY h.download_time DESC LIMIT ? OFFSET ?"
                    .to_string(),
                [
                    sea_orm::Value::from(fts_match),
                    sea_orm::Value::from(page_size as i64),
                    sea_orm::Value::from(((page - 1) * page_size) as i64),
                ],
            ))
            .await?;
        use sea_orm::FromQueryResult;
        let models = rows
            .iter()
            .filter_map(|row| history::Model::from_query_result(row, "").ok())
            .collect();
        Ok((models, total))
    }

    /// 更新某 bvid 的烧录状态与产物路径（烧录完成后调用）。
    pub async fn mark_burned(
        &self,
        bvid: &str,
        source: &str,
        output_path: Option<&Path>,
    ) -> AppResult<()> {
        use sea_orm::{ActiveModelTrait, Set};
        let h = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await?;
        let Some(h) = h else {
            return Ok(());
        };
        let mut model: history::ActiveModel = h.into();
        match source {
            "danmaku" => {
                model.burned_danmaku = Set(Some(true));
            }
            "subtitle" => {
                model.burned_subtitle = Set(Some(true));
            }
            _ => {
                model.burned_danmaku = Set(Some(true));
                model.burned_subtitle = Set(Some(true));
            }
        }
        if let Some(p) = output_path {
            model.file_path = Set(Some(p.to_string_lossy().to_string()));
        }
        model.auto_burn_status = Set(Some("completed".to_string()));
        model.auto_burn_next_retry_at = Set(None);
        model.update(&self.db).await?;
        Ok(())
    }
}

/// FTS5 查询安全清理：移除所有 FTS5 操作符和特殊语法字符，
/// 只保留字母、数字、CJK 字符和空格，防止 MATCH 注入。
fn sanitize_fts5_query(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            c.is_alphanumeric()
                || c.is_whitespace()
                || *c == '_'
                || *c == '-'
                // 保留 CJK 字符用于中文搜索
                || ('\u{4E00}'..='\u{9FFF}').contains(c)
                || ('\u{3400}'..='\u{4DBF}').contains(c)
                || ('\u{F900}'..='\u{FAFF}').contains(c)
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
