use crate::state::SharedState;
use axum::{extract::State, routing::get, Json, Router};
use serde_json::json;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/health", get(health_check))
        .route("/api/ready", get(readiness_check))
}

async fn health_check(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let aria2 = state.media.aria2.is_available().await;
    let (ffmpeg_path, _) = state
        .media
        .video_processor
        .detect_ffmpeg("auto", None)
        .await;
    let ffmpeg = match ffmpeg_path {
        Some(path) => state.media.video_processor.check_ffmpeg(&path).await.0,
        None => false,
    };
    Json(json!({
        "status": if aria2 && ffmpeg { "ok" } else { "degraded" },
        "aria2": aria2,
        "ffmpeg": ffmpeg,
        "message": if aria2 && ffmpeg {
            "运行时依赖正常"
        } else {
            "运行时依赖不完整，请查看 aria2/FFmpeg 状态"
        },
    }))
}

async fn readiness_check(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let db_healthy = state
        .infra
        .db
        .ping()
        .await
        .inspect_err(|e| tracing::warn!("ready check db ping failed: {e}"))
        .is_ok();
    let aria2_ready = state.media.aria2.is_available().await;
    Json(json!({
        "status": if db_healthy && aria2_ready { "ok" } else { "degraded" },
        "db": db_healthy,
        "aria2": aria2_ready,
    }))
}
