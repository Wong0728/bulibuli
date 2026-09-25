//! aria2 任务投递：并发闸门、Cookie 富化与 aria2 选项组装；
//! 以及既有任务（重复入队/重试）在清理旧传输后的状态重置与重新派发。

use crate::models::download_task;
use crate::services::cdn_registry::is_mcdn_url;
use crate::services::concurrency_gate::ConcurrencyPermit;
use crate::services::file_safety::{
    ensure_disk_space, ensure_existing_within_root, sanitize_filename,
};
use anyhow::{anyhow, Result};
use sea_orm::{ActiveModelTrait, Set};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use tracing::{error, info, warn};

use super::engine::TransferEngine;
use super::{DownloadManager, TaskOutcome};

impl DownloadManager {
    /// 向 aria2 投递下载任务并获取并发许可。
    /// 返回 `(gid, permit)`：调用方须持有 `permit` 跨越 spawn，
    /// spawn 成功后将 permit 转移给任务；spawn 失败则 permit 自动释放（Drop）。
    pub(super) async fn add_to_aria2(
        &self,
        _task_id: i32,
        url: &str,
        bvid: &str,
        cookies: &str,
        dir: &Path,
        filename: &str,
    ) -> Result<(String, ConcurrencyPermit)> {
        crate::services::bili_url_policy::validate_download_url(url).await?;
        ensure_existing_within_root(&self.paths.download_dir, dir).await?;
        let filename = sanitize_filename(filename);
        let max_parallel = self
            .settings_service
            .current()
            .parallel_download
            .max_parallel;
        // aria2 自行调度任务，必须同时更新其全局并发选项。
        if self.concurrency_gate.set_limit(max_parallel).await {
            if let Err(e) = self
                .aria2
                .change_global_option("max-concurrent-downloads", &max_parallel.to_string())
                .await
            {
                warn!("同步 max-concurrent-downloads={max_parallel} 到 aria2 失败: {e}");
            }
        }
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
        ensure_disk_space(dir, None).await?;

        // CDN 过滤：如果 URL 来自劣质 MCDN/PCDN 节点，记录警告但不阻塞下载
        // （B 站有时只返回单个 URL，无法换源，只能记录并依赖 aria2 重试）。
        if is_mcdn_url(url) {
            // 字符边界安全截断：URL 是外部输入，字节切片可能切开多字节字符导致 panic
            // （release 为 panic=abort，会直接杀死进程）。
            let prefix: String = url.chars().take(80).collect();
            warn!(
                "[CDN] {bvid} 使用了劣质 MCDN/PCDN 节点: {prefix}...。\
                   可能导致 SSL 错误或速度不稳定，aria2 将自动重试"
            );
        }

        let mut headers = HashMap::new();
        headers.insert("User-Agent".to_string(), self.config.user_agent.clone());
        headers.insert(
            "Referer".to_string(),
            format!("https://www.bilibili.com/video/{bvid}"),
        );
        headers.insert("Origin".to_string(), "https://www.bilibili.com".to_string());

        // 富化 Cookie：合并设备指纹（buvid3/bili_ticket/...）。
        // 否则 B 站 CDN 会以 403/-799 风控拒绝 m4s/m4a 下载。
        // enrich 失败时降级为原始 Cookie，避免阻塞下载（会记录 warn 便于排查）。
        let enriched_cookies = match self.bili_api.enrich_cookies_public(cookies).await {
            Ok(c) => c,
            Err(e) => {
                warn!("富化 cookies 失败（降级为原始 cookies） bvid={bvid}: {e}");
                cookies.to_string()
            }
        };
        // Cookie 只随白名单内 B 站主机出站：CDN 链接的鉴权在签名 query 里
        // （对标 Bili23），名单外的新 PCDN 域照常下载但不附带 Cookie，
        // 保证登录凭据永不出 B 站域。
        let scoped_cookies = if crate::services::bili_url_policy::host_allows_credentials(url) {
            enriched_cookies
        } else {
            info!("[CDN] {bvid} 下载主机不在 B 站域白名单内，本次下载不携带 Cookie");
            String::new()
        };

        let settings = self.settings_value().await?;
        let aria2_basic = settings.get("aria2c_basic").cloned().unwrap_or_default();
        let mut options: HashMap<String, Value> = HashMap::new();
        if let Some(v) = aria2_basic.get("split").and_then(|v| v.as_i64()) {
            options.insert("split".to_string(), json!(v));
        }
        if let Some(v) = aria2_basic
            .get("max_connection_per_server")
            .and_then(|v| v.as_i64())
        {
            options.insert("max-connection-per-server".to_string(), json!(v));
        }
        if let Some(v) = aria2_basic.get("min_split_size").and_then(|v| v.as_str()) {
            options.insert("min-split-size".to_string(), json!(v));
        }
        if let Some(v) = aria2_basic.get("max_tries").and_then(|v| v.as_i64()) {
            options.insert("max-tries".to_string(), json!(v));
        }
        if let Some(v) = aria2_basic.get("retry_wait").and_then(|v| v.as_i64()) {
            options.insert("retry-wait".to_string(), json!(v));
        }
        options.insert("dir".to_string(), json!(dir.to_string_lossy().to_string()));
        options.insert("out".to_string(), json!(&filename));

        let gid = self
            .aria2
            .add_download(url, &filename, &scoped_cookies, headers, options)
            .await
            .map_err(|e| anyhow!("添加 Aria2 任务失败: {e}"))?;
        Ok((gid, permit))
    }

    /// 既有任务（completed 缺产物、来源切换、失败重试等）的重置与重新派发：
    /// 先收掉上一轮仍在跑的传输（native 取消令牌 + aria2 旧 gid），
    /// 再重置任务行状态并重新投递引擎。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn reset_and_dispatch_existing_task(
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
        // 重新派发前先收掉上一轮仍在跑的传输：cleanup_task_caches 只清内存缓存，不碰外部
        // 引擎，否则同一个 dir/out 上会挂两个任务并发写同一份文件。
        if let Some(token) = self.native_tasks.lock().await.remove(&download_id) {
            token.cancel();
        }
        if let Some(stale_gid) = existing.gid.as_deref() {
            if let Err(error) = self.aria2.remove(stale_gid).await {
                warn!("重新派发前移除旧 Aria2 任务失败 gid={stale_gid}: {error}");
            }
        }
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
        // retry_task 已把任务置为 retrying 并写好 attempts / 退避时间，此处不能再清零，
        // 否则自动重试计数被抹平、MAX_VIDEO_RETRIES 永不触顶（失败任务会 30 秒一轮无限重打）。
        let is_retry_dispatch = existing.status == "retrying";
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
        if !is_retry_dispatch {
            // 用户重新添加同一链接视为新的失败回合，重置自动重试额度。
            model.attempts = Set(0);
            model.next_retry_at = Set(None);
        }
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
}
