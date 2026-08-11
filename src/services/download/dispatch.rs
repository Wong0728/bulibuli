//! aria2 任务投递：并发闸门、Cookie 富化与 aria2 选项组装。

use crate::services::cdn_registry::is_mcdn_url;
use crate::services::concurrency_gate::ConcurrencyPermit;
use crate::services::file_safety::ensure_disk_space;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use tracing::warn;

use super::DownloadManager;

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
        crate::services::bili_url_policy::validate(url).await?;
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
            warn!(
                "[CDN] {bvid} 使用了劣质 MCDN/PCDN 节点: {}...。\
                   可能导致 SSL 错误或速度不稳定，aria2 将自动重试",
                &url[..url.len().min(80)]
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
        options.insert("out".to_string(), json!(filename));

        let gid = self
            .aria2
            .add_download(url, filename, &enriched_cookies, headers, options)
            .await
            .map_err(|e| anyhow!("添加 Aria2 任务失败: {e}"))?;
        Ok((gid, permit))
    }
}
