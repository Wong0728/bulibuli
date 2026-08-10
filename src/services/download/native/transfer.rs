//! 传输实现：分片 seek 定点写入与单线程整段流式下载。

use anyhow::{bail, Context, Result};
use futures::StreamExt;
use reqwest::header::HeaderMap;
use reqwest::{Client, StatusCode};
use std::io::SeekFrom;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{NativeProgress, TransferRequest, CHUNK_COUNT};

/// 分片下载：预分配文件长度后各分片独立打开文件句柄 seek 定点写入。
/// 仅重试失败分片；连续失败后才由调用方降级单线程整段下载。
pub(super) async fn chunked(
    client: &Client,
    request: &TransferRequest,
    headers: &HeaderMap,
    total: i64,
    progress: &Arc<NativeProgress>,
    cancel: &CancellationToken,
) -> Result<()> {
    // 预分配文件长度，保证各分片 seek 写入互不越界
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&request.target)
        .await
        .context("创建下载目标文件失败")?;
    file.set_len(total as u64)
        .await
        .context("预分配下载文件长度失败")?;
    drop(file);

    // 均分分片，余数归最后一片（调用方已保证 total ≥ 分片阈值，chunk_size 必为正）
    let chunk_size = total / CHUNK_COUNT;
    let mut remaining = Vec::new();
    for index in 0..CHUNK_COUNT {
        let start = index * chunk_size;
        let end = if index == CHUNK_COUNT - 1 {
            total - 1
        } else {
            start + chunk_size - 1
        };
        remaining.push((start, end));
    }

    let mut completed = 0_i64;
    let mut last_error = None;
    for attempt in 1..=3 {
        progress.set_downloaded(completed);
        let results = futures::future::join_all(remaining.iter().map(|(start, end)| {
            fetch_chunk(client, request, headers, *start, *end, progress, cancel)
        }))
        .await;

        let mut failed = Vec::new();
        for ((start, end), result) in remaining.into_iter().zip(results) {
            match result {
                Ok(()) => completed += end - start + 1,
                Err(error) => {
                    warn!("原生分片下载失败: bytes={start}-{end}, error={error}");
                    last_error = Some(error);
                    failed.push((start, end));
                }
            }
        }
        progress.set_downloaded(completed);
        if failed.is_empty() {
            return Ok(());
        }
        remaining = failed;
        if attempt < 3 {
            tokio::time::sleep(std::time::Duration::from_millis(250 * attempt)).await;
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("原生分片下载失败")))
}

/// 下载单个分片并写入 `[start, end]` 字节区间。
async fn fetch_chunk(
    client: &Client,
    request: &TransferRequest,
    headers: &HeaderMap,
    start: i64,
    end: i64,
    progress: &Arc<NativeProgress>,
    cancel: &CancellationToken,
) -> Result<()> {
    let response = super::send_validated(
        client,
        reqwest::Method::GET,
        &request.url,
        headers,
        Some(format!("bytes={start}-{end}")),
    )
    .await
    .context("分片请求失败")?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        bail!("分片请求未返回 206（实际 {}）", response.status());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .open(&request.target)
        .await
        .context("打开下载目标文件失败")?;
    file.seek(SeekFrom::Start(start as u64))
        .await
        .context("分片 seek 失败")?;
    let expected = end - start + 1;
    let written = write_stream(response, &mut file, progress, cancel).await?;
    if written != expected {
        bail!("分片字节数不符：期望 {expected}，实际 {written}");
    }
    Ok(())
}

/// 单线程整段下载：不依赖 Range 支持，适用于分片失败降级与大小未知的响应。
pub(super) async fn single(
    client: &Client,
    request: &TransferRequest,
    headers: &HeaderMap,
    progress: &Arc<NativeProgress>,
    cancel: &CancellationToken,
) -> Result<()> {
    let response = super::send_validated(client, reqwest::Method::GET, &request.url, headers, None)
        .await
        .context("下载请求失败")?;
    if !response.status().is_success() {
        bail!("下载请求返回 HTTP {}", response.status());
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&request.target)
        .await
        .context("创建下载目标文件失败")?;
    write_stream(response, &mut file, progress, cancel).await?;
    Ok(())
}

/// 把响应体流式写入文件，累计进度并响应取消信号。返回写入字节数。
async fn write_stream(
    response: reqwest::Response,
    file: &mut tokio::fs::File,
    progress: &Arc<NativeProgress>,
    cancel: &CancellationToken,
) -> Result<i64> {
    let mut stream = response.bytes_stream();
    let mut written: i64 = 0;
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => bail!("下载已取消"),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let bytes = chunk.context("读取下载数据流失败")?;
        file.write_all(&bytes).await.context("写入下载文件失败")?;
        written += bytes.len() as i64;
        progress.add_downloaded(bytes.len() as i64);
    }
    // tokio 文件句柄内部有缓冲，必须显式 flush 才能保证数据全部落盘
    file.flush().await.context("刷新下载文件失败")?;
    Ok(written)
}
