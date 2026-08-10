//! aria2 子系统不可用时的 reqwest 流式下载兜底。
//!
//! 仅作为最后兜底，引擎选择逻辑见 `engine`；aria2 恢复后自动切回。
//! - `probe`：HEAD 探测 Content-Length / Accept-Ranges，失败时 Range: bytes=0-0 二次探测
//! - `transfer`：分片 seek 定点写入，分片失败自动降级单线程整段下载

mod probe;
mod transfer;

use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Client;
use reqwest::{Method, Response};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// 分片下载启用阈值：小于该值直接单线程整段下载（分片收益不足）。
const CHUNK_THRESHOLD: i64 = 8 * 1024 * 1024;
/// 分片数：兜底路径不追求极限速度，取保守值（对齐 aria2 默认 split 量级）。
const CHUNK_COUNT: i64 = 4;

/// 原生下载进度（供管理侧定时读取并广播到 ProgressWriter/WS）。
#[derive(Default)]
pub(super) struct NativeProgress {
    downloaded: AtomicI64,
    total: AtomicI64,
}

impl NativeProgress {
    pub fn downloaded(&self) -> i64 {
        self.downloaded.load(Ordering::Relaxed)
    }

    pub fn total(&self) -> i64 {
        self.total.load(Ordering::Relaxed)
    }

    fn add_downloaded(&self, bytes: i64) {
        self.downloaded.fetch_add(bytes, Ordering::Relaxed);
    }

    fn reset_downloaded(&self) {
        self.downloaded.store(0, Ordering::Relaxed);
    }

    fn set_downloaded(&self, downloaded: i64) {
        self.downloaded.store(downloaded, Ordering::Relaxed);
    }

    fn set_total(&self, total: i64) {
        self.total.store(total, Ordering::Relaxed);
    }
}

/// 一次原生下载请求：URL、请求头（含富化后的 Cookie）与目标文件路径。
pub(super) struct TransferRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub target: PathBuf,
}

/// reqwest 原生下载器：探测 → 分片（可降级）→ 大小校验。
#[derive(Clone)]
pub(super) struct NativeDownloader {
    client: Client,
}

impl NativeDownloader {
    pub fn new(tls_verify: bool) -> Result<Self> {
        // 流式下载不能设总超时（大文件必然超）；用连接超时 + 读超时兜住停滞连接，
        // 与 aria2 使用相同的连接和停滞超时。
        let client = Client::builder()
            .danger_accept_invalid_certs(!tls_verify)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(30))
            .build()
            .context("创建原生下载 HTTP 客户端失败")?;
        Ok(Self { client })
    }

    /// 执行下载并返回最终文件大小（字节）。
    /// 流程：探测大小/断点支持 → 满足条件走分片（失败自动降级单线程）→ 校验文件大小。
    pub async fn download(
        &self,
        request: &TransferRequest,
        progress: &Arc<NativeProgress>,
        cancel: &CancellationToken,
    ) -> Result<i64> {
        let headers = build_header_map(&request.headers)?;
        let outcome = probe::probe(&self.client, &request.url, &headers).await;
        if let Some(total) = outcome.total_size {
            progress.set_total(total);
        }
        let chunkable = outcome.supports_ranges
            && outcome
                .total_size
                .is_some_and(|total| total >= CHUNK_THRESHOLD);
        if chunkable {
            let total = outcome.total_size.unwrap_or_default();
            match transfer::chunked(&self.client, request, &headers, total, progress, cancel).await
            {
                Ok(()) => return finalize(&request.target, Some(total)).await,
                Err(error) => {
                    // 分片失败后改用单线程整段下载。
                    warn!("原生分片下载失败，降级为单线程整段下载: {error}");
                    progress.reset_downloaded();
                }
            }
        }
        transfer::single(&self.client, request, &headers, progress, cancel).await?;
        if progress.total() <= 0 {
            progress.set_total(progress.downloaded());
        }
        finalize(&request.target, outcome.total_size).await
    }
}

async fn send_validated(
    client: &Client,
    method: Method,
    url: &str,
    headers: &HeaderMap,
    range: Option<String>,
) -> Result<Response> {
    let mut current = url.to_string();
    for redirect_count in 0..=5 {
        crate::services::bili_url_policy::validate(&current)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let mut request = client
            .request(method.clone(), &current)
            .headers(headers.clone());
        if let Some(range) = range.as_ref() {
            request = request.header(reqwest::header::RANGE, range);
        }
        let response = request.send().await.context("下载请求失败")?;
        if !response.status().is_redirection() {
            return Ok(response);
        }
        if redirect_count == 5 {
            bail!("下载重定向超过 5 次");
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("下载重定向缺少有效 Location")?;
        current = response
            .url()
            .join(location)
            .context("下载重定向 URL 无效")?
            .to_string();
    }
    unreachable!("redirect loop returns or errors")
}

/// 校验落盘文件大小与探测值一致（探测不到大小时跳过校验），返回实际大小。
async fn finalize(target: &Path, expected: Option<i64>) -> Result<i64> {
    let metadata = tokio::fs::metadata(target)
        .await
        .context("读取下载文件元数据失败")?;
    let size = metadata.len() as i64;
    if let Some(expected) = expected {
        if expected > 0 && size != expected {
            bail!("下载文件大小不符：期望 {expected} 字节，实际 {size} 字节");
        }
    }
    Ok(size)
}

fn build_header_map(pairs: &[(String, String)]) -> Result<HeaderMap> {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("非法请求头名: {name}"))?;
        let header_value =
            HeaderValue::from_str(value).with_context(|| format!("非法请求头值: {name}"))?;
        map.insert(header_name, header_value);
    }
    Ok(map)
}
