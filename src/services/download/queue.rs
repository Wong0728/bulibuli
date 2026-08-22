use crate::domain::{DownloadStage, DownloadStatus};
use crate::models::{blogger, download_task, history};
use crate::services::file_safety::sanitize_filename;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

use super::engine::TransferEngine;
use super::history_sync::HistoryPlaceholder;
use super::{
    backoff_key, file_stem_for, is_valid_bvid, task_cache_key, DownloadManager, PageInfo,
    TaskOutcome,
};

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

/// aria2 幂等性错误判定：`pause` 对"已是 paused"报 cannot be paused now，
/// 属于重复暂停而非真故障，应视为成功照常落库。
fn is_idempotent_pause_err(e: &anyhow::Error) -> bool {
    e.to_string().contains("cannot be paused now")
}

/// 产物文件名严格匹配：仅接受 `{stem}.{ext}` 精确命中或 `{title}_{stem}.{ext}`
/// 标题前缀命中（大小写归一）。要求 stem 与扩展名之间是硬边界，排除前缀/包含误匹配：
/// 裸 `starts_with(stem)` 曾让单P stem 命中多P产物 `{bvid}_p2.mp4`、
/// `{bvid}_p2` 命中 `_p20`，导致缺文件的分P被判已存在而不重下。
fn product_file_matches(name: &str, stem: &str, extensions: &[&str]) -> bool {
    let lower = name.to_ascii_lowercase();
    let lower_stem = stem.to_ascii_lowercase();
    extensions.iter().any(|ext| {
        let suffix = format!(".{ext}");
        lower.ends_with(&suffix)
            && (lower.starts_with(&format!("{lower_stem}."))
                || lower.ends_with(&format!("_{lower_stem}{suffix}")))
    })
}

/// 同上：`unpause` 对"已非 paused"（active/waiting/complete）报
/// cannot be unpaused now，属于重复恢复而非真故障。
fn is_idempotent_unpause_err(e: &anyhow::Error) -> bool {
    e.to_string().contains("cannot be unpaused now")
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
        crate::services::bili_url_policy::validate(url)
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
                    let _ = self.aria2.remove(gid).await;
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

    #[allow(clippy::too_many_arguments)]
    async fn reset_and_dispatch_existing_task(
        &self,
        existing: download_task::Model,
        engine: TransferEngine,
        url: &str,
        cookies: &str,
        uid: Option<&str>,
        source: &str,
        dir: &Path,
        filename: String,
        bump_generation: bool,
        message: &str,
    ) -> Result<TaskOutcome> {
        let filename = sanitize_filename(&filename);
        self.cleanup_task_caches(&existing.bvid, existing.cid).await;
        let download_id = existing.id;
        let (gid, permit) = self
            .dispatch_transfer(
                engine,
                download_id,
                url,
                &existing.bvid,
                cookies,
                dir,
                &filename,
            )
            .await?;
        let next_generation = if bump_generation {
            existing.generation.saturating_add(1)
        } else {
            existing.generation
        };
        let priority = if source == "manual" {
            300
        } else if matches!(existing.status.as_str(), "failed" | "retrying") {
            200
        } else {
            100
        };
        let mut model: download_task::ActiveModel = existing.into();
        model.status = Set("downloading".to_string());
        model.error = Set(None);
        model.url = Set(Some(url.to_string()));
        model.progress_percent = Set(0);
        model.downloaded_size = Set(0);
        model.total_size = Set(0);
        model.speed = Set(0);
        model.gid = Set(gid.clone());
        model.filename = Set(Some(filename));
        model.download_dir = Set(Some(dir.to_string_lossy().to_string()));
        model.source = Set(Some(source.to_string()));
        model.priority = Set(priority);
        model.generation = Set(next_generation);
        model.completion_triggered = Set(false);
        model.stage = Set("transferring".to_string());
        // 重试额度按"每次运行"计算：重新派发时清零累计 attempts，
        // 否则 video_retry/audio_retry 读到历史累计值会提前耗尽自动重试
        model.attempts = Set(0);
        model.next_retry_at = Set(None);
        model.error_kind = Set(None);
        if let Err(e) = model.update(&self.db).await {
            if let Some(gid) = gid.as_deref() {
                if let Err(remove_error) = self.aria2.remove(gid).await {
                    error!("回滚 Aria2 任务失败 gid={gid}: {remove_error}");
                }
            }
            return Err(anyhow!(
                "下载任务状态持久化失败，已取消外部传输: task_id={}, error={}",
                download_id,
                e
            ));
        }
        if engine == TransferEngine::Native {
            // permit 转移至 spawned task；spawn 失败时 permit 自动 Drop 释放
            self.spawn_native_transfer(download_id, url, cookies, uid, permit)
                .await?;
        }
        Ok(TaskOutcome::accepted(message, download_id))
    }

    /// 判断指定词根的最终产物是否已存在。`stem` 单P为 bvid，多P为 `{bvid}_p{page}`。
    /// 合并产物命名为 `{title}_{stem}.mp4`，匹配规则见 [`product_file_matches`]：
    /// 仅接受精确命中与标题前缀命中，边界判定与 storage.rs 的去重扫描对齐。
    pub(super) async fn completed_product_exists(dir: &Path, stem: &str, task_type: &str) -> bool {
        let extensions: &[&str] = if task_type == "video" {
            &["mp4", "mkv", "flv", "mov"]
        } else if task_type == "audio" {
            &["m4a", "mp3", "aac", "wav", "flac"]
        } else {
            return false;
        };
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return false;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Some(name) = entry.file_name().into_string().ok() else {
                continue;
            };
            if product_file_matches(&name, stem, extensions) {
                return true;
            }
        }
        false
    }

    /// 回滚手动任务时同步清理入队时写入的 history 占位记录：
    /// 占位行 state=pending 且无完成时间，残留会让看板「下载中」
    /// 永远挂着一张不会推进的卡片。
    async fn cleanup_history_placeholder(&self, bvid: &str, cid: Option<i64>) {
        let mut query = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .filter(history::Column::State.eq("pending"))
            .filter(history::Column::DownloadTime.is_null());
        query = match cid {
            Some(cid) => query.filter(history::Column::Cid.eq(cid)),
            None => query.filter(history::Column::Cid.is_null()),
        };
        let rows = match query.all(&self.db).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("查询 history 占位记录失败 {bvid}: {e}");
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        if let Err(e) = history::Entity::delete_many()
            .filter(history::Column::Id.is_in(rows.iter().map(|h| h.id)))
            .exec(&self.db)
            .await
        {
            warn!("清理 history 占位记录失败 {bvid}: {e}");
        }
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

    pub async fn retry_task(&self, bvid: &str, task_type: &str) -> Result<TaskOutcome> {
        // 多P：同一 (bvid, task_type) 可能对应多个分P任务，逐个重试；单P时恰为一行，行为不变。
        let tasks = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(download_task::Column::TaskType.eq(task_type))
            .all(&self.db)
            .await?;
        if tasks.is_empty() {
            return Ok(TaskOutcome::rejected("未找到任务"));
        }
        // 重试使用当前登录态。
        let cookies = self
            .settings_service
            .cookie_header()
            .await
            .unwrap_or_default();
        let uid = self.get_blogger_uid_from_history(bvid).await;
        let mut last: Option<TaskOutcome> = None;
        for task in tasks {
            // 退避键按分P隔离：单P为 `{bvid}_{task_type}`（不变），多P为 `{bvid}#{cid}_{task_type}`。
            let key = backoff_key(bvid, task.cid, task_type);
            if let Some(wait_secs) = self.check_backoff(&key).await {
                last = Some(TaskOutcome::rejected(format!(
                    "重试过于频繁，请 {wait_secs} 秒后再试"
                )));
                continue;
            }
            if task.status == "failed" {
                self.state_service
                    .transition(
                        task.id,
                        task.generation,
                        DownloadStatus::Retrying,
                        DownloadStage::Resolving,
                    )
                    .await?;
                let mut retrying: download_task::ActiveModel = task.clone().into();
                retrying.priority = Set(200);
                retrying.attempts = Set(task.attempts.saturating_add(1));
                retrying.next_retry_at = Set(None);
                retrying.update(&self.db).await?;
            }
            let url = self
                .resolve_resume_url(&task)
                .await
                .filter(|value| !value.is_empty())
                .or_else(|| task.url.clone())
                .unwrap_or_default();
            if url.is_empty() {
                last = Some(TaskOutcome::rejected("无法解析下载链接"));
                continue;
            }
            // 重试保留任务原有来源，避免手动任务重试后日志混入博主日志
            let task_source = task.source.clone().unwrap_or_else(|| "auto".to_string());
            let page_info = page_info_from_task(&task);
            let result = self
                .add_task(
                    bvid,
                    task.title.as_deref().unwrap_or(bvid),
                    &url,
                    &cookies,
                    task.quality,
                    task_type,
                    uid.as_deref(),
                    &task_source,
                    page_info.as_ref(),
                    None,
                )
                .await?;
            let success = result.ok;
            self.update_backoff(&key, success).await;
            if !success {
                self.persist_retry_schedule(bvid, task.cid, task_type, "transient")
                    .await;
            }
            last = Some(result);
        }
        Ok(last.unwrap_or_else(|| TaskOutcome::rejected("未找到任务")))
    }

    pub async fn retry_all_failed(&self, since: Option<i64>) -> Result<TaskOutcome> {
        let mut query =
            download_task::Entity::find().filter(download_task::Column::Status.eq("failed"));
        if let Some(ts) = since {
            let since_dt: DateTime<Local> = DateTime::from_timestamp(ts, 0)
                .map(|dt| dt.into())
                .unwrap_or_else(|| Local::now() - chrono::Duration::days(1));
            query = query.filter(download_task::Column::UpdatedAt.gte(since_dt));
        }
        let tasks = query.all(&self.db).await?;
        // 批量重试统一使用当前登录态。
        let cookies = self
            .settings_service
            .cookie_header()
            .await
            .unwrap_or_default();
        let mut count = 0;
        let mut skipped_count = 0;
        let mut failed_count = 0;
        for task in tasks {
            if task.url.is_some() {
                let key = backoff_key(&task.bvid, task.cid, &task.task_type);
                if self.check_backoff(&key).await.is_some() {
                    skipped_count += 1;
                    continue;
                }
                let uid = self.get_blogger_uid_from_history(&task.bvid).await;
                let task_source = task.source.clone().unwrap_or_else(|| "auto".to_string());
                let page_info = page_info_from_task(&task);
                let url = self
                    .resolve_resume_url(&task)
                    .await
                    .filter(|value| !value.is_empty())
                    .or_else(|| task.url.clone());
                let Some(url) = url else {
                    failed_count += 1;
                    continue;
                };
                let result = self
                    .add_task(
                        &task.bvid,
                        task.title.as_deref().unwrap_or(&task.bvid),
                        &url,
                        &cookies,
                        task.quality,
                        &task.task_type,
                        uid.as_deref(),
                        &task_source,
                        page_info.as_ref(),
                        None,
                    )
                    .await?;
                let success = result.ok;
                self.update_backoff(&key, success).await;
                if !success {
                    self.persist_retry_schedule(&task.bvid, task.cid, &task.task_type, "transient")
                        .await;
                }
                if success {
                    count += 1;
                } else {
                    failed_count += 1;
                }
            }
        }
        Ok(TaskOutcome::done(format!(
            "已重试 {count} 个失败的下载任务，{skipped_count} 个因退避跳过，{failed_count} 个重试失败"
        )))
    }

    pub async fn remove_task(&self, bvid: &str, task_type: &str) -> Result<TaskOutcome> {
        // 多P：同一 (bvid, task_type) 可能对应多个分P任务，需逐行移除；单P时恰为一行，行为不变。
        let tasks = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(download_task::Column::TaskType.eq(task_type))
            .all(&self.db)
            .await?;
        for task in tasks {
            // 原生兜底任务：取消传输（后台任务感知取消后自行退出，不再写终态）
            if let Some(token) = self.native_tasks.lock().await.remove(&task.id) {
                token.cancel();
            }
            if let Some(gid) = &task.gid {
                if let Err(error) = self.aria2.remove(gid).await {
                    warn!("移除 aria2 任务失败 gid={gid}: {error}");
                }
            }
            download_task::Entity::delete_by_id(task.id)
                .exec(&self.db)
                .await?;
            // 清理关联的内存缓存：避免移除后残留的进度/退避/合并标记影响后续操作
            // 注：retry_backoff 按 `{cache_key}_{task_type}` 索引（单P为 `{bvid}_{task_type}`），需精确移除
            {
                let mut backoff = self.retry_backoff.lock().await;
                backoff.remove(&format!("{}_{}", task_cache_key(bvid, task.cid), task_type));
            }
        }
        // 若该 bvid 下已无任何任务，则一并清理 bvid 级缓存
        let remaining = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .count(&self.db)
            .await?;
        if remaining == 0 {
            // 进度缓存按 task_cache_key 索引：单P键为 bvid，多P键为 `{bvid}#{cid}`，统一按前缀清理。
            {
                let mut cache = self.progress_cache.lock().await;
                let prefix = format!("{bvid}#");
                let keys: Vec<String> = cache
                    .keys()
                    .filter(|k| k.as_str() == bvid || k.starts_with(&prefix))
                    .cloned()
                    .collect();
                for k in keys {
                    cache.remove(&k);
                }
            }
            {
                let mut guard = self.lock_merge_set();
                let prefix = format!("{bvid}#");
                let keys: Vec<String> = guard
                    .iter()
                    .filter(|k| k.as_str() == bvid || k.starts_with(&prefix))
                    .cloned()
                    .collect();
                for k in keys {
                    guard.remove(&k);
                }
            }
        }
        Ok(TaskOutcome::done("已移除下载记录"))
    }

    /// 暂停下载任务。
    /// - `task_id = None`：全局暂停（逐任务暂停并持久化所有 downloading/pending 为 paused）。
    /// - `task_id = Some(id)`：仅暂停指定任务。仅 downloading/pending 可暂停，其他状态返回可读拒绝信息。
    ///
    /// 暂停后：速度归零、generation+=1（防陈旧回调覆盖）；释放并发额度
    /// （native 路径 token 取消后 spawn 退出自动 Drop permit；aria2 路径不持有 permit）。
    /// 程序重启后 paused 任务不在 `resume_pending_tasks` 查询范围内，保持暂停状态。
    pub async fn pause_task(&self, task_id: Option<i32>) -> Result<TaskOutcome> {
        match task_id {
            Some(id) => self.pause_single_task(id).await,
            None => self.pause_all_tasks().await,
        }
    }

    /// 恢复下载任务。
    /// - `task_id = None`：全局恢复（逐任务重新调度所有 paused 任务）。
    /// - `task_id = Some(id)`：仅恢复指定任务。仅 paused 可恢复。
    ///
    /// 恢复时重新解析 URL（B 站 CDN URL 带 deadline 签名，过期会 403），
    /// 然后重新 dispatch_transfer 进入引擎调度。aria2 路径天然支持断点续传；
    /// 原生路径当前为整段重下（不依赖 .part 续传）。
    pub async fn resume_task(&self, task_id: Option<i32>) -> Result<TaskOutcome> {
        match task_id {
            Some(id) => self.resume_single_task(id).await,
            None => self.resume_all_tasks().await,
        }
    }

    /// aria2 任务暂停：先优雅 `pause`，失败时降级 `forcePause`。
    /// 两者都失败说明 gid 已失效或 aria2 异常，调用方不得将任务落库为 paused
    ///（否则 monitor_loop 不再轮询该 gid，形成"实际仍在下载但状态为暂停"的孤儿任务）。
    pub(super) async fn pause_gid_with_fallback(&self, gid: &str) -> Result<()> {
        if let Err(e) = self.aria2.pause(gid).await {
            warn!("暂停 aria2 任务 gid={gid} 失败，尝试 forcePause: {e}");
            self.aria2.force_pause(gid).await?;
        }
        Ok(())
    }

    async fn pause_single_task(&self, task_id: i32) -> Result<TaskOutcome> {
        let task = download_task::Entity::find_by_id(task_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| anyhow!("任务 {task_id} 不存在"))?;

        // 仅 downloading/pending 可暂停；paused 重复暂停视为成功（幂等）
        let status = task.status.as_str();
        if status == "paused" {
            return Ok(TaskOutcome::done("任务已处于暂停状态"));
        }
        if !matches!(status, "downloading" | "pending") {
            return Ok(TaskOutcome::rejected(format!(
                "当前状态为 {status}，仅下载中/等待中可暂停"
            )));
        }

        // Native：取消传输（spawn 感知取消后自行退出并删除 .downloading 临时文件，
        // 不再写终态）
        if let Some(token) = self.native_tasks.lock().await.remove(&task.id) {
            token.cancel();
        } else if let Some(gid) = &task.gid {
            // aria2 任务：pause 失败降级 forcePause；"cannot be paused now"
            // 表示任务已被暂停（幂等场景），视为成功照常落库 paused；
            // 其余失败则不落库，避免 monitor_loop 停止轮询后形成孤儿任务
            if let Err(e) = self.pause_gid_with_fallback(gid).await {
                if is_idempotent_pause_err(&e) {
                    debug!("aria2 任务 gid={gid} 已处于暂停状态");
                } else {
                    warn!("暂停 aria2 任务 gid={gid} 失败: {e}");
                    return Ok(TaskOutcome::rejected(format!(
                        "暂停任务 {} 失败: {e}",
                        task.bvid
                    )));
                }
            }
        }

        // 落库：status=paused，generation+=1（防 stale 回调覆盖）
        let updated = self
            .state_service
            .transition(
                task.id,
                task.generation,
                DownloadStatus::Paused,
                DownloadStage::Transferring,
            )
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        // 速度归零（保留 downloaded_size 供前端展示断点进度）
        let mut model: download_task::ActiveModel = updated.into();
        model.speed = Set(0);
        if let Err(e) = model.update(&self.db).await {
            warn!("持久化暂停速度归零失败 task_id={task_id}: {e}");
        }

        // 广播 paused 状态，前端立即把进度条置为已暂停
        self.broadcast_progress(
            &task,
            "paused",
            task.progress_percent,
            task.downloaded_size,
            task.total_size,
            0,
            None,
        )
        .await;

        info!(
            "[DownloadManager] 已暂停任务: {} ({})",
            task.bvid, task.task_type
        );
        Ok(TaskOutcome::done(format!("已暂停任务 {}", task.bvid)))
    }

    async fn pause_all_tasks(&self) -> Result<TaskOutcome> {
        let tasks = download_task::Entity::find()
            .filter(download_task::Column::Status.is_in(vec!["downloading", "pending"]))
            .all(&self.db)
            .await?;

        let mut count = 0usize;
        let mut failed = 0usize;
        for task in &tasks {
            // Native：取消传输；aria2 单任务 gid 也显式 pause（pauseAll 已涵盖，这里只做兜底）
            if let Some(token) = self.native_tasks.lock().await.remove(&task.id) {
                token.cancel();
            } else if let Some(gid) = &task.gid {
                // pause 失败降级 forcePause；"cannot be paused now" 表示任务已被
                // 暂停（幂等场景），视为成功照常落库；其余失败跳过该任务，不落库
                // paused（否则 monitor_loop 停止轮询后形成"实际仍在下载"的孤儿任务）
                if let Err(e) = self.pause_gid_with_fallback(gid).await {
                    if is_idempotent_pause_err(&e) {
                        debug!("全局暂停时 aria2 任务 gid={gid} 已处于暂停状态");
                    } else {
                        warn!("全局暂停时暂停 aria2 任务 gid={gid} 失败: {e}");
                        failed += 1;
                        continue;
                    }
                }
            }

            // 落库为 paused（失败则跳过该任务，继续处理其他）
            let transition_result = self
                .state_service
                .transition(
                    task.id,
                    task.generation,
                    DownloadStatus::Paused,
                    DownloadStage::Transferring,
                )
                .await;
            let updated = match transition_result {
                Ok(model) => model,
                Err(e) => {
                    warn!("全局暂停时持久化任务 {} 状态失败: {e}", task.bvid);
                    continue;
                }
            };

            let mut model: download_task::ActiveModel = updated.into();
            model.speed = Set(0);
            if let Err(e) = model.update(&self.db).await {
                warn!("全局暂停时持久化任务 {} 速度归零失败: {e}", task.bvid);
            }

            self.broadcast_progress(
                task,
                "paused",
                task.progress_percent,
                task.downloaded_size,
                task.total_size,
                0,
                None,
            )
            .await;
            count += 1;
        }

        info!("[DownloadManager] 全局暂停完成，共暂停 {count} 个任务，{failed} 个暂停失败");
        let msg = if failed > 0 {
            format!("已暂停 {count} 个下载任务，{failed} 个暂停失败")
        } else {
            format!("已暂停 {count} 个下载任务")
        };
        Ok(TaskOutcome::done(msg))
    }

    async fn resume_single_task(&self, task_id: i32) -> Result<TaskOutcome> {
        let task = download_task::Entity::find_by_id(task_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| anyhow!("任务 {task_id} 不存在"))?;

        if task.status != "paused" {
            return Ok(TaskOutcome::rejected(format!(
                "当前状态为 {}，仅已暂停可恢复",
                task.status
            )));
        }

        let download_id = task.id;
        match self.resume_task_inner(&task).await? {
            true => Ok(TaskOutcome::accepted("已恢复下载任务", download_id)),
            false => Ok(TaskOutcome::rejected(
                "恢复失败：无法解析下载链接或投递引擎失败",
            )),
        }
    }

    async fn resume_all_tasks(&self) -> Result<TaskOutcome> {
        // 不调全局 unpauseAll：与逐任务 resume 并发会产生
        // "cannot be unpaused now" 误判，统一走逐任务路径（DB 为事实源）
        let tasks = download_task::Entity::find()
            .filter(download_task::Column::Status.eq("paused"))
            .all(&self.db)
            .await?;

        let mut count = 0usize;
        let mut failed = 0usize;
        for task in &tasks {
            match self.resume_task_inner(task).await {
                Ok(true) => count += 1,
                Ok(false) => failed += 1,
                Err(e) => {
                    warn!("恢复任务 {} 失败: {e}", task.bvid);
                    failed += 1;
                }
            }
        }

        info!("[DownloadManager] 全局恢复完成，恢复 {count} 个，失败 {failed} 个");
        Ok(TaskOutcome::done(format!(
            "已恢复 {count} 个暂停任务，{failed} 个失败"
        )))
    }

    /// 恢复单个 paused 任务的内部实现。
    /// - aria2 路径：先 tellStatus 校验 gid 当前状态，再决定动作：
    ///   paused → unpause（保留断点续传控制文件 .aria2）；active/waiting/complete →
    ///   无需 unpause，直接落库 downloading（complete 由 monitor 轮询补走完成流程）；
    ///   gid 已失效（aria2 重启后 session 丢失）→ 降级为重新解析 URL + 重新 dispatch。
    /// - native 路径：始终重新解析 URL + 重新 dispatch（native 不支持 .part 续传）。
    ///
    /// 返回 `true` 表示已成功投递至引擎；`false` 表示 URL 解析或投递失败。
    async fn resume_task_inner(&self, task: &download_task::Model) -> Result<bool> {
        let cookies = self
            .settings_service
            .cookie_header()
            .await
            .unwrap_or_default();

        // 路径一：原 aria2 任务，gid 仍可能在 aria2 session 中
        if let Some(gid) = &task.gid {
            match self.aria2.get_download_status(gid).await {
                Ok(status) => match status.status.as_str() {
                    // 已在传输或已完成：无需 unpause，直接落库 downloading；
                    // waiting 由 monitor 归一为 pending，complete 由 monitor 走完成流程
                    "active" | "waiting" | "complete" => {
                        let updated = self
                            .state_service
                            .transition(
                                task.id,
                                task.generation,
                                DownloadStatus::Downloading,
                                DownloadStage::Transferring,
                            )
                            .await
                            .map_err(|e| anyhow!(e.to_string()))?;
                        let mut model: download_task::ActiveModel = updated.into();
                        model.speed = Set(0);
                        model.error = Set(None);
                        model.next_retry_at = Set(None);
                        if let Err(e) = model.update(&self.db).await {
                            error!("恢复任务 {} 时持久化状态失败: {e}", task.bvid);
                            return Ok(false);
                        }
                        self.queue_notify.notify_one();
                        info!(
                            "[DownloadManager] aria2 任务 {} ({}) 已处于 {} 状态，直接落库恢复",
                            task.bvid, task.task_type, status.status
                        );
                        return Ok(true);
                    }
                    // 仍是 paused：unpause 恢复传输
                    "paused" => {
                        let updated = self
                            .state_service
                            .transition(
                                task.id,
                                task.generation,
                                DownloadStatus::Downloading,
                                DownloadStage::Transferring,
                            )
                            .await
                            .map_err(|e| anyhow!(e.to_string()))?;
                        if let Err(e) = self.aria2.unpause(gid).await {
                            // "cannot be unpaused now"：状态已变（幂等场景），复核后按实际状态处理
                            if !is_idempotent_unpause_err(&e) {
                                // 复核：若实际已在传输/完成仍算恢复成功，避免 DB 回滚与 aria2 分叉
                                match self.aria2.get_download_status(gid).await {
                                    Ok(recheck)
                                        if matches!(
                                            recheck.status.as_str(),
                                            "active" | "waiting" | "complete"
                                        ) => {}
                                    _ => {
                                        warn!("恢复 aria2 任务 gid={gid} 失败: {e}");
                                        let _ = self
                                            .state_service
                                            .transition(
                                                task.id,
                                                updated.generation,
                                                DownloadStatus::Paused,
                                                DownloadStage::Transferring,
                                            )
                                            .await;
                                        return Ok(false);
                                    }
                                }
                            }
                        }
                        let mut model: download_task::ActiveModel = updated.into();
                        model.speed = Set(0);
                        model.error = Set(None);
                        model.next_retry_at = Set(None);
                        if let Err(e) = model.update(&self.db).await {
                            error!("恢复任务 {} 时持久化状态失败: {e}", task.bvid);
                            let _ = self.aria2.pause(gid).await;
                            return Ok(false);
                        }
                        self.queue_notify.notify_one();
                        info!(
                            "[DownloadManager] 已恢复 aria2 任务: {} ({})",
                            task.bvid, task.task_type
                        );
                        return Ok(true);
                    }
                    // error 等异常态：走下面的重建路径
                    _ => {
                        warn!(
                            "aria2 gid={} 状态异常（{}），将重新解析 URL 并重建任务",
                            gid, status.status
                        );
                    }
                },
                // gid 已失效：走下面的重建路径
                Err(_) => {
                    warn!("aria2 gid={} 已失效，将重新解析 URL 并重建任务", gid);
                }
            }
        }

        // 路径二：重建（native 必走；aria2 gid 失效时降级走）
        // B 站 m4s/m4a 的 CDN URL 带 deadline 签名（约 2h），暂停时间过长会过期，必须重新解析。
        let url = match self.resolve_resume_url(task).await {
            Some(u) if !u.is_empty() => u,
            _ => {
                warn!("恢复任务 {}：无法解析下载链接，保持暂停状态", task.bvid);
                return Ok(false);
            }
        };

        let engine = self.select_engine().await;
        let dir = self.task_download_dir(task).await;
        let filename = task
            .filename
            .clone()
            .unwrap_or_else(|| format!("{}.{}", task.bvid, task.task_type));

        let updated = self
            .state_service
            .transition(
                task.id,
                task.generation,
                DownloadStatus::Downloading,
                DownloadStage::Transferring,
            )
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        // 先持久化状态，再派发外部引擎；派发失败时恢复 paused，避免 DB 与引擎分叉。
        let (gid, permit) = match self
            .dispatch_transfer(engine, task.id, &url, &task.bvid, &cookies, &dir, &filename)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                let _ = self
                    .state_service
                    .transition(
                        task.id,
                        updated.generation,
                        DownloadStatus::Paused,
                        DownloadStage::Transferring,
                    )
                    .await;
                return Err(error);
            }
        };
        let runtime_gid = gid.clone();

        // 更新 gid / 清除 error / next_retry_at
        let updated_generation = updated.generation;
        let mut model: download_task::ActiveModel = updated.into();
        if let Some(g) = gid {
            model.gid = Set(Some(g));
        }
        model.url = Set(Some(url.clone()));
        model.speed = Set(0);
        model.error = Set(None);
        model.next_retry_at = Set(None);
        if let Err(e) = model.update(&self.db).await {
            error!("恢复任务 {} 时持久化状态失败: {e}", task.bvid);
            if let Some(gid) = runtime_gid.as_deref() {
                let _ = self.aria2.remove(gid).await;
            }
            let _ = self
                .state_service
                .transition(
                    task.id,
                    updated_generation,
                    DownloadStatus::Paused,
                    DownloadStage::Transferring,
                )
                .await;
            return Ok(false);
        }

        // Native 路径需重新 spawn 传输（aria2 路径已通过 dispatch 进入调度）
        if engine == TransferEngine::Native {
            let uid = if task.source.as_deref() == Some("manual") {
                None
            } else {
                self.get_blogger_uid_from_history(&task.bvid).await
            };
            if let Err(error) = self
                .spawn_native_transfer(task.id, &url, &cookies, uid.as_deref(), permit)
                .await
            {
                let _ = self
                    .state_service
                    .transition(
                        task.id,
                        updated_generation,
                        DownloadStatus::Paused,
                        DownloadStage::Transferring,
                    )
                    .await;
                return Err(error);
            }
        }
        // aria2 路径：permit 在此处 Drop 释放（aria2 自行管理并发上限）

        // 唤醒 monitor_loop 空闲退避，立即开始监控恢复后的任务
        self.queue_notify.notify_one();

        info!(
            "[DownloadManager] 已恢复任务（重建路径）: {} ({})",
            task.bvid, task.task_type
        );
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::product_file_matches;

    const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "flv", "mov"];

    #[test]
    fn product_match_accepts_exact_and_title_prefixed_hits() {
        // 精确命中：{stem}.{ext}
        assert!(product_file_matches(
            "BV1xx411c7mD.mp4",
            "BV1xx411c7mD",
            VIDEO_EXTS
        ));
        // 标题前缀命中：{title}_{stem}.{ext}
        assert!(product_file_matches(
            "标题_BV1xx411c7mD_p2.mp4",
            "BV1xx411c7mD_p2",
            VIDEO_EXTS
        ));
        // 大小写归一（扩展名大小写不敏感）
        assert!(product_file_matches(
            "BV1xx411c7mD.MP4",
            "BV1xx411c7mD",
            VIDEO_EXTS
        ));
    }

    #[test]
    fn product_match_rejects_prefix_and_page_boundary_collisions() {
        // 单P stem 不得命中其他分P产物
        assert!(!product_file_matches(
            "BV1xx411c7mD_p2.mp4",
            "BV1xx411c7mD",
            VIDEO_EXTS
        ));
        // 分P词根不得命中更大页码（_p20）
        assert!(!product_file_matches(
            "标题_BV1xx411c7mD_p20.mp4",
            "BV1xx411c7mD_p2",
            VIDEO_EXTS
        ));
        assert!(!product_file_matches(
            "BV1xx411c7mD_p20.m4a",
            "BV1xx411c7mD_p2",
            &["m4a"]
        ));
        // 扩展名不匹配
        assert!(!product_file_matches(
            "BV1xx411c7mD.m4s",
            "BV1xx411c7mD",
            VIDEO_EXTS
        ));
    }
}
