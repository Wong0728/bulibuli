//! BiliApi 请求管线：WBI 签名获取、风控参数注入、cookie 富化、
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
const MAX_API_ERROR_PREVIEW_BYTES: usize = 32 * 1024;

use super::models::{BiliEnvelope, RiskControlData};
use super::BiliApi;

impl BiliApi {
    /// 获取 WBI img_key/sub_key。
    ///
    /// `enriched_cookies` 必须是已经过 `enrich_cookies` 合并设备指纹后的 cookie 字符串。
    /// `/x/web-interface/nav` 在新风控策略下要求携带登录态 Cookie，否则返回 -101。
    pub(super) async fn get_wbi_keys(&self, enriched_cookies: &str) -> Result<(String, String)> {
        // WBI keys 来自 api.bilibili.com，必须走严格 TLS 的 api_client。
        const NAV_URL: &str = "https://api.bilibili.com/x/web-interface/nav";
        self.wbi_keys
            .get(
                self.client_for(NAV_URL),
                &self.config.user_agent,
                &self.config.referer,
                enriched_cookies,
            )
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

    /// 富化用户 cookie：合并设备指纹 cookie（buvid3/bili_ticket/...）。
    pub(super) async fn enrich_cookies(&self, user_cookies: &str) -> Result<String> {
        self.cookie_manager.enrich(user_cookies).await
    }

    /// 公开 cookie 富化入口，供 DownloadManager.add_to_aria2 复用。
    /// 下载流（m4s/m4a）需要带设备指纹 cookie 才能绕过 B站风控（403/-799 等）。
    pub async fn enrich_cookies_public(&self, user_cookies: &str) -> Result<String> {
        self.enrich_cookies(user_cookies).await
    }

    /// 统一构建 GET 请求：注入 User-Agent / Referer / Origin / Accept / Cookie。
    /// `params` 为查询参数；`referer` 由调用方指定；`cookies` 已富化。
    pub(super) async fn build_get_request(
        &self,
        url: &str,
        params: &HashMap<String, String>,
        referer: &str,
        cookies: &str,
    ) -> RequestBuilder {
        self.rate_limiter.until_ready().await;
        let client = self.client_for(url);
        let mut req = client
            .get(url)
            .query(params)
            .header("User-Agent", &self.config.user_agent)
            .header("Referer", referer)
            .header("Origin", "https://www.bilibili.com")
            .header("Accept", "application/json, text/plain, */*")
            .timeout(if url.contains("playurl") {
                std::time::Duration::from_secs(10)
            } else {
                std::time::Duration::from_secs(5)
            });
        let credential = crate::services::credential::Credential::from_cookie_header(cookies);
        debug!(credential = ?credential, "B站请求凭证");
        let cookie_header = credential.to_cookie_header();
        if !cookie_header.is_empty() {
            req = req.header("Cookie", cookie_header);
        }
        req
    }

    /// Execute a cloneable API request with the retry policy shared by every
    /// Bilibili endpoint. Business errors are intentionally handled later by
    /// `parse_json_response` and are never retried here.
    pub(super) async fn send_with_retry(
        &self,
        request: RequestBuilder,
    ) -> Result<reqwest::Response> {
        const NETWORK_BACKOFF_MS: [u64; 3] = [500, 1_000, 2_000];
        let mut network_retries = 0usize;
        let mut server_retries = 0usize;
        let mut rate_limit_retried = false;

        loop {
            let attempt = request
                .try_clone()
                .ok_or_else(|| anyhow!("B站 API 请求无法安全克隆，已拒绝重试"))?;
            match attempt.send().await {
                Ok(response) if response.status().as_u16() == 429 && !rate_limit_retried => {
                    rate_limit_retried = true;
                    let wait_seconds = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(1)
                        .min(60);
                    warn!(wait_seconds, "B站 API 触发 429，按 Retry-After 重试一次");
                    tokio::time::sleep(std::time::Duration::from_secs(wait_seconds)).await;
                }
                Ok(response) if response.status().is_server_error() && server_retries < 3 => {
                    let status = response.status();
                    let body_preview = read_limited_body(response, MAX_API_ERROR_PREVIEW_BYTES)
                        .await
                        .map(|bytes| {
                            String::from_utf8_lossy(&bytes)
                                .chars()
                                .take(200)
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    server_retries += 1;
                    let delay_secs = 1u64 << (server_retries - 1); // 指数退避: 1s, 2s, 4s
                    warn!(
                        attempt = server_retries,
                        max_retries = 3,
                        %status,
                        body_preview = %body_preview,
                        "B站 API 服务器错误，{delay_secs}s 后重试"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
                }
                Ok(response) => return Ok(response),
                Err(error)
                    if (error.is_timeout() || error.is_connect())
                        && network_retries < NETWORK_BACKOFF_MS.len() =>
                {
                    let delay = NETWORK_BACKOFF_MS[network_retries];
                    network_retries += 1;
                    warn!(
                        retry = network_retries,
                        delay_ms = delay,
                        error = %error,
                        "B站 API 网络错误，准备重试"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
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

    /// Parse a speculative/fallback API response without broadcasting global
    /// auth or risk-control events. The caller must retry its fallback and use
    /// the regular parser for the terminal attempt.
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
            if let Some(host) = host.as_deref() {
                self.bad_cdns.record_failure(host).await;
            }
            let preview = String::from_utf8_lossy(
                &read_limited_body(response, MAX_API_ERROR_PREVIEW_BYTES)
                    .await
                    .context("读取 B站 API 错误响应失败")?,
            )
            .chars()
            .take(500)
            .collect::<String>();
            if status.as_u16() == 412 {
                let error = BiliApiError::classify(-412, preview.clone());
                if notify {
                    self.notify_bili_error(&error, &serde_json::Value::Null)
                        .await;
                }
                return Err(error.into());
            }
            return Err(anyhow!("B站 API {api_name} 返回 HTTP {status}: {preview}"));
        }
        if !content_type.is_empty() && !content_type.contains("json") {
            let preview = String::from_utf8_lossy(
                &read_limited_body(response, MAX_API_ERROR_PREVIEW_BYTES)
                    .await
                    .context("读取 B站 API 非 JSON 响应失败")?,
            )
            .chars()
            .take(500)
            .collect::<String>();
            return Err(anyhow!(
                "B站 API {api_name} 返回非 JSON Content-Type {content_type}: {preview}"
            ));
        }
        if let Some(host) = host.as_deref() {
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

fn deserialize_payload<T: DeserializeOwned>(
    value: serde_json::Value,
    api_name: &str,
    field: &str,
) -> Result<T> {
    let shape = payload_shape(&value);
    serde_path_to_error::deserialize(value.into_deserializer()).map_err(|error| {
        anyhow!(
            "反序列化 B站 API {api_name} {field} 失败，字段路径={}，响应形状={shape}: {}",
            error.path(),
            error.inner()
        )
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
