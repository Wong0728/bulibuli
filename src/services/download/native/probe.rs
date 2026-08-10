//! 下载前探测：确定文件总大小与服务端是否支持 Range 断点。

use reqwest::header::{HeaderMap, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE};
use reqwest::{Client, StatusCode};
use tracing::debug;

/// 探测结论：`total_size=None` 表示大小未知（只能单线程流式下）。
pub(super) struct ProbeOutcome {
    pub total_size: Option<i64>,
    pub supports_ranges: bool,
}

/// 先 HEAD 探测 Content-Length + Accept-Ranges；HEAD 被拒或缺字段时，
/// 用 `Range: bytes=0-0` GET 二次探测（206 + Content-Range 可同时拿到两项信息）。
/// 探测失败不视为错误：返回未知大小 + 不支持分片，由单线程整段下载兜底。
pub(super) async fn probe(client: &Client, url: &str, headers: &HeaderMap) -> ProbeOutcome {
    if let Some(outcome) = probe_via_head(client, url, headers).await {
        return outcome;
    }
    probe_via_range(client, url, headers).await
}

async fn probe_via_head(client: &Client, url: &str, headers: &HeaderMap) -> Option<ProbeOutcome> {
    let response =
        match super::send_validated(client, reqwest::Method::HEAD, url, headers, None).await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                debug!("HEAD 探测返回非 2xx（{}），改用 Range 探测", r.status());
                return None;
            }
            Err(error) => {
                debug!("HEAD 探测失败，改用 Range 探测: {error}");
                return None;
            }
        };
    let total_size = header_i64(response.headers(), CONTENT_LENGTH.as_str());
    // 部分 CDN 不回 Accept-Ranges 但实际支持 Range；只有拿到明确的
    // Content-Length + Accept-Ranges: bytes 才直接采信，否则继续 Range 探测确认
    let supports_ranges = response
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("bytes"));
    if total_size.is_some() && supports_ranges {
        return Some(ProbeOutcome {
            total_size,
            supports_ranges,
        });
    }
    None
}

async fn probe_via_range(client: &Client, url: &str, headers: &HeaderMap) -> ProbeOutcome {
    let response = match super::send_validated(
        client,
        reqwest::Method::GET,
        url,
        headers,
        Some("bytes=0-0".to_string()),
    )
    .await
    {
        Ok(r) => r,
        Err(error) => {
            debug!("Range 探测失败，按未知大小处理: {error}");
            return ProbeOutcome {
                total_size: None,
                supports_ranges: false,
            };
        }
    };
    if response.status() == StatusCode::PARTIAL_CONTENT {
        // Content-Range: bytes 0-0/12345 → 总大小在斜杠之后
        let total_size = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0);
        return ProbeOutcome {
            total_size,
            supports_ranges: true,
        };
    }
    // 200：服务端忽略 Range，不支持断点；Content-Length 即总大小
    let total_size = header_i64(response.headers(), CONTENT_LENGTH.as_str());
    ProbeOutcome {
        total_size,
        supports_ranges: false,
    }
}

fn header_i64(headers: &HeaderMap, name: &str) -> Option<i64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
}
