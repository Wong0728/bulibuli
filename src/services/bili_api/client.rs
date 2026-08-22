//! BiliApi 请求管线：WBI 签名获取、风控参数注入、Cookie 富化、
//! 统一 GET 构建、网络重试与 JSON 响应解析。

use crate::error::{BiliApiError, BiliErrorKind};
use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use reqwest::RequestBuilder;
use serde::de::DeserializeOwned;
use serde::de::IntoDeserializer;
use serde_json::json;
use std::collections::HashMap;
use tracing::{debug, warn};

const MAX_API_JSON_BYTES: usize = 2 * 1024 * 1024;

use super::models::{BiliEnvelope, RiskControlData};
use super::BiliApi;

impl BiliApi {
    /// 获取 WBI img_key/sub_key。
    ///
    /// `enriched_cookies` 必须是已经过 `enrich_cookies` 合并设备指纹后的 Cookie 字符串。
    /// `/x/web-interface/nav` 在新风控策略下要求携带登录态 Cookie，否则返回 -101。
    pub(super) async fn get_wbi_keys(&self, enriched_cookies: &str) -> Result<(String, String)> {
        // WBI keys 来自 api.bilibili.com，必须走严格 TLS 的 api_client。
        const NAV_URL: &str = "https://api.bilibili.com/x/web-interface/nav";
        self.wbi_keys
            .get(|| async {
                // nav 请求复用统一请求管线（限流在发送处统一扣减、网络重试、
                // 超时与请求头均与其他 B站 API 一致），不再绕过重试直连。
                let params = HashMap::new();
                let request = self
                    .build_get_request(NAV_URL, &params, &self.config.referer, enriched_cookies)
                    .await;
                self.send_with_retry(request).await
            })
            .await
    }

    /// 公开 WBI keys 获取方法，供 DanmakuService 等复用。
    ///
    /// 调用方需先 `cookie_manager.enrich(...)` 合并设备指纹，再把结果传入。
    pub async fn get_wbi_keys_public(&self, enriched_cookies: &str) -> Result<(String, String)> {
        self.get_wbi_keys(enriched_cookies).await
    }

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

    /// 注入 B 站风控必需参数（与 Bili23-Downloader 一致）。
    /// 这些固定值是 B 站前端在 WebGL/Canvas 等环境采集后编码的"指纹"，几乎所有 wbi 接口都需要。
    /// `web_location` 由调用方在调用前设置（若已存在则不覆盖）。
    pub(super) fn inject_risk_params(
        &self,
        params: &mut HashMap<String, String>,
        web_location: &str,
    ) {
        // dm_img_str: WebGL 版本信息（base64）
        params
            .entry("dm_img_str".to_string())
            .or_insert_with(|| "V2ViR0wgMS4wIChPcGVuR0wgRVMgMi4wIENocm9taXVtKQ".to_string());
        // dm_cover_img_str: ANGLE 后端信息（base64）
        params.entry("dm_cover_img_str".to_string()).or_insert_with(|| {
            "QU5HTEUgKE5WSURJQSwgTlZJRElBIEdlRm9yY2UgUlRYIDQwNjAgTGFwdG9wIEdQVSAoMHgwMDAwMjhFMCkgRGlyZWN0M0QxMSB2c181XzAgcHNfNV8wLCBEM0QxMSlHb29nbGUgSW5jLiAoTlZJRElBKQ".to_string()
        });
        // dm_img_list: 空数组
        params
            .entry("dm_img_list".to_string())
            .or_insert_with(|| "[]".to_string());
        // dm_img_inter: 设备交互指纹（ds/wh/of 字段）
        params
            .entry("dm_img_inter".to_string())
            .or_insert_with(|| r#"{"ds":[],"wh":[5073,6031,29],"of":[206,412,206]}"#.to_string());
        // gaia_source: 来源标识
        params
            .entry("gaia_source".to_string())
            .or_insert_with(|| "web_main".to_string());
        // web_location: 页面位置（调用方指定）
        if !web_location.is_empty() {
            params
                .entry("web_location".to_string())
                .or_insert_with(|| web_location.to_string());
        }
    }

    /// 富化用户 Cookie：合并设备指纹 Cookie（buvid3/bili_ticket/...）。
    pub(super) async fn enrich_cookies(&self, user_cookies: &str) -> Result<String> {
        self.cookie_manager.enrich(user_cookies).await
    }

    /// 公开 Cookie 富化入口，供 DownloadManager.add_to_aria2 复用。
    /// 下载流（m4s/m4a）需要带设备指纹 Cookie 才能绕过 B 站风控（403/-799 等）。
    pub async fn enrich_cookies_public(&self, user_cookies: &str) -> Result<String> {
        self.enrich_cookies(user_cookies).await
    }

    /// 统一构建 GET 请求：注入 User-Agent / Referer / Origin / Accept / Cookie。
    /// `params` 为查询参数；`referer` 由调用方指定；`cookies` 已富化。
    ///
    /// 限流不在此处扣减：配额只在 `send_with_retry*` 真正发送时扣一次，
    /// 避免「构建 + 发送」双扣减把前台 5rps 实际压成 ~2.5rps。
    pub(super) async fn build_get_request(
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
        debug!(credential = ?credential, "B站请求凭证");
        let cookie_header = credential.to_cookie_header();
        if !cookie_header.is_empty() {
            req = req.header("Cookie", cookie_header);
        }
        req
    }

    /// 按所有 B 站接口共享的重试策略执行可克隆请求。
    /// 业务错误由后续解析函数处理，不在此处重试。
    pub(super) async fn send_with_retry(
        &self,
        request: RequestBuilder,
    ) -> Result<reqwest::Response> {
        self.send_with_retry_limited(request, &self.rate_limiter)
            .await
    }

    /// 带自定义限流器的重试发送。
    ///
    /// 限流配额**只在真正发送处扣减一次**（构建请求不扣减），
    /// 后台批量探测传入 `background_rate_limiter`，不再额外占用前台交互配额。
    pub(super) async fn send_with_retry_limited(
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
                    if let Err(notify_error) = self
                        .ws
                        .broadcast_system(
                            "bili:risk-control",
                            json!({
                                "code": 429,
                                "message": "触发 B 站限流，请稍后再试"
                            }),
                        )
                        .await
                    {
                        warn!("推送 B站限流提示事件失败: {notify_error}");
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

    /// 强类型解析 B 站 API 响应：检查 HTTP 状态 + 校验 `code==0` 后，
    /// 把 `data` 反序列化为调用方指定的类型 `T`。
    /// 非 0 走 `BiliApiError::classify`（保持风控/登录失效通知行为不变）。
    pub(super) async fn parse_data<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        api_name: &str,
    ) -> Result<T> {
        let envelope = self.read_envelope(response, api_name).await?;
        deserialize_payload(envelope.data, api_name, "data")
    }

    /// 解析试探或回退接口的响应，但不广播全局登录和风控事件。
    /// 调用方必须继续尝试回退接口，并在最终尝试时使用普通解析器。
    pub(super) async fn parse_data_silent<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        api_name: &str,
    ) -> Result<T> {
        let envelope = self
            .read_envelope_with_notification(response, api_name, false)
            .await?;
        deserialize_payload(envelope.data, api_name, "data")
    }

    /// 番剧（PGC）响应解析入口：与 `parse_data` 一致，但读取 `result` 字段而非 `data`。
    /// 番剧接口（`/pgc/view/web/season`、`/pgc/player/web/playurl`）的响应主体在 `result`。
    pub(super) async fn parse_result<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        api_name: &str,
    ) -> Result<T> {
        let envelope = self.read_envelope(response, api_name).await?;
        deserialize_payload(envelope.result, api_name, "result")
    }

    /// 读取并校验 B 站响应信封：HTTP 状态 + Content-Type + `code==0`。
    /// `code!=0` 时按 classify 分类并推送前端系统事件，然后返回 Err；
    /// 返回的 `BiliEnvelope` 已保证 `code==0`，`data` 待调用方按需反序列化。
    pub(super) async fn read_envelope(
        &self,
        response: reqwest::Response,
        api_name: &str,
    ) -> Result<BiliEnvelope> {
        self.read_envelope_with_notification(response, api_name, true)
            .await
    }

    async fn read_envelope_with_notification(
        &self,
        response: reqwest::Response,
        api_name: &str,
        notify: bool,
    ) -> Result<BiliEnvelope> {
        let status = response.status();
        let host = response.url().host_str().map(str::to_owned);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !status.is_success() {
            // 只有 CDN 域名才进入坏节点熔断表：api.bilibili.com 等主域的业务
            // 失败（401/403/风控等）与 CDN 节点质量无关，误记会污染熔断表。
            if let Some(host) = host.as_deref().filter(|host| is_cdn_host(host)) {
                self.bad_cdns.record_failure(host).await;
            }
            let code = match status.as_u16() {
                401 => -101,
                403 | 412 => -403,
                404 => -404,
                429 => 429,
                value if value >= 500 => value as i64,
                _ => -400,
            };
            let error = BiliApiError::classify(code, format!("HTTP {status}"));
            if notify {
                self.notify_bili_error(&error, &serde_json::Value::Null)
                    .await;
            }
            return Err(error.into());
        }
        if !content_type.is_empty() && !content_type.contains("json") {
            return Err(anyhow!(
                "B站 API {api_name} 返回非 JSON Content-Type {content_type}"
            ));
        }
        if let Some(host) = host.as_deref().filter(|host| is_cdn_host(host)) {
            self.bad_cdns.record_success(host).await;
        }
        let bytes = read_limited_body(response, MAX_API_JSON_BYTES)
            .await
            .context("读取 B站 API 响应失败")?;
        let envelope: BiliEnvelope = serde_json::from_slice(&bytes)
            .with_context(|| format!("解析 B站 API {api_name} JSON 失败"))?;
        // 统一入口：缺 code 字段视为失败（响应异常）；code==0 才是成功，非 0 一律走 classify。
        let code = envelope
            .code
            .ok_or_else(|| anyhow!("B站 API {api_name} 响应缺少 code 字段"))?;
        if code == 0 {
            return Ok(envelope);
        }
        let message = envelope
            .message
            .as_deref()
            .unwrap_or("B站 API 返回业务错误");
        let error = BiliApiError::classify(code, message);
        if matches!(error.kind, BiliErrorKind::RiskControl) && matches!(code, -352 | -412 | -799) {
            self.wbi_keys.invalidate().await;
        }
        if notify {
            self.notify_bili_error(&error, &envelope.data).await;
        }
        Err(error.into())
    }

    pub(super) async fn notify_bili_error(&self, error: &BiliApiError, data: &serde_json::Value) {
        let event = match error.kind {
            BiliErrorKind::RiskControl => Some("bili:risk-control"),
            BiliErrorKind::Unauthorized => Some("bili:auth-expired"),
            _ => None,
        };
        let Some(event) = event else {
            return;
        };
        let mut payload = json!({ "code": error.code, "message": error.message });
        if matches!(error.kind, BiliErrorKind::RiskControl) {
            if let Ok(risk) = serde_json::from_value::<RiskControlData>(data.clone()) {
                if let Some(v_voucher) = risk.v_voucher.filter(|value| !value.is_empty()) {
                    payload["v_voucher"] = json!(v_voucher);
                }
            }
        }
        if let Err(notify_error) = self.ws.broadcast_system(event, payload).await {
            warn!("推送 B站系统事件失败: {notify_error}");
        }
    }
}

async fn read_limited_body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("读取响应分块失败")?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(anyhow!("响应体超过 {} 字节上限", limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
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

/// 判断 host 是否为 CDN 域名（非 bilibili.com 主域）。
/// 仅 CDN 域名的传输层成败参与坏节点熔断统计。
fn is_cdn_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    !(host == "bilibili.com" || host.ends_with(".bilibili.com"))
}

fn deserialize_payload<T: DeserializeOwned>(
    value: serde_json::Value,
    api_name: &str,
    field: &str,
) -> Result<T> {
    let shape = payload_shape(&value);
    serde_path_to_error::deserialize(value.into_deserializer()).map_err(|error| {
        anyhow!(crate::error::BiliDeserializeError(format!(
            "反序列化 B站 API {api_name} {field} 失败，字段路径={}，响应形状={shape}: {}",
            error.path(),
            error.inner()
        )))
    })
}

fn payload_shape(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(object) => {
            let mut keys = object.keys().take(30).cloned().collect::<Vec<_>>();
            keys.sort();
            format!("object(keys=[{}])", keys.join(","))
        }
        serde_json::Value::Array(items) => format!("array(len={})", items.len()),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(_) => "bool".to_string(),
        serde_json::Value::Number(_) => "number".to_string(),
        serde_json::Value::String(_) => "string".to_string(),
    }
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

    #[test]
    fn only_cdn_hosts_enter_bad_cdn_registry() {
        assert!(!is_cdn_host("api.bilibili.com"));
        assert!(!is_cdn_host("api.live.bilibili.com"));
        assert!(!is_cdn_host("passport.bilibili.com"));
        assert!(is_cdn_host("upos-sz-mirror.bilivideo.com"));
        assert!(is_cdn_host("i0.hdslb.com"));
        assert!(is_cdn_host("upos-hz-mirrorakam.akamaized.net"));
    }
}
