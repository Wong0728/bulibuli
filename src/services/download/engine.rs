//! 下载引擎选择与原生兜底任务驱动。
//!
//! aria2 始终是默认且优先的下载路径；仅当其重试和实例重建均失败时才降级。
//! `Native`（reqwest 流式下载兜底，见 `native` 模块），恢复后自动切回。
//! 兜底判定基于 aria2 服务状态，而非单个任务失败。

use crate::models::download_task;
use crate::services::aria2::Aria2Status;
use crate::services::concurrency_gate::ConcurrencyPermit;
use crate::services::file_safety::ensure_disk_space;
use anyhow::{anyhow, Result};
use sea_orm::{EntityTrait, Set};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::completion::CompleteOutcome;
use super::native::{NativeProgress, TransferRequest};
use super::{task_cache_key, DownloadManager, ProgressCache};

/// 统一下载引擎（枚举分发，非 trait object）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TransferEngine {
    Aria2,
    Native,
}

impl DownloadManager {
    /// 引擎选择：aria2 可用（或经重建恢复）永远走 aria2；
    /// 只有 aria2 恢复失败后才降级为 Native。
    pub(super) async fn select_engine(&self) -> TransferEngine {
        if self.aria2.is_available().await {
            return TransferEngine::Aria2;
        }
        if self.try_recover_aria2().await {
            return TransferEngine::Aria2;
        }
        warn!("aria2 子系统级完全不可用（重试与实例重建均已耗尽），本次任务降级为原生下载兜底");
        TransferEngine::Native
    }

    /// 穷尽 aria2 恢复手段：重建实例（embedded/system 会重启 aria2c 子进程，
    /// external 刷新 RPC 配置）+ 连续 3 次真实探测（不走可用性缓存）。
    /// 带 60 秒冷却：冷却期内不重复拉起子进程（防重建风暴），仅轻量探测，
    /// 探测成功即视为恢复（满足「aria2 恢复后自动切回」验收标准）。
    async fn try_recover_aria2(&self) -> bool {
        const RECOVER_COOLDOWN: Duration = Duration::from_secs(60);
        let cooldown_active = {
            let guard = self.aria2_recover_failed_at.lock().await;
            guard.is_some_and(|last_failed| last_failed.elapsed() < RECOVER_COOLDOWN)
        };
        if cooldown_active {
            return self.aria2.is_available_uncached().await;
        }
        let settings = self.settings_service.current();
        if let Err(e) = self.aria2.init(settings.as_ref()).await {
            warn!("重建 aria2 实例失败: {e}");
        }
        for _ in 0..3 {
            if self.aria2.is_available_uncached().await {
                *self.aria2_recover_failed_at.lock().await = None;
                info!("aria2 已恢复，切回 aria2 优先下载");
                return true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        *self.aria2_recover_failed_at.lock().await = Some(Instant::now());
        false
    }

    /// 按引擎投递传输：aria2 返回 `Some(gid)` 并附带并发许可；
    /// Native 只做前置校验并返回 `None`，待调用方把任务行落库后再
    /// `spawn_native_transfer` 启动传输（保证进度更新读到的 generation/filename
    /// 是最终值）。两种路径均返回 `ConcurrencyPermit`，调用方须持有该 permit
    /// 跨越 spawn 过程，spawn 失败时 permit 自动释放（Drop）。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dispatch_transfer(
        &self,
        engine: TransferEngine,
        task_id: i32,
        url: &str,
        bvid: &str,
        cookies: &str,
        dir: &Path,
        filename: &str,
    ) -> Result<(Option<String>, ConcurrencyPermit)> {
        match engine {
            TransferEngine::Aria2 => {
                let (gid, permit) = self
                    .add_to_aria2(task_id, url, bvid, cookies, dir, filename)
                    .await?;
                Ok((Some(gid), permit))
            }
            TransferEngine::Native => {
                // aria2 会自动建目录，原生路径需自建；磁盘校验与 aria2 路径对齐
                ensure_disk_space(dir, None).await?;
                tokio::fs::create_dir_all(dir).await?;
                // 为原生路径也获取并发许可，保持与 aria2 路径一致的 TOCTOU 保护
                let wait_seconds = self
                    .settings_service
                    .current()
                    .parallel_download
                    .wait_slot_timeout;
                let permit = self
                    .concurrency_gate
                    .acquire_timeout(std::time::Duration::from_secs(wait_seconds))
                    .await
                    .ok_or_else(|| anyhow!("等待下载槽位超过 {wait_seconds} 秒"))?;
                Ok((None, permit))
            }
        }
    }

    /// 启动一条原生兜底传输：注册取消令牌后 spawn 后台任务自驱动
    /// 进度广播（ProgressWriter/WS）与完成/失败落库。
    /// `permit` 为并发许可，由 `dispatch_transfer` 获取，在此转移至 spawned task，
    /// spawn 失败时 permit 自动 Drop 释放槽位。
    pub(super) async fn spawn_native_transfer(
        &self,
        task_id: i32,
        url: &str,
        cookies: &str,
        uid: Option<&str>,
        permit: ConcurrencyPermit,
    ) -> Result<()> {
        crate::services::bili_url_policy::validate(url).await?;
        let task = download_task::Entity::find_by_id(task_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| anyhow!("原生下载任务不存在: {task_id}"))?;
        let dir = self.task_download_dir(&task).await;
        let filename = task
            .filename
            .clone()
            .unwrap_or_else(|| format!("{}.{}", task.bvid, task.task_type));
        // 与 aria2 路径一致：富化 Cookie（buvid3/bili_ticket/...）以绕过 CDN 403/-799 风控。
        let enriched_cookies = match self.bili_api.enrich_cookies_public(cookies).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "富化 cookies 失败（降级为原始 cookies） bvid={}: {e}",
                    task.bvid
                );
                cookies.to_string()
            }
        };
        let mut headers = vec![
            ("User-Agent".to_string(), self.config.user_agent.clone()),
            (
                "Referer".to_string(),
                format!("https://www.bilibili.com/video/{}", task.bvid),
            ),
            ("Origin".to_string(), "https://www.bilibili.com".to_string()),
        ];
        if !enriched_cookies.is_empty() {
            headers.push(("Cookie".to_string(), enriched_cookies));
        }
        let request = TransferRequest {
            url: url.to_string(),
            headers,
            target: dir.join(&filename),
        };
        let cancel = self.cancellation.child_token();
        self.native_tasks
            .lock()
            .await
            .insert(task_id, cancel.clone());
        info!(
            "原生兜底下载启动: {} ({}) -> {}",
            task.bvid,
            task.task_type,
            request.target.display()
        );
        let manager = self.clone();
        let uid = uid.map(str::to_string);
        tokio::spawn(async move {
            manager
                .run_native_transfer(task, request, cancel, uid, permit)
                .await;
        });
        Ok(())
    }

    /// 原生传输主体：持有并发许可执行下载，每秒广播一次进度；
    /// 成功后复用 aria2 完成管线（MD5 去重/写历史/封面/合并），失败按守卫落库。
    /// `permit` 由 `spawn_native_transfer` 转入，在整个传输过程中持有，
    /// 函数结束时 permit 随 Drop 自动释放。
    async fn run_native_transfer(
        &self,
        task: download_task::Model,
        request: TransferRequest,
        cancel: CancellationToken,
        uid: Option<String>,
        permit: ConcurrencyPermit,
    ) {
        let progress = Arc::new(NativeProgress::default());
        let host = url::Url::parse(&request.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned));
        let filename = request
            .target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let download = self.native.download(&request, &progress, &cancel);
        tokio::pin!(download);
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_downloaded: i64 = 0;
        let result = loop {
            tokio::select! {
                result = &mut download => break result,
                _ = ticker.tick() => {
                    let downloaded = progress.downloaded();
                    let total = progress.total();
                    let speed = (downloaded - last_downloaded).max(0);
                    last_downloaded = downloaded;
                    let percent = if total > 0 {
                        ((downloaded * 100) / total).clamp(0, 100) as i32
                    } else {
                        0
                    };
                    self.progress_cache.lock().await.insert(
                        task_cache_key(&task.bvid, task.cid),
                        ProgressCache {
                            progress_percent: percent,
                            downloaded_size: downloaded,
                            total_size: total,
                            speed,
                        },
                    );
                    self.broadcast_progress(&task, "downloading", percent, downloaded, total, speed, None)
                        .await;
                }
            }
        };
        drop(permit);
        self.native_tasks.lock().await.remove(&task.id);
        self.progress_cache
            .lock()
            .await
            .remove(&task_cache_key(&task.bvid, task.cid));

        if cancel.is_cancelled() {
            // 用户移除任务或程序关停：任务行已删除/无需再写终态
            info!("原生下载已取消: {} ({})", task.bvid, task.task_type);
            return;
        }
        match result {
            Ok(total) => {
                // 复用坏 CDN 熔断：成功清除该 host 的失败计数
                if let Some(host) = &host {
                    self.bili_api.bad_cdns().record_success(host).await;
                }
                // 合成 complete 状态复用 aria2 完成管线：
                // MD5 去重归位、写历史、下封面、广播、触发音视频合并
                let status = Aria2Status {
                    status: "complete".to_string(),
                    progress_percent: 100,
                    downloaded_size: total,
                    total_size: total,
                    speed: 0,
                    error_message: None,
                    filename,
                };
                if let CompleteOutcome::Finished { uid: resolved } =
                    self.handle_complete(&task, &status).await
                {
                    let uid = resolved.or(uid);
                    if let Err(e) = self
                        .on_task_completed(&task.bvid, task.cid, uid.as_deref())
                        .await
                    {
                        error!("处理原生下载完成任务失败 {}: {e}", task.bvid);
                    }
                }
            }
            Err(e) => {
                // 复用坏 CDN 熔断：失败计入该 host（两次即熔断 10 分钟），
                // 后续重试重新解析 URL 时上游会避开该 host（多 URL 轮试）
                if let Some(host) = &host {
                    self.bili_api.bad_cdns().record_failure(host).await;
                }
                warn!("原生下载失败 {} ({}): {e}", task.bvid, task.task_type);
                let message = format!("原生下载失败: {e}");
                let mut model: download_task::ActiveModel = task.clone().into();
                model.status = Set("failed".to_string());
                model.error = Set(Some(message.clone()));
                model.speed = Set(0);
                self.apply_guarded_update(&task.bvid, task.id, task.generation, model)
                    .await;
                self.log_bvid(
                    &task.bvid,
                    uid.as_deref(),
                    &format!("下载失败（{}，原生兜底）：{e}", task.task_type),
                    "error",
                )
                .await;
                self.broadcast_progress(
                    &task,
                    "failed",
                    task.progress_percent,
                    progress.downloaded(),
                    progress.total(),
                    0,
                    Some(&message),
                )
                .await;
            }
        }
    }
}
