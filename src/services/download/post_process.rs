//! 下载完成后处理：触发音视频合并并回写历史记录文件路径。

use crate::models::{download_task, history};
use crate::services::file_safety::sanitize_filename;
use anyhow::Result;
use chrono::Local;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use std::path::Path;
use tracing::{info, warn};

use super::{file_stem_for, task_cache_key, DownloadManager};

impl DownloadManager {
    pub(super) async fn on_task_completed(
        &self,
        bvid: &str,
        cid: Option<i64>,
        uid: Option<&str>,
    ) -> Result<()> {
        // 合并幂等键：单P为 bvid（行为不变），多P为 `{bvid}#{cid}`，避免同 bvid 不同分P互相阻断。
        let cache_key = task_cache_key(bvid, cid);
        // 幂等检查：避免 monitor_loop 多次轮询重复触发同一分P合并
        {
            let mut guard = self.lock_merge_set();
            if !guard.insert(cache_key.clone()) {
                // 已有合并任务进行中，跳过
                return Ok(());
            }
        }

        // 一次查询 bvid 下所有已完成任务（避免两次 select 间状态变化的 TOCTOU 竞态）
        // 多P时按具体 cid 隔离，单P按 cid IS NULL 匹配存量数据。
        let all_tasks = {
            let mut find = download_task::Entity::find()
                .filter(download_task::Column::Bvid.eq(bvid))
                .filter(download_task::Column::Status.eq("completed"));
            find = match cid {
                Some(c) => find.filter(download_task::Column::Cid.eq(c)),
                None => find.filter(download_task::Column::Cid.is_null()),
            };
            find.all(&self.db).await?
        };
        let video = all_tasks.iter().find(|t| t.task_type == "video").cloned();
        let audio = all_tasks.iter().find(|t| t.task_type == "audio").cloned();

        let (Some(video), Some(audio)) = (video, audio) else {
            self.lock_merge_set().remove(&cache_key);
            return Ok(());
        };

        let _uid = match uid {
            Some(u) if !u.is_empty() => Some(u.to_string()),
            _ => self.get_blogger_uid_from_history(bvid).await,
        };
        // 文件名词根：单P为 bvid，多P为 `{bvid}_p{page}`，与下载阶段一致。
        let stem = file_stem_for(bvid, video.page);
        // 优先使用 video task 存储的下载目录，避免跨天日期变化导致目录不匹配
        let dir = self.task_download_dir(&video).await;
        let v_path = dir.join(video.filename.as_deref().unwrap_or(&format!("{stem}.m4s")));
        let a_path = dir.join(audio.filename.as_deref().unwrap_or(&format!("{stem}.m4a")));

        if !v_path.exists() || !a_path.exists() {
            self.lock_merge_set().remove(&cache_key);
            return Ok(());
        }

        let title = video.title.as_deref().unwrap_or(bvid);
        let safe_title = sanitize_filename(title);
        // 合并容器与产物扩展：flac(Hi-Res)/ec3(杜比)音轨 → mkv；m4a → mp4（保持现状）。
        // 通过音频文件扩展名判定，避免对 download_task 加字段或额外探针。
        let audio_ext = audio
            .filename
            .as_deref()
            .and_then(|n| n.rsplit('.').next())
            .unwrap_or("m4a")
            .to_ascii_lowercase();
        let (container, out_ext) = match audio_ext.as_str() {
            "flac" | "ec3" => ("matroska", "mkv"),
            _ => ("mp4", "mp4"),
        };
        // 合并产物命名 `{title}_{stem}.{ext}`：单P=`{title}_{bvid}.mp4`（不变），多P带分P后缀。
        let output = dir.join(format!("{safe_title}_{stem}.{out_ext}"));

        let db = self.db.clone();
        let ws = self.ws.clone();
        let bvid_string = bvid.to_string();
        let output_for_cb = output.clone();
        let merge_in_progress_for_cb = self.merge_in_progress.clone();
        let merge_in_progress_for_match = self.merge_in_progress.clone();
        let cache_key_for_cb = cache_key.clone();
        let cb_cid = cid;
        let cb_page = video.page;
        let source_for_cb = video.source.clone().unwrap_or_else(|| "auto".to_string());
        let callback: Option<crate::services::video_processor::MergeCallback> = Some(Box::new(
            move |result| {
                let db = db.clone();
                let ws = ws.clone();
                let bvid = bvid_string.clone();
                let output = output_for_cb.clone();
                let merge_in_progress = merge_in_progress_for_cb.clone();
                let cache_key_for_cb = cache_key_for_cb.clone();
                let source = source_for_cb.clone();
                tokio::spawn(async move {
                    // 合并任务结束，释放幂等标记
                    {
                        let mut guard = match merge_in_progress.lock() {
                            Ok(guard) => guard,
                            Err(error) => {
                                let mut guard = error.into_inner();
                                guard.clear();
                                guard
                            }
                        };
                        guard.remove(&cache_key_for_cb);
                    }
                    if result.success {
                        if let Err(e) = Self::update_history_after_merge_static(
                            &db, &bvid, cb_cid, cb_page, &output, &source,
                        )
                        .await
                        {
                            warn!("更新历史记录失败 {bvid}: {e}");
                        }
                    }
                    let payload = serde_json::json!({
                        "bvid": bvid,
                        "cid": cb_cid,
                        "page": cb_page,
                        "type": "video",
                        "status": if result.success { "merged" } else { "merge_failed" },
                        "progress_percent": if result.success { 100 } else { 0 },
                        "downloaded_size": 0,
                        "total_size": 0,
                        "speed": 0,
                        "success": result.success,
                        "message": result.message,
                        "output_path": result.output_path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    });
                    if let Err(error) = ws.broadcast_download_progress(payload).await {
                        warn!("推送合并结果失败 bvid={bvid}: {error}");
                    }
                });
            },
        ));

        match self
            .video_processor
            .merge_and_cleanup(&v_path, &a_path, &output, container, callback)
            .await
        {
            Ok(result) if result.success => {
                info!("已启动音视频合并任务 {bvid}: {}", output.display());
            }
            Ok(result) => {
                warn!("合并任务未成功启动 {bvid}: {}", result.message);
                // 合并未成功启动，释放幂等标记（回调可能已释放，幂等）
                let guard = merge_in_progress_for_match.lock();
                let mut guard = guard.unwrap_or_else(|e| e.into_inner());
                guard.remove(&cache_key);
            }
            Err(e) => {
                warn!("合并失败 {bvid}: {e}");
                let guard = merge_in_progress_for_match.lock();
                let mut guard = guard.unwrap_or_else(|e| e.into_inner());
                guard.remove(&cache_key);
            }
        }
        Ok(())
    }

    /// 合并成功后以最终 MP4 为准同步 history。
    ///
    /// 自动产物优先作为当前产物；自动产物仍存在时，后续手动副本不会夺走当前指针，
    /// 但会由详情扫描作为独立产物展示。
    pub(super) async fn update_history_after_merge_static(
        db: &DatabaseConnection,
        bvid: &str,
        cid: Option<i64>,
        page: Option<i32>,
        path: &Path,
        source: &str,
    ) -> Result<()> {
        let mut query = history::Entity::find().filter(history::Column::Bvid.eq(bvid));
        query = match cid {
            Some(cid) => query.filter(history::Column::Cid.eq(cid)),
            None => query.filter(history::Column::Cid.is_null()),
        };
        let h = query.one(db).await?;
        if let Some(h) = h {
            let current_exists = h
                .file_path
                .as_deref()
                .is_some_and(|current| Path::new(current).exists());
            let should_promote = source != "manual" || h.source != "auto" || !current_exists;
            if !should_promote {
                return Ok(());
            }

            let digest = crate::services::file_safety::stream_file_md5(path).await?;
            let mut cover_local_path = h.cover_local_path.clone();
            if let (Some(existing_cover), Some(output_dir)) =
                (h.cover_local_path.as_deref(), path.parent())
            {
                let existing_cover = Path::new(existing_cover);
                if existing_cover.exists() {
                    if let Some(filename) = existing_cover.file_name() {
                        let target = output_dir.join(filename);
                        if target != existing_cover {
                            if !target.exists() {
                                tokio::fs::copy(existing_cover, &target).await?;
                            }
                            cover_local_path = Some(target.to_string_lossy().to_string());
                        }
                    }
                }
            }

            let mut model: history::ActiveModel = h.into();
            model.cid = Set(cid);
            model.page = Set(page);
            model.file_path = Set(Some(path.to_string_lossy().to_string()));
            model.cover_local_path = Set(cover_local_path);
            model.download_time = Set(Some(Local::now()));
            model.md5 = Set(Some(digest));
            model.md5_last_checked_at = Set(Some(Local::now()));
            model.state = Set(Some("completed".to_string()));
            if source != "manual" {
                model.source = Set("auto".to_string());
            }
            model.update(db).await?;
        }
        Ok(())
    }
}
