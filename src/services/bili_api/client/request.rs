//! 统一 GET 请求构建与发送管线：请求头/Cookie 注入、限流扣减、
//! 429 Retry-After 重试、5xx 指数退避与网络错误重试。
//!
//! 限流配额只在真正发送处扣减一次（构建请求不扣减），后台批量探测
//! 通过 `send_with_retry_limited` 走独立限流桶。

use anyhow::{anyhow, Context, Result};
use reqwest::RequestBuilder;
use std::collections::HashMap;
use tracing::warn;

use crate::services::bili_api::BiliApi;

impl BiliApi {
    /// 供弹幕、评论等业务复用统一限流、请求头和网络重试管线。
    pub async fn send_get_public(
        &self,
        url: &str,
        params: &HashMap<String, String>,
        referer: &str,
        cookies: &str,
    ) -> Result<reqwest::Response> {
        let request = self.build_get_request(url, params, referer, cookies).await;
        self.send_with_retry(request).await
    }

    /// 统一构建 GET 请求：注入 User-Agent / Referer / Origin / Accept / Cookie。
    /// `params` 为查询参数；`referer` 由调用方指定；`cookies` 已富化。
    ///
    /// 限流不在此处扣减：配额只在 `send_with_retry*` 真正发送时扣一次，
    /// 避免「构建 + 发送」双扣减把前台 5rps 实际压成 ~2.5rps。
    pub(in crate::services::bili_api) async fn build_get_request(
        &self,
        url: &str,
        params: &HashMap<String, String>,
        referer: &str,
        cookies: &str,
    ) -> RequestBuilder {
        let client = self.client_for(url);
        // WBI 签名基于百分号编码（空格→%20）计算，而 reqwest 的 query() 会把
        // 空格序列化为 `+`，导致含空格/中文关键词签名校验失败。
        // 这里与签名共用 wbi::build_query 手工拼接完整 URL，保证「签名串 == 发送串」。
        let mut req = client
            .get(append_query(url, params))
            .header("User-Agent", &self.config.user_agent)
            .header("Referer", referer)
            .header("Origin", "https://www.bilibili.com")
            .header("Accept", "application/json, text/plain, */*");
        // 不再按接口硬编码 5s/10s per-request 超时：
        // 超时统一走客户端构建时的 BILI_API_TIMEOUT 配置，保证配置生效。
        let credential = crate::services::credential::Credential::from_cookie_header(cookies);
        tracing::debug!(credential = ?credential, "B站请求凭证");
        let cookie_header = credential.to_cookie_header();
        if !cookie_header.is_empty() {
            req = req.header("Cookie", cookie_header);
        }
        req
    }

    /// 按所有 B 站接口共享的重试策略执行可克隆请求。
    /// 业务错误由后续解析函数处理，不在此处重试。
    pub(in crate::services::bili_api) async fn send_with_retry(
        &self,
        request: RequestBuilder,
    ) -> Result<reqwest::Response> {
        self.send_with_retry_limited(request, &self.rate_limiter)
            .await
    }

    /// 按调用类别选择限流桶：后台批量扫描（监控轮询、博主资料刷新）不得占用
    /// 交互请求配额，否则扫描密集期用户的搜索/刷新会在 5rps 队列后面排队到超时。
    /// 注意 WBI keys 仍走前台桶——它有 30 分钟缓存，摊薄后可忽略。
    pub(in crate::services::bili_api) async fn send_classed(
        &self,
        request: RequestBuilder,
        background: bool,
    ) -> Result<reqwest::Response> {
        if background {
            self.send_with_retry_limited(request, &self.background_rate_limiter)
                .await
        } else {
            self.send_with_retry(request).await
        }
    }

    /// 带自定义限流器的重试发送。
    ///
    /// 限流配额**只在真正发送处扣减一次**（构建请求不扣减），
    /// 后台批量探测传入 `background_rate_limiter`，不再额外占用前台交互配额。
    pub(in crate::services::bili_api) async fn send_with_retry_limited(
        &self,
        request: RequestBuilder,
        limiter: &governor::DefaultDirectRateLimiter,
    ) -> Result<reqwest::Response> {
        const NETWORK_BACKOFF_MS: [u64; 3] = [500, 1_000, 2_000];
        let mut network_retries = 0usize;
        let mut server_retries = 0usize;
        let mut rate_limit_retried = false;

        loop {
            // 每次重试尝试都重新排队限流配额：重试请求同样消耗 B 站 API 配额，
            // 避免退避后的突发重试绕过全局限流器再次触发 429。
            limiter.until_ready().await;
            let attempt = request
                .try_clone()
                .ok_or_else(|| anyhow!("B站 API 请求无法安全克隆，已拒绝重试"))?;
            match attempt.send().await {
                Ok(response) if response.status().as_u16() == 429 && !rate_limit_retried => {
                    rate_limit_retried = true;
                    // Retry-After 兼容秒数与 RFC 7231 HTTP-date 两种形式；
                    // 服务端明确指定的等待时间不做抖动，按原值等待。
                    let wait_seconds = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .map(parse_retry_after)
                        .unwrap_or(1);
                    warn!(wait_seconds, "B站 API 触发 429，按 Retry-After 重试一次");
                    tokio::time::sleep(std::time::Duration::from_secs(wait_seconds)).await;
                }
                Ok(response) if response.status().as_u16() == 429 => {
                    // 重试后仍 429：持续限流，放弃并通过既有风控事件通道提示用户，
                    // 而不是让业务静默失败。
                    warn!("B站 API 重试后仍触发 429，放弃重试并推送风控提示");
                    if self.system_notice_due("bili:risk-control").await {
                        if let Err(notify_error) = self
                            .ws
                            .broadcast_system(
                                "bili:risk-control",
                                serde_json::json!({
                                    "code": 429,
                                    "message": "触发 B 站限流，请稍后再试"
                                }),
                                crate::ws::NoticeAudience::CanAct,
                            )
                            .await
                        {
                            warn!("推送 B站限流提示事件失败: {notify_error}");
                        }
                    }
                    return Ok(response);
                }
                Ok(response) if response.status().is_server_error() && server_retries < 3 => {
                    let status = response.status();
                    server_retries += 1;
                    let delay_secs = 1u64 << (server_retries - 1); // 指数退避: 1s, 2s, 4s
                    warn!(
                        attempt = server_retries,
                        max_retries = 3,
                        %status,
                        "B站 API 服务器错误，{delay_secs}s 后重试"
                    );
                    // ±20% 抖动：避免多个并发请求退避后同步重发形成请求尖峰。
                    tokio::time::sleep(backoff_with_jitter(std::time::Duration::from_secs(
                        delay_secs,
                    )))
                    .await;
                }
                Ok(response) => return Ok(response),
                Err(error)
                    if (error.is_timeout() || error.is_connect())
                        && network_retries < NETWORK_BACKOFF_MS.len() =>
                {
                    let delay = backoff_with_jitter(std::time::Duration::from_millis(
                        NETWORK_BACKOFF_MS[network_retries],
                    ));
                    network_retries += 1;
                    warn!(
                        retry = network_retries,
                        delay_ms = delay.as_millis() as u64,
                        error = %error,
                        "B站 API 网络错误，准备重试"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error).context("发送 B站 API 请求失败"),
            }
        }
    }
}

/// 在 URL 后追加与 WBI 签名一致（百分号编码）的查询串。
fn append_query(url: &str, params: &HashMap<String, String>) -> String {
    if params.is_empty() {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!(
        "{url}{separator}{}",
        crate::services::wbi::build_query(params)
    )
}

/// 解析 Retry-After 头：兼容「秒数」与 RFC 7231 HTTP-date（如
/// `Fri, 22 Aug 2026 12:00:00 GMT`）两种形式，统一折算为距现在的秒数，
/// 钳制在 0..=60；无法解析时兜底 1 秒。
fn parse_retry_after(text: &str) -> u64 {
    let text = text.trim();
    if let Ok(seconds) = text.parse::<u64>() {
        return seconds.min(60);
    }
    if let Ok(datetime) = chrono::DateTime::parse_from_rfc2822(text) {
        let seconds = datetime
            .with_timezone(&chrono::Utc)
            .signed_duration_since(chrono::Utc::now())
            .num_seconds();
        return seconds.clamp(0, 60) as u64;
    }
    1
}

/// 给退避时长加 ±20% 抖动，避免并发请求在退避后同步重发形成请求尖峰。
fn backoff_with_jitter(base: std::time::Duration) -> std::time::Duration {
    use rand::Rng;
    let factor = rand::rng().random_range(-0.2..=0.2);
    let millis = (base.as_millis() as f64 * (1.0 + factor)).max(1.0);
    std::time::Duration::from_millis(millis as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_query_uses_percent_encoding_not_plus() {
        // B1 回归：含空格参数的 URL 查询串必须是 %20，不能是 `+`。
        let mut params = HashMap::new();
        params.insert("keyword".to_string(), "原神 启动".to_string());
        let url = append_query(
            "https://api.bilibili.com/x/web-interface/wbi/search/type",
            &params,
        );
        assert!(
            url.contains("keyword=%E5%8E%9F%E7%A5%9E%20%E5%90%AF%E5%8A%A8"),
            "url={url}"
        );
        assert!(!url.contains('+'), "url 不得包含 `+`: {url}");
        // 空参数时保持原 URL
        assert_eq!(
            append_query(
                "https://api.bilibili.com/x/web-interface/nav",
                &HashMap::new()
            ),
            "https://api.bilibili.com/x/web-interface/nav"
        );
        // 已有查询串时用 & 追加
        let mut extra = HashMap::new();
        extra.insert("a".to_string(), "b".to_string());
        assert_eq!(
            append_query("https://example.com/path?x=1", &extra),
            "https://example.com/path?x=1&a=b"
        );
    }

    #[test]
    fn retry_after_parses_seconds_http_date_and_fallbacks() {
        // 秒数形式
        assert_eq!(parse_retry_after("30"), 30);
        assert_eq!(parse_retry_after(" 30 "), 30);
        // 超上限钳制到 60
        assert_eq!(parse_retry_after("3600"), 60);
        // RFC 7231 HTTP-date 形式：折算为距现在的秒数
        let future = chrono::Utc::now() + chrono::Duration::seconds(30);
        let parsed = parse_retry_after(&future.to_rfc2822());
        assert!((25..=30).contains(&parsed), "parsed={parsed}");
        // 过去时间 → 0（立即重试）
        let past = chrono::Utc::now() - chrono::Duration::seconds(30);
        assert_eq!(parse_retry_after(&past.to_rfc2822()), 0);
        // 无法解析 → 兜底 1 秒
        assert_eq!(parse_retry_after("not-a-date"), 1);
    }

    #[test]
    fn backoff_jitter_stays_within_twenty_percent() {
        for base_ms in [500u64, 1_000, 2_000, 4_000] {
            for _ in 0..64 {
                let jittered = backoff_with_jitter(std::time::Duration::from_millis(base_ms));
                let ratio = jittered.as_millis() as f64 / base_ms as f64;
                assert!(
                    (0.8..=1.2).contains(&ratio),
                    "base={base_ms}ms jittered={jittered:?} ratio={ratio}"
                );
            }
        }
    }
}
