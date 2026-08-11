//! Aria2 JSON-RPC 层：可用性探测、通用调用（带重试）与各业务 RPC 方法。

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::warn;

use super::{
    Aria2Error, Aria2Manager, Aria2Mode, Aria2Status, AVAILABILITY_CACHE_TTL, BASE_RETRY_DELAY_MS,
    MAX_RETRIES,
};

impl Aria2Manager {
    /// 查询 Aria2 RPC 是否可用（带 TTL 缓存）。
    ///
    /// 多数调用方（如 `status()`、`get_status`）只关心"现在能不能用"，
    /// 不必每次都发起 `aria2.getVersion` RPC。缓存 TTL 由 `AVAILABILITY_CACHE_TTL` 控制。
    ///
    /// 启动轮询、显式健康检查等需要实时结果的场景请使用 `is_available_uncached`。
    pub async fn is_available(&self) -> bool {
        // 快速路径：命中缓存直接返回，避免获取锁后还要走 RPC
        {
            let inner = self.inner.lock().await;
            if let Some((available, ts)) = inner.available_cache {
                if ts.elapsed() < AVAILABILITY_CACHE_TTL {
                    return available;
                }
            }
        }
        // 缓存过期：发起真实 RPC 探测
        let available = self.is_available_uncached().await;
        // 写回缓存（即使 false 也写，避免故障期每秒一次的 RPC 雪崩）
        let mut inner = self.inner.lock().await;
        inner.available_cache = Some((available, Instant::now()));
        available
    }

    /// 不走缓存的可用性探测，直接发起 `aria2.getVersion` RPC。
    /// 用于启动轮询（需要每次都拿到真实状态）和显式健康检查。
    pub async fn is_available_uncached(&self) -> bool {
        self.call("aria2.getVersion", vec![]).await.is_ok()
    }

    /// 返回 Aria2 状态字符串：`connected` / `starting` / `disconnected` / `failed`。
    /// `starting` 表示 aria2c 进程已启动但 RPC 尚未就绪（启动重试期间）。
    pub async fn status(&self) -> &'static str {
        if self.is_available().await {
            let mut inner = self.inner.lock().await;
            inner.ready = true;
            inner.last_error = None;
            "connected"
        } else {
            let mut inner = self.inner.lock().await;
            let mut process_exited = None;
            if let Some(child) = inner.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(exit_status)) => process_exited = Some(exit_status.to_string()),
                    Ok(None) => {}
                    Err(error) => {
                        inner.last_error = Some(format!("检查 aria2c 进程状态失败: {error}"));
                    }
                }
            }
            if let Some(exit_status) = process_exited {
                inner.child.take();
                inner.last_error = Some(format!("aria2c 已退出（{exit_status}）"));
                inner.ready = false;
                inner.available_cache = Some((false, Instant::now()));
                return "failed";
            }

            let within_startup_window = !inner.ready
                && inner
                    .started_at
                    .is_some_and(|started_at| started_at.elapsed() <= Duration::from_secs(7));
            if within_startup_window && (inner.child.is_some() || inner.mode == Aria2Mode::External)
            {
                "starting"
            } else if inner.last_error.is_some() && inner.mode != Aria2Mode::External {
                "failed"
            } else {
                "disconnected"
            }
        }
    }

    /// 面向健康检查和前端诊断的非敏感状态详情。
    pub async fn diagnostics(&self) -> Value {
        let state = self.status().await;
        let inner = self.inner.lock().await;
        json!({
            "state": state,
            "mode": inner.mode.as_str(),
            "process_alive": inner.child.is_some(),
            "rpc_reachable": state == "connected",
            "endpoint": inner.rpc_url,
            "last_error": inner.last_error,
            "starting_for_ms": inner.started_at.map(|time| time.elapsed().as_millis() as u64),
        })
    }

    pub(super) async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        let mode = self.inner.lock().await.mode;
        // embedded 模式下不重试：内置进程要么在运行要么不在，重试只会产生无意义延迟。
        let max_retries = if mode == Aria2Mode::Embedded {
            1
        } else {
            MAX_RETRIES
        };

        let mut last_error = None;
        for attempt in 0..max_retries {
            match self.call_once(method, params.clone()).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    warn!(
                        "Aria2 RPC 调用失败 (attempt {}/{}): {e}",
                        attempt + 1,
                        max_retries
                    );
                    last_error = Some(e);
                    if attempt < max_retries - 1 {
                        let delay = BASE_RETRY_DELAY_MS * 2_u64.pow(attempt);
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                }
            }
        }
        Err(anyhow!(last_error.unwrap_or_else(|| {
            Aria2Error::Unexpected("未知错误".to_string())
        })))
    }

    async fn call_once(&self, method: &str, params: Vec<Value>) -> Result<Value, Aria2Error> {
        let inner = self.inner.lock().await;
        let secret = inner.secret.clone();
        let url = inner.rpc_url.clone();
        drop(inner);

        let mut real_params = vec![];
        if !secret.is_empty() {
            real_params.push(json!(format!("token:{secret}")));
        }
        real_params.extend(params);

        let body = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": method,
            "params": real_params,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    Aria2Error::Timeout(e.to_string())
                } else if e.is_connect() {
                    Aria2Error::Connection(e.to_string())
                } else {
                    Aria2Error::Unexpected(e.to_string())
                }
            })?;

        let status = resp.status();
        if !status.is_success() {
            let body_preview = resp.text().await.unwrap_or_default();
            let preview: String = body_preview.chars().take(500).collect();
            warn!("Aria2 RPC 返回非2xx状态: HTTP {status}, 响应: {preview}");
            return Err(Aria2Error::Unexpected(format!("HTTP {status}: {preview}")));
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Aria2Error::Unexpected(format!("读取Aria2响应体失败: {e}")))?;
        let data: Value = serde_json::from_slice(&bytes).map_err(|e| {
            let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(500)]);
            Aria2Error::Unexpected(format!(
                "解析Aria2响应失败: {e}，Content-Type: {content_type}，前500字节: {preview}"
            ))
        })?;
        if let Some(err) = data.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("未知 RPC 错误")
                .to_string();
            return Err(Aria2Error::Rpc { code, message });
        }
        data.get("result")
            .cloned()
            .ok_or_else(|| Aria2Error::Unexpected("Aria2 RPC 响应中缺少 result 字段".to_string()))
    }

    pub async fn add_download(
        &self,
        url: &str,
        filename: &str,
        cookies: &str,
        headers: HashMap<String, String>,
        options: HashMap<String, Value>,
    ) -> Result<String> {
        let mut aria_options = json!(options);
        let obj = aria_options
            .as_object_mut()
            .ok_or_else(|| anyhow!("Aria2 选项不是 JSON 对象"))?;
        obj.insert("out".to_string(), json!(filename));

        // 单任务 SSL/网络容错选项：即使全局配置被覆盖，单任务仍能正确处理。
        obj.entry("check-certificate")
            .or_insert_with(|| json!("true"));
        obj.entry("max-http-redirect").or_insert_with(|| json!("0"));
        obj.entry("timeout").or_insert_with(|| json!("30"));
        obj.entry("connect-timeout").or_insert_with(|| json!("20"));
        obj.entry("lowest-speed-limit")
            .or_insert_with(|| json!("1K"));

        if !cookies.is_empty() {
            obj.insert("header".to_string(), json!([format!("Cookie: {cookies}")]));
        }
        let header_list: Vec<String> = headers.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        if !header_list.is_empty() {
            let existing = obj.entry("header").or_insert_with(|| json!([]));
            let arr = existing
                .as_array_mut()
                .ok_or_else(|| anyhow!("Aria2 header 选项不是数组"))?;
            for h in header_list {
                arr.push(json!(h));
            }
        }

        let result = self
            .call("aria2.addUri", vec![json!([url]), aria_options])
            .await?;
        let gid = result.as_str().context("Aria2 返回的 gid 不是字符串")?;
        Ok(gid.to_string())
    }

    pub async fn get_download_status(&self, gid: &str) -> Result<Aria2Status> {
        let result = self
            .call(
                "aria2.tellStatus",
                vec![
                    json!(gid),
                    json!([
                        "status",
                        "totalLength",
                        "completedLength",
                        "downloadSpeed",
                        "errorMessage",
                        "files"
                    ]),
                ],
            )
            .await?;
        Ok(Aria2Status::from_json(&result))
    }

    pub async fn remove(&self, gid: &str) -> Result<()> {
        self.call("aria2.remove", vec![json!(gid)]).await?;
        Ok(())
    }

    /// 运行时修改 aria2 全局选项（如 `max-concurrent-downloads`）。
    /// 实际并发由 aria2 的 `--max-concurrent-downloads` 决定，设置页改并发数后
    /// 必须经此 RPC 同步，否则运行期改动不生效。
    pub async fn change_global_option(&self, key: &str, value: &str) -> Result<()> {
        self.call("aria2.changeGlobalOption", vec![json!({ key: value })])
            .await?;
        Ok(())
    }

    /// 预留磁盘空间耗尽时暂停所有活动中的 aria2 传输。
    pub async fn pause_all(&self) -> Result<()> {
        self.call("aria2.pauseAll", vec![]).await?;
        Ok(())
    }

    /// 暂停单个 aria2 任务（优雅暂停：等当前连接排空后再切换为 paused）。
    /// 用于下载队列的「单任务暂停」操作。
    pub async fn pause(&self, gid: &str) -> Result<()> {
        self.call("aria2.pause", vec![json!(gid)]).await?;
        Ok(())
    }

    /// 恢复单个被暂停的 aria2 任务，重新进入 aria2 调度队列。
    pub async fn unpause(&self, gid: &str) -> Result<()> {
        self.call("aria2.unpause", vec![json!(gid)]).await?;
        Ok(())
    }

    /// 恢复所有被暂停的 aria2 任务（与 `pause_all` 对应）。
    pub async fn unpause_all(&self) -> Result<()> {
        self.call("aria2.unpauseAll", vec![]).await?;
        Ok(())
    }
}
