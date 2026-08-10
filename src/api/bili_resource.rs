//! B 站静态资源请求入口：统一域名校验、请求头、登录态、上游状态与大小限制。

use crate::error::AppError;
use crate::state::SharedState;
use reqwest::header::{HeaderMap, HeaderValue, LOCATION};

pub(super) struct BiliResourceClient;

impl BiliResourceClient {
    pub(super) async fn get(
        state: &SharedState,
        url: &str,
        accept: &'static str,
        authenticated: bool,
        max_content_length: Option<u64>,
    ) -> Result<reqwest::Response, AppError> {
        let initial = crate::services::bili_url_policy::validate(url).await?;

        let mut headers = HeaderMap::new();
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(&state.infra.config.user_agent)?,
        );
        headers.insert(
            "Referer",
            HeaderValue::from_str(&state.infra.config.referer)?,
        );
        headers.insert(
            "Origin",
            HeaderValue::from_static("https://www.bilibili.com"),
        );
        headers.insert("Accept", HeaderValue::from_static(accept));

        if authenticated {
            let stored = state.infra.settings_service.cookie_header().await?;
            if !stored.trim().is_empty() {
                let enriched = state.bili.bili_api.enrich_cookies_public(&stored).await?;
                headers.insert("Cookie", HeaderValue::from_str(&enriched)?);
            }
        }

        // 复用 AppState 中的共享客户端（连接池/TLS 配置只建一次，带超时），
        // 重定向由下方循环手动跟随以便逐跳复验 URL 白名单。
        let client = &state.bili.resource_client;
        let mut current = initial.to_string();
        let mut final_response = None;
        for redirect_count in 0..=5 {
            current = crate::services::bili_url_policy::validate(&current)
                .await?
                .to_string();
            let response = client.get(&current).headers(headers.clone()).send().await?;
            if !response.status().is_redirection() {
                final_response = Some(response);
                break;
            }
            if redirect_count == 5 {
                return Err(AppError::BadRequest("B 站资源重定向超过 5 次".to_string()));
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| AppError::BadRequest("B 站资源重定向缺少 Location".to_string()))?;
            current = response
                .url()
                .join(location)
                .map_err(|_| AppError::BadRequest("B 站资源重定向 URL 无效".to_string()))?
                .to_string();
        }
        let response =
            final_response.ok_or_else(|| AppError::BadRequest("B 站资源重定向失败".to_string()))?;
        if !response.status().is_success() {
            return Err(AppError::ExternalProcess(format!(
                "B 站资源上游返回 HTTP {}",
                response.status()
            )));
        }
        if max_content_length.is_some_and(|limit| {
            response
                .content_length()
                .is_some_and(|length| length > limit)
        }) {
            return Err(AppError::BadRequest("B 站资源大小超过限制".to_string()));
        }
        Ok(response)
    }
}
