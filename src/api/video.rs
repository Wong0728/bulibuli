//! 视频 API：解析/取流/权限校验（`stream`）与弹幕/评论/封面等附件（`sidecar`）。

mod cover;
mod sidecar;
mod stream;

use crate::state::SharedState;
use axum::{
    routing::{get, post},
    Router,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/video/resolve", post(stream::resolve_video))
        .route("/api/video/get-videos", post(stream::get_videos))
        .route("/api/video/get-video-urls", post(stream::get_video_urls))
        .route("/api/video/get-audio-url", post(stream::get_audio_url))
        .route(
            "/api/video/download-danmaku",
            post(sidecar::download_danmaku),
        )
        .route(
            "/api/video/download-comments",
            post(sidecar::download_comments),
        )
        .route("/api/video/comments", get(sidecar::get_comments))
        .route("/api/video/danmaku", get(sidecar::get_danmaku))
        .route("/api/video/download-cover", post(cover::download_cover))
        .route("/api/video/proxy-image", get(cover::proxy_image))
        .route("/api/video/info", get(stream::get_video_info))
        .route("/api/video/gate-download", post(stream::gate_download))
}
