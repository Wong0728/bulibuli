//! 任务入队：add_task 链路（URL 校验、去重、目录派生、落库、投递与音频伴生任务），
//! 以及"近期完成"幂等缓存（吸收前端超时重试的重复入队请求）。

use crate::models::{blogger, download_task};
use crate::services::file_safety::sanitize_filename;
use anyhow::{anyhow, Result};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use super::engine::TransferEngine;
use super::history_placeholder::HistoryPlaceholder;
use super::{backoff_key, file_stem_for, is_valid_bvid, DownloadManager, PageInfo, TaskOutcome};

/// 任务完成后多久以内的重复入队请求视为重复（前端超时重试、连点），
/// 直接幂等返回"产物已存在"，而不是整段重下再 SHA-256 比对。
const RECENT_COMPLETION_WINDOW: Duration = Duration::from_secs(60);
/// recent_completions 缓存最长保留时间（顺带清理过期项，防止无限增长）。
const RECENT_COMPLETION_RETENTION: Duration = Duration::from_secs(10 * 60);

/// durl 直链（音视频已封装的 flv/mp4）识别：URL 路径带对应扩展名。
/// 命中时视频任务无需音频伴生任务即可成片（DASH m4s 分离流返回 None）。
fn muxed_direct_link_ext(url: &str) -> Option<&'static str> {
    let path = url.split('?').next().unwrap_or_default();
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".flv") {
        Some("flv")
    } else if lower.ends_with(".mp4") {
        Some("mp4")
    } else {
        None
    }
}

/// 从任务行还原分P信息：单P（cid/page 为 NULL）返回 None，保持存量语义；
/// 多P时重建 PageInfo，供重试等入口沿用原分P的 cid/文件名。
pub(super) fn page_info_from_task(task: &download_task::Model) -> Option<PageInfo> {
    match (task.cid, task.page) {
        (Some(cid), Some(page)) => Some(PageInfo {
            cid,
            page,
            part_title: task.part_title.clone().unwrap_or_default(),
        }),
        _ => None,
    }
}

impl DownloadManager {
    #[allow(clippy::too_many_arguments)]
    pub async fn add_task(
        &self,
        bvid: &str,
        title: &str,
        url: &str,
        cookies: &str,
        quality: i32,
        task_type: &str,
        uid: Option<&str>,
        source: &str,
        page: Option<&PageInfo>,
        audio_ext: Option<&str>,
    ) -> Result<TaskOutcome> {
        self.add_task_inner(
            bvid, title, url, cookies, quality, task_type, uid, source, page, audio_ext,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn add_task_inner(
        &self,
        bvid: &str,
        title: &str,
        url: &str,
        cookies: &str,
        quality: i32,
        task_type: &str,
        uid: Option<&str>,
        source: &str,
        page: Option<&PageInfo>,
        audio_ext: Option<&str>,
    ) -> Result<TaskOutcome> {
        // 同键请求串行化：并发的重复 add（如前端超时重试连发三次）在锁上排队，
        // 后到者重新查库即可看到先行者写入的 downloading/completed 状态，
        // 避免同时穿过"无存量任务"检查创建出两行任务或对同一产物双重派发。
        let key = backoff_key(bvid, page.map(|p| p.cid), task_type);
        let entry = {
            let mut guards = self.add_task_locks.lock().await;
            guards
                .entry(key.clone())
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _serial = entry.lock().await;
        let outcome = self
            .add_task_serialized(
                bvid, title, url, cookies, quality, task_type, uid, source, page, audio_ext,
            )
            .await;
        {
            let mut guards = self.add_task_locks.lock().await;
            // 仅当没有其它等待者复用这把锁时才移除，防止映射随 bvid 数量无限增长
            if std::sync::Arc::strong_count(&entry) == 2 {
                guards.remove(&key);
            }
        }
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    async fn add_task_serialized(
        &self,
        bvid: &str,
        title: &str,
        url: &str,
        cookies: &str,
        quality: i32,
        task_type: &str,
        uid: Option<&str>,
        source: &str,
        page: Option<&PageInfo>,
        audio_ext: Option<&str>,
    ) -> Result<TaskOutcome> {
        crate::services::bili_url_policy::validate_download_url(url)
            .await
            .map_err(anyhow::Error::from)?;
        // 入口校验 bvid 格式：防止恶意 bvid 触发路径穿越或文件名注入
        if !is_valid_bvid(bvid) {
            return Ok(TaskOutcome::rejected(format!(
                "非法 bvid 格式: {bvid}（期望 BV + 10 位字符）"
            )));
        }

        // 手动产物固定进入 manual 区域，不能因 history 保存了 UP 主 UID 而漂移到自动目录。
        // 自动/恢复任务仍可从 history 回退 UID。
        let uid_resolved: Option<String> = match (source, uid) {
            ("manual", _) => None,
            (_, Some(u)) if !u.is_empty() => Some(u.to_string()),
            _ => self.get_blogger_uid_from_history(bvid).await,
        };
        let uid = uid_resolved.as_deref();
        // history 元数据用的 UID 与下载目录解耦：手动目录仍走 manual 区，
        // 但看板分组/封面定位依赖 UID，否则下载期间分组显示 "unknown"、
        // 封面被 /api/cover 落到日期兜底目录。
        let metadata_uid: Option<String> = uid_resolved
            .clone()
            .or_else(|| uid.filter(|u| !u.is_empty()).map(str::to_string));

        // 快照博主头像：创建任务时从 bloggers 表复制，供前端展示。
        let face_url: Option<String> = if let Some(uid_val) = metadata_uid.as_deref() {
            blogger::Entity::find()
                .filter(blogger::Column::Uid.eq(uid_val))
                .one(&self.db)
                .await
                .ok()
                .flatten()
                .and_then(|b| b.face)
        } else {
            None
        };

        // 分P信息：单P（page=None）保持存量语义——cid/page/part_title 全为 NULL，
        // 文件名与去重键均不含分P后缀，与现状完全一致。
        let cid = page.map(|p| p.cid);
        let page_num = page.map(|p| p.page);
        let part_title = page.map(|p| p.part_title.clone());
        let stem = file_stem_for(bvid, page_num);

        // aria2 优先；只有重试和实例重建均失败时才降级。
        let engine = self.select_engine().await;

        // 视频流用 .m4s，音频流默认 .m4a；杜比/Hi-Res 命中时由调用方传入 ec3/flac 覆盖。
        // durl 直链（.flv/.mp4）音视频已封装，直接用容器扩展名。
        let muxed_ext = (task_type == "video")
            .then(|| muxed_direct_link_ext(url))
            .flatten();
        let default_ext = match task_type {
            "audio" => audio_ext.unwrap_or("m4a"),
            _ => muxed_ext.unwrap_or("m4s"),
        };
        let desired_dir = self
            .templated_download_dir(uid, title, bvid, quality, task_type, page)
            .await;

        // 去重键为 (bvid, cid, type)：单P按 cid IS NULL 匹配存量数据，多P按具体 cid 隔离各分P。
        let existing = {
            let mut find = download_task::Entity::find()
                .filter(download_task::Column::Bvid.eq(bvid))
                .filter(download_task::Column::TaskType.eq(task_type));
            find = match cid {
                Some(c) => find.filter(download_task::Column::Cid.eq(c)),
                None => find.filter(download_task::Column::Cid.is_null()),
            };
            find.one(&self.db).await?
        };

        if let Some(existing) = existing {
            if existing.status == "downloading" {
                return Ok(TaskOutcome::rejected(format!("该{task_type}正在下载中")));
            }
            if existing.status == "completed" {
                let same_source = existing.source.as_deref().unwrap_or("auto") == source;
                let dir = if same_source {
                    existing
                        .download_dir
                        .as_deref()
                        .map(PathBuf::from)
                        .filter(|path| path.starts_with(&self.paths.download_dir) && path.exists())
                        .unwrap_or_else(|| desired_dir.clone())
                } else {
                    desired_dir.clone()
                };
                if same_source && Self::completed_product_exists(&dir, &stem, task_type).await {
                    return Ok(TaskOutcome::done(format!(
                        "该{task_type}产物已存在，已保留现有文件"
                    )));
                }
                let default_filename = format!("{stem}.{default_ext}");
                let filename =
                    sanitize_filename(existing.filename.as_deref().unwrap_or(&default_filename));
                if dir.join(&filename).exists() {
                    // 完成窗口内的重复请求（前端超时重试、连点）直接幂等返回，
                    // 不再整段重下只为 SHA-256 比对；窗口外的显式重下仍走比对路径。
                    if same_source
                        && self
                            .recently_completed(&existing.bvid, existing.cid, task_type)
                            .await
                    {
                        return Ok(TaskOutcome::done(format!(
                            "该{task_type}产物刚下载完成，已保留现有文件"
                        )));
                    }
                    // 下载到 .downloading 临时文件，完成后由 monitor_loop
                    // 调用 dedupe_and_finalize_file 进行 SHA-256 比对，避免覆盖原文件。
                    let temp_filename = format!("{stem}.{default_ext}.downloading");
                    return self
                        .reset_and_dispatch_existing_task(
                            existing,
                            engine,
                            url,
                            cookies,
                            uid,
                            source,
                            &dir,
                            temp_filename,
                            true,
                            "已重新添加到下载队列（将进行 SHA-256 去重比对）",
                        )
                        .await;
                }
                return self
                    .reset_and_dispatch_existing_task(
                        existing,
                        engine,
                        url,
                        cookies,
                        uid,
                        source,
                        &dir,
                        default_filename,
                        true,
                        if same_source {
                            "原产物缺失，已重新添加到下载队列"
                        } else {
                            "检测到手动/自动来源不同，已在对应区域创建独立产物"
                        },
                    )
                    .await;
            } else {
                let default_filename = format!("{stem}.{default_ext}");
                let filename =
                    sanitize_filename(existing.filename.as_deref().unwrap_or(&default_filename));
                return self
                    .reset_and_dispatch_existing_task(
                        existing,
                        engine,
                        url,
                        cookies,
                        uid,
                        source,
                        &desired_dir,
                        filename,
                        true,
                        "已重新添加到下载队列",
                    )
                    .await;
            }
        }

        // 自动添加音频任务。音频 URL 已在视频落库前解析；失败时撤销视频任务。
        // durl 直链（flv/mp4 封装流）自带音轨，跳过音频解析与伴生任务，
        // 否则 get_audio_url 因无 DASH 返回 None 会让整个视频任务误报失败。
        let prepared_audio = if task_type == "video" && muxed_ext.is_none() {
            let preference = self
                .settings_service
                .current()
                .query
                .audio_quality_preference
                .clone();
            match self
                .bili_api
                .get_audio_url(bvid, cid, cookies, &preference)
                .await
            {
                Ok(Some(audio)) if !audio.audio_url.is_empty() => {
                    Some((audio.audio_url, audio.ext))
                }
                Ok(_) => return Err(anyhow!("获取音频 URL 失败: 未找到音频流")),
                Err(error) => return Err(error),
            }
        } else {
            None
        };

        let filename = format!("{stem}.{default_ext}");
        let dir = desired_dir;
        let new_task = download_task::ActiveModel {
            bvid: Set(bvid.to_string()),
            title: Set(Some(title.to_string())),
            url: Set(Some(url.to_string())),
            quality: Set(quality),
            task_type: Set(task_type.to_string()),
            cid: Set(cid),
            page: Set(page_num),
            part_title: Set(part_title.clone()),
            status: Set("pending".to_string()),
            filename: Set(Some(filename.clone())),
            original_url: Set(Some(url.to_string())),
            download_dir: Set(Some(dir.to_string_lossy().to_string())),
            source: Set(Some(source.to_string())),
            // 以下字段在模型中为 NOT NULL（非 Option），缺少默认值，
            // 必须显式设置，否则 SQLite 会报 "NOT NULL constraint failed" 导致 500
            progress_percent: Set(0),
            downloaded_size: Set(0),
            total_size: Set(0),
            speed: Set(0),
            generation: Set(0),
            completion_triggered: Set(false),
            stage: Set("queued".to_string()),
            priority: Set(if source == "manual" { 300 } else { 100 }),
            attempts: Set(0),
            next_retry_at: Set(None),
            error_kind: Set(None),
            selected_quality: Set(Some(quality)),
            selected_codec: Set(None),
            fallback_reason: Set(None),
            face_url: Set(face_url),
            ..Default::default()
        };
        let task = new_task.insert(&self.db).await?;
        // 手动下载入队即写 history 占位记录：看板「下载中」由 history 表驱动，
        // 无记录则下载期间看板无卡片，直到首个子任务完成才出现。
        // 仅 video/audio：danmaku/comments 单独下载本就不建独立记录（与完成路径一致）。
        // 占位记录携带 UID/封面目录：下载期间分组即归属博主，封面直接落到任务目录。
        if source == "manual" && matches!(task_type, "video" | "audio") {
            self.ensure_history_placeholder(HistoryPlaceholder {
                bvid,
                title,
                uid: metadata_uid.as_deref(),
                cid,
                page: page_num,
                part_title: part_title.as_deref(),
                cover_dir: Some(dir.as_path()),
            })
            .await;
        }
        self.queue_notify.notify_one(); // 唤醒 monitor_loop 空闲退避，新任务立即被监控
        let (gid, permit) = self
            .dispatch_transfer(engine, task.id, url, bvid, cookies, &dir, &filename)
            .await?;
        let runtime_gid = gid.clone();
        let mut model: download_task::ActiveModel = task.clone().into();
        model.gid = Set(gid.clone());
        model.status = Set("downloading".to_string());
        if let Err(e) = model.update(&self.db).await {
            if let Some(gid) = runtime_gid.as_deref() {
                if let Err(remove_error) = self.aria2.remove(gid).await {
                    error!("回滚 Aria2 任务失败 gid={gid}: {remove_error}");
                }
            }
            if let Err(e) = download_task::Entity::delete_by_id(task.id)
                .exec(&self.db)
                .await
            {
                warn!("补偿删除任务行失败 task_id={}: {e}", task.id);
            }
            // 手动任务入队时可能已写 history 占位记录，回滚时一并清理
            self.cleanup_history_placeholder(bvid, cid).await;
            return Err(anyhow!(
                "下载任务状态持久化失败，已取消外部传输: task_id={}, error={}",
                task.id,
                e
            ));
        }
        if engine == TransferEngine::Native {
            // permit 转移至 spawned task，持有跨越整个下载生命周期；
            // spawn 失败时 permit 随函数退出自动 Drop 释放并发槽位
            self.spawn_native_transfer(task.id, url, cookies, uid, permit)
                .await?;
        }
        // aria2 路径：permit 在此处 Drop 释放（aria2 自行管理并发上限）

        // 自动添加音频任务。音频 URL 已在视频落库前解析；失败时撤销视频任务。
        if let Some((audio_url, audio_ext)) = prepared_audio {
            let audio_result = Box::pin(self.add_task_inner(
                bvid,
                title,
                &audio_url,
                cookies,
                quality,
                "audio",
                uid,
                source,
                page,
                Some(audio_ext.as_str()),
            ))
            .await;
            if !matches!(audio_result, Ok(ref result) if result.ok) {
                if let Some(token) = self.native_tasks.lock().await.remove(&task.id) {
                    token.cancel();
                }
                if let Some(gid) = runtime_gid.as_deref() {
                    if let Err(remove_error) = self.aria2.remove(gid).await {
                        warn!("回滚 Aria2 任务失败 gid={gid}: {remove_error}");
                    }
                }
                if let Err(e) = download_task::Entity::delete_by_id(task.id)
                    .exec(&self.db)
                    .await
                {
                    warn!("补偿删除任务行失败 task_id={}: {e}", task.id);
                }
                // 音频任务添加失败回滚：清理视频入队时写的 history 占位记录，
                // 避免看板留下永远 pending 的卡片
                self.cleanup_history_placeholder(bvid, cid).await;
                return match audio_result {
                    Ok(result) => Err(anyhow!("自动添加音频任务失败: {}", result.message)),
                    Err(error) => Err(error),
                };
            }
            info!("已自动添加音频任务 {bvid}");
        }

        Ok(TaskOutcome::accepted(
            match engine {
                TransferEngine::Aria2 => "已添加到 Aria2 下载队列",
                TransferEngine::Native => "已添加到下载队列（aria2 不可用，原生下载兜底）",
            },
            task.id,
        ))
    }

    /// 记录任务完成时刻，供 `add_task` 在短窗口内幂等吸收重复入队请求。
    /// 同时顺带清理过期项，缓存规模以最近 10 分钟内的完成为上限。
    pub(super) async fn record_recent_completion(
        &self,
        bvid: &str,
        cid: Option<i64>,
        task_type: &str,
    ) {
        let mut recent = self.recent_completions.lock().await;
        recent.retain(|_, at| at.elapsed() < RECENT_COMPLETION_RETENTION);
        recent.insert(backoff_key(bvid, cid, task_type), Instant::now());
    }

    /// 任务是否在完成窗口内刚完成（用于吸收前端超时重试造成的重复请求）。
    async fn recently_completed(&self, bvid: &str, cid: Option<i64>, task_type: &str) -> bool {
        let recent = self.recent_completions.lock().await;
        recent
            .get(&backoff_key(bvid, cid, task_type))
            .is_some_and(|at| at.elapsed() < RECENT_COMPLETION_WINDOW)
    }
}
