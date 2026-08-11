use crate::api::bili_resource::BiliResourceClient;
use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::extract::State;
use axum::Json;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

fn limit_image_stream<S>(
    stream: S,
    max_bytes: u64,
) -> impl futures::Stream<Item = Result<axum::body::Bytes, AppError>>
where
    S: futures::Stream<Item = Result<axum::body::Bytes, reqwest::Error>>,
{
    stream.scan((0_u64, false), move |(received, finished), item| {
        if *finished {
            return futures::future::ready(None);
        }
        let result = match item {
            Ok(chunk) => {
                *received = received.saturating_add(chunk.len() as u64);
                if *received > max_bytes {
                    *finished = true;
                    Err(AppError::BadRequest("图片大小超过 20 MiB 限制".to_string()))
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

fn image_content_type(headers: &reqwest::header::HeaderMap) -> Result<String, AppError> {
    let content_type = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or(value)
                .trim()
                .to_ascii_lowercase()
        })
        .ok_or_else(|| AppError::BadRequest("B 站图片响应缺少 Content-Type".to_string()))?;
    if !content_type.starts_with("image/") {
        return Err(AppError::BadRequest(format!(
            "B 站资源不是图片: {content_type}"
        )));
    }
    Ok(content_type)
}

#[derive(Deserialize)]
pub(super) struct ProxyImageQuery {
    url: String,
}

pub(super) async fn proxy_image(
    State(state): State<SharedState>,
    axum::extract::Query(query): axum::extract::Query<ProxyImageQuery>,
) -> Result<axum::response::Response, AppError> {
    use axum::body::Body;
    use axum::http::{header, StatusCode};

    if !crate::api::download::is_allowed_proxy_url(&query.url) {
        return Err(AppError::BadRequest("不支持的图片域名".to_string()));
    }
    let response = BiliResourceClient::get(
        &state,
        &query.url,
        "image/*,*/*;q=0.8",
        false,
        Some(MAX_IMAGE_BYTES),
    )
    .await?;
    let content_type = image_content_type(response.headers())?;
    let limited_stream = limit_image_stream(response.bytes_stream(), MAX_IMAGE_BYTES);
    let body = Body::from_stream(limited_stream);
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .body(body)
        .map_err(|error| AppError::Internal(format!("构建响应失败: {error}")))
}

#[derive(Deserialize)]
pub(super) struct DownloadCoverRequest {
    bvid: String,
    uid: Option<String>,
}

pub(super) async fn download_cover(
    State(state): State<SharedState>,
    Json(request): Json<DownloadCoverRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = request.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }
    let cookies = state.infra.settings_service.cookie_header().await?;
    let info = state.bili.bili_api.get_video_info(bvid, &cookies).await?;
    if info.pic.is_empty() {
        return Err(AppError::NotFound("未找到封面URL".to_string()));
    }
    let response = BiliResourceClient::get(
        &state,
        &info.pic,
        "image/*,*/*;q=0.8",
        false,
        Some(MAX_IMAGE_BYTES),
    )
    .await?;
    let content_type = image_content_type(response.headers())?;
    let extension = match content_type.as_str() {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "jpg",
    };
    let download_dir = state
        .media
        .download_manager
        .download_dir(request.uid.as_deref())
        .await;
    tokio::fs::create_dir_all(&download_dir).await?;
    let filename = format!("{bvid}_cover.{extension}");
    let filepath = download_dir.join(&filename);
    let mut file = tokio::fs::File::create(&filepath).await?;
    let mut stream = response.bytes_stream();
    let mut written = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        written += chunk.len() as u64;
        if written > MAX_IMAGE_BYTES {
            drop(file);
            if let Err(error) = tokio::fs::remove_file(&filepath).await {
                tracing::warn!(%error, path = %filepath.display(), "清理超限封面失败");
            }
            return Err(AppError::BadRequest("封面大小超过 20 MiB 限制".to_string()));
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(Json(ApiResponse::with_message(
        json!({
            "filename": filename,
            "size": written,
        }),
        "封面下载成功",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

    #[test]
    fn validates_image_content_types() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("image/webp; charset=binary"),
        );
        assert_eq!(image_content_type(&headers).expect("image"), "image/webp");
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
        assert!(image_content_type(&headers).is_err());
    }

    #[tokio::test]
    async fn chunked_image_stream_stops_at_limit() {
        let stream = futures::stream::iter(vec![
            Ok::<_, reqwest::Error>(axum::body::Bytes::from_static(b"123")),
            Ok::<_, reqwest::Error>(axum::body::Bytes::from_static(b"456")),
            Ok::<_, reqwest::Error>(axum::body::Bytes::from_static(b"ignored")),
        ]);
        let chunks = limit_image_stream(stream, 5).collect::<Vec<_>>().await;
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].is_ok());
        assert!(chunks[1].is_err());
    }
}
