//! 代理下载：带 Referer/Cookie 转发 B 站资源字节流（含 SSRF 域名白名单）。

use crate::api::bili_resource::BiliResourceClient;
use crate::error::AppError;
use crate::state::SharedState;
use axum::{extract::Query, extract::State, response::Response};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub(super) struct ProxyQuery {
    url: String,
    filename: Option<String>,
}

pub(super) async fn download_proxy(
    State(state): State<SharedState>,
    Query(q): Query<ProxyQuery>,
) -> Result<Response, AppError> {
    use axum::body::Body;
    use axum::http::{header, StatusCode};

    // SSRF 防护：仅允许 B 站相关域名
    if crate::services::bili_url_policy::validate(&q.url)
        .await
        .is_err()
    {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "success": false,
                    "message": "不支持的下载域名，仅允许 bilibili.com / bilivideo.com"
                })
                .to_string(),
            ))
            .map_err(|e| AppError::Internal(format!("构建拒绝响应失败: {e}")));
    }

    let resp = BiliResourceClient::get(&state, &q.url, "*/*", true, None).await?;

    let content_type = resp
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let content_length = resp
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let stream = resp.bytes_stream();
    let body = Body::from_stream(stream);

    let filename = q.filename.unwrap_or_else(|| "download".to_string());
    let disposition = if filename.is_ascii() {
        format!("attachment; filename=\"{filename}\"")
    } else {
        format!(
            "attachment; filename*=UTF-8''{}",
            urlencoding::encode(&filename)
        )
    };

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "no-cache");
    if let Some(len) = content_length {
        builder = builder.header(header::CONTENT_LENGTH, len);
    }
    builder
        .body(body)
        .map_err(|e| AppError::Internal(format!("构建响应失败: {e}")))
}

/// SSRF 防护：仅允许 B 站相关域名的代理请求。
pub fn is_allowed_proxy_url(raw: &str) -> bool {
    crate::services::bili_url_policy::normalize_syntax(raw).is_ok()
}
