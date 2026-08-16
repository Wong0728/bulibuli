//! 代理下载：带 Referer/Cookie 转发 B 站资源字节流（含 SSRF 域名白名单）。

use crate::api::bili_resource::BiliResourceClient;
use crate::error::AppError;
use crate::state::SharedState;
use axum::{extract::Query, extract::State, response::Response};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;

const MAX_PROXY_BYTES: u64 = 4 * 1024 * 1024 * 1024;

fn limit_proxy_stream<S>(stream: S) -> impl Stream<Item = Result<axum::body::Bytes, AppError>>
where
    S: Stream<Item = Result<axum::body::Bytes, reqwest::Error>>,
{
    stream.scan((0_u64, false), |(received, finished), item| {
        if *finished {
            return futures::future::ready(None);
        }
        let result = match item {
            Ok(chunk) => {
                *received = received.saturating_add(chunk.len() as u64);
                if *received > MAX_PROXY_BYTES {
                    *finished = true;
                    Err(AppError::BadRequest("代理资源超过 4 GiB 限制".to_string()))
                } else {
                    Ok(chunk)
                }
            }
            Err(error) => {
                *finished = true;
                Err(AppError::Network(error))
            }
        };
        futures::future::ready(Some(result))
    })
}

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

    // SSRF 防护：BiliResourceClient::get 内部执行完整校验（域名白名单 + DNS 私网
    // 检查 + 逐跳重定向复验）。此前此处又 validate 一次，同一 URL 做两次 DNS 解析，
    // 浪费且扩大 TOCTOU 窗口——移除重复调用，仅保留语法预检以给出更友好的拒绝文案。
    if !crate::api::download::is_allowed_proxy_url(&q.url) {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({
                    "success": false,
                    "message": "不支持的下载域名，仅允许 bilibili.com / bilivideo.com / hdslb.com"
                })
                .to_string(),
            ))
            .map_err(|e| AppError::Internal(format!("构建拒绝响应失败: {e}")));
    }

    let resp = BiliResourceClient::get(&state, &q.url, "*/*", true, MAX_PROXY_BYTES).await?;

    let content_type = resp
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let stream = limit_proxy_stream(resp.bytes_stream());
    let body = Body::from_stream(stream);

    let filename = q.filename.unwrap_or_else(|| "download".to_string());
    let disposition = if filename.is_ascii() && !filename.contains('"') && !filename.contains('\\')
    {
        format!("attachment; filename=\"{filename}\"")
    } else {
        format!(
            "attachment; filename*=UTF-8''{}",
            urlencoding::encode(&filename)
        )
    };

    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "no-cache");
    builder
        .body(body)
        .map_err(|e| AppError::Internal(format!("构建响应失败: {e}")))
}

/// SSRF 防护：仅允许 B 站相关域名的代理请求。
pub fn is_allowed_proxy_url(raw: &str) -> bool {
    crate::services::bili_url_policy::normalize_syntax(raw).is_ok()
}
