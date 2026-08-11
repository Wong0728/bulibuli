//! 音频失败自动重试：视频完成后按指数退避重试音频，耗尽后降级为纯视频 remux。

use crate::models::download_task;
use crate::services::file_safety::sanitize_filename;
use chrono::{Duration, Local};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QuerySelect, Set};
use std::collections::HashSet;
use tracing::{error, info, warn};

use super::engine::TransferEngine;
use super::DownloadManager;

impl DownloadManager {
    /// 音频重试耗尽后，降级为纯视频合并（ffmpeg remux m4s 为 mp4，无音频）
    async fn fallback_video_only_merge(&self, bvid: &str) {
        // 查找视频任务
        let video_task = match download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(download_task::Column::TaskType.eq("video"))
            .filter(download_task::Column::Status.eq("completed"))
            .one(&self.db)
            .await
        {
            Ok(Some(t)) => t,
            _ => return,
        };

        let dir = self.task_download_dir(&video_task).await;
        let default_filename = format!("{bvid}.m4s");
        let v_filename = video_task.filename.as_deref().unwrap_or(&default_filename);
        let v_path = dir.join(v_filename);
        if !v_path.exists() {
            return;
        }

        let title = video_task.title.as_deref().unwrap_or(bvid);
        let safe_title = sanitize_filename(title);
        let output = dir.join(format!("{safe_title}_{bvid}.mp4"));

        info!("[音频降级] {bvid} 音频重试耗尽，执行纯视频 remux（无音频）");

        // 使用 ffmpeg 将 m4s remux 为 mp4（单输入、-an 仅保留视频流），
        // 同步等待完成，确认成功后才更新历史记录
        match self
            .video_processor
            .remux_video_only(&v_path, &output)
            .await
        {
            Ok(()) => {
                info!("[音频降级] {bvid} 纯视频 remux 完成: {}", output.display());
                // 更新历史记录
                let source = video_task.source.as_deref().unwrap_or("auto");
                if let Err(e) =
                    Self::update_history_after_merge_static(&self.db, bvid, &output, source).await
                {
                    warn!("[音频降级] 更新历史记录失败 {bvid}: {e}");
                }
                self.log_bvid(
                    bvid,
                    None,
                    "音频下载失败，已降级为纯视频（无声音）",
                    "warning",
                )
                .await;
                if let Ok(Some(audio_task)) = download_task::Entity::find()
                    .filter(download_task::Column::Bvid.eq(bvid))
                    .filter(download_task::Column::TaskType.eq("audio"))
                    .one(&self.db)
                    .await
                {
                    let mut model: download_task::ActiveModel = audio_task.into();
                    model.status = Set("degraded".to_string());
                    model.attempts = Set(4);
                    model.next_retry_at = Set(None);
                    if let Err(error) = model.update(&self.db).await {
                        warn!("[音频自动重试] 更新降级状态失败: {error}");
                    }
                }
            }
            Err(e) => {
                warn!("[音频降级] {bvid} 纯视频 remux 失败: {e}");
            }
        }
    }

    /// 检查并自动重试失败的音频任务（仅当对应视频已完成时）
    pub(super) async fn check_audio_retry(&self) {
        // 查找失败的音频任务
        let failed_audio = match download_task::Entity::find()
            .filter(download_task::Column::TaskType.eq("audio"))
            .filter(download_task::Column::Status.eq("failed"))
            .all(&self.db)
            .await
        {
            Ok(tasks) => tasks,
            Err(e) => {
                error!("查询失败音频任务失败: {e}");
                return;
            }
        };
        let failed_bvids = failed_audio
            .iter()
            .map(|task| task.bvid.clone())
            .collect::<Vec<_>>();
        let completed_video_bvids = if failed_bvids.is_empty() {
            HashSet::new()
        } else {
            download_task::Entity::find()
                .select_only()
                .column(download_task::Column::Bvid)
                .filter(download_task::Column::Bvid.is_in(failed_bvids))
                .filter(download_task::Column::TaskType.eq("video"))
                .filter(download_task::Column::Status.eq("completed"))
                .into_tuple::<String>()
                .all(&self.db)
                .await
                .unwrap_or_default()
                .into_iter()
                .collect::<HashSet<_>>()
        };

        for audio_task in failed_audio {
            let bvid = audio_task.bvid.clone();

            let attempts = audio_task.attempts.max(0) as u32;
            if attempts >= 3 {
                // 已达最大重试次数，触发纯视频降级合并
                info!(
                    "[音频自动重试] {bvid} 已达最大重试次数 {}, 执行纯视频降级",
                    attempts
                );
                self.fallback_video_only_merge(&bvid).await;
                continue;
            }

            // 检查该 bvid 的视频任务是否已完成
            let video_done = completed_video_bvids.contains(&bvid);
            if !video_done {
                // 视频未完成，跳过音频重试（等视频完成后再试）
                continue;
            }

            // 检查冷却时间（指数退避）
            let now = Local::now();
            let ready = audio_task
                .next_retry_at
                .is_none_or(|retry_at| now >= retry_at);
            if !ready {
                // 未到重试时间，跳过本轮
                continue;
            }

            // 详细日志：开始重试前记录关键状态
            info!(
                "[音频自动重试] {bvid} 检查通过: attempts={}, video_done={}, ready={}",
                attempts, video_done, ready
            );

            info!("[音频自动重试] {bvid} 尝试第 {} 次重试", attempts + 1);

            // 使用当前登录态重新获取带时效签名的音频 URL。
            let cookies = self
                .settings_service
                .cookie_header()
                .await
                .unwrap_or_default();
            let audio_url = match self
                .bili_api
                .get_audio_url(
                    &bvid,
                    None,
                    &cookies,
                    &self
                        .settings_service
                        .current()
                        .query
                        .audio_quality_preference,
                )
                .await
            {
                Ok(Some(audio)) if !audio.audio_url.is_empty() => audio.audio_url,
                result => {
                    // 解析 B 站错误码：不可重试错误（风控 -352/-403、视频删除 -404、
                    // 充电/地区限制、未登录 -101）直接终止自动重试并降级为纯视频，
                    // 不可重试错误直接降级，避免持续触发风控。
                    if let Err(e) = &result {
                        if let Some(bili) = e.downcast_ref::<crate::error::BiliApiError>() {
                            if !bili.retryable {
                                warn!("[音频自动重试] {bvid} 遇不可重试错误(kind={:?}, code={})，停止重试并降级为纯视频", bili.kind, bili.code);
                                self.persist_task_error_kind(
                                    &bvid,
                                    "audio",
                                    &format!("{:?}", bili.kind),
                                )
                                .await;
                                let mut model: download_task::ActiveModel =
                                    audio_task.clone().into();
                                model.attempts = Set(3);
                                model.next_retry_at = Set(Some(now));
                                if let Err(error) = model.update(&self.db).await {
                                    warn!("[音频自动重试] 更新重试状态失败: {error}");
                                }
                                continue;
                            }
                        }
                    }
                    warn!("[音频自动重试] {bvid} 获取音频 URL 失败，将在下次循环重试");
                    let mut model: download_task::ActiveModel = audio_task.clone().into();
                    model.next_retry_at = Set(Some(now + Duration::seconds(30)));
                    if let Err(error) = model.update(&self.db).await {
                        warn!("[音频自动重试] 延后 URL 重试失败: {error}");
                    }
                    continue;
                }
            };

            if audio_url.is_empty() {
                continue;
            }

            // 重新入队；aria2 不可用时由引擎选择器降级。
            let dir = self.task_download_dir(&audio_task).await;
            let filename = audio_task
                .filename
                .clone()
                .unwrap_or_else(|| format!("{bvid}.m4a"));
            let task_id = audio_task.id;
            let engine = self.select_engine().await;
            match self
                .dispatch_transfer(
                    engine, task_id, &audio_url, &bvid, &cookies, &dir, &filename,
                )
                .await
            {
                Ok((gid, permit)) => {
                    let mut model: download_task::ActiveModel = audio_task.into();
                    model.status = Set("downloading".to_string());
                    model.error = Set(None);
                    model.url = Set(Some(audio_url.clone()));
                    model.gid = Set(gid);
                    model.progress_percent = Set(0);
                    model.downloaded_size = Set(0);
                    model.speed = Set(0);
                    model.attempts = Set((attempts + 1) as i32);
                    let delay = Duration::seconds(10 * 2_i64.pow(attempts + 1));
                    model.next_retry_at = Set(Some(now + delay));
                    if let Err(e) = model.update(&self.db).await {
                        error!("[音频自动重试] 更新任务失败 {bvid}: {e}");
                    }
                    if engine == TransferEngine::Native {
                        // permit 转移至 spawned task；spawn 失败时 permit 自动 Drop 释放
                        if let Err(e) = self
                            .spawn_native_transfer(task_id, &audio_url, &cookies, None, permit)
                            .await
                        {
                            warn!("[音频自动重试] {bvid} 启动原生兜底传输失败: {e}");
                        }
                    }
                    info!(
                        "[音频自动重试] {bvid} 重试添加成功，下次重试需等待 {} 秒",
                        delay.num_seconds()
                    );
                }
                Err(e) => {
                    warn!("[音频自动重试] {bvid} 投递下载失败: {e}，将在下次循环重试");
                    let mut model: download_task::ActiveModel = audio_task.into();
                    model.next_retry_at = Set(Some(now + Duration::seconds(30)));
                    if let Err(error) = model.update(&self.db).await {
                        warn!("[音频自动重试] 延后下载重试失败: {error}");
                    }
                }
            }
        }
    }
}
