//! 响应信封读取校验与强类型反序列化：HTTP 状态检查、Content-Type 检查、
//! `code==0` 业务码校验、响应体大小上限与带字段路径的解析错误。

use crate::error::{BiliApiError, BiliErrorKind};
use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde::de::IntoDeserializer;

use crate::services::bili_api::models::BiliEnvelope;
use crate::services::bili_api::BiliApi;

/// 单个 B 站 API JSON 响应体的字节上限：超过即视为异常响应（风控页/错误页）。
const MAX_API_JSON_BYTES: usize = 2 * 1024 * 1024;

impl BiliApi {
    /// 强类型解析 B 站 API 响应：检查 HTTP 状态 + 校验 `code==0` 后，
    /// 把 `data` 反序列化为调用方指定的类型 `T`。
    /// 非 0 走 `BiliApiError::classify`（保持风控/登录失效通知行为不变）。
    pub(in crate::services::bili_api) async fn parse_data<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        api_name: &str,
    ) -> Result<T> {
        let envelope = self.read_envelope(response, api_name).await?;
        deserialize_payload(envelope.data, api_name, "data")
    }

    /// 解析试探或回退接口的响应，但不广播全局登录和风控事件。
    /// 调用方必须继续尝试回退接口，并在最终尝试时使用普通解析器。
    pub(in crate::services::bili_api) async fn parse_data_silent<T: DeserializeOwned>(
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
    pub(in crate::services::bili_api) async fn parse_result<T: DeserializeOwned>(
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
    pub(in crate::services::bili_api) async fn read_envelope(
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
            // HTTP 200 + 非 JSON：B 站风控拦截页（HTML）。此前只抛裸 anyhow，既不分类
            // 也不推事件，前端毫无提示，表现就是"莫名其妙什么都拉不到"。
            self.wbi_keys.invalidate().await;
            let error = BiliApiError::classify(
                -412,
                format!("B站 API {api_name} 返回非 JSON Content-Type {content_type}"),
            );
            if notify {
                self.notify_bili_error(&error, &serde_json::Value::Null)
                    .await;
            }
            return Err(error.into());
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
    fn only_cdn_hosts_enter_bad_cdn_registry() {
        assert!(!is_cdn_host("api.bilibili.com"));
        assert!(!is_cdn_host("api.live.bilibili.com"));
        assert!(!is_cdn_host("passport.bilibili.com"));
        assert!(is_cdn_host("upos-sz-mirror.bilivideo.com"));
        assert!(is_cdn_host("i0.hdslb.com"));
        assert!(is_cdn_host("upos-hz-mirrorakam.akamaized.net"));
    }
}
