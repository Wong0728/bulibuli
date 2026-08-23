use crate::error::ApiResponse;
use crate::state::SharedState;
use axum::{extract::State, routing::get, Json, Router};
use serde_json::json;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// FFmpeg 探测结果缓存有效期：探测要 spawn 子进程，避免 /api/health 被频繁
/// 轮询时反复拉起 FFmpeg。缓存键含当前配置（模式+路径），设置变更自动失效。
const FFMPEG_CACHE_TTL: Duration = Duration::from_secs(60);

/// (探测时间, ffmpeg.mode, ffmpeg.custom_path, 是否可用)
type FfmpegCacheEntry = (Instant, String, Option<String>, bool);

fn ffmpeg_cache() -> &'static Mutex<Option<FfmpegCacheEntry>> {
    static CACHE: OnceLock<Mutex<Option<FfmpegCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// /api/health 是免认证公开端点，除中间件层按 IP 限流（见 security_server 的
/// health_limiter）外不做会话判断，避免心跳探测被登录态卡住。
pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/health", get(health_check))
        .route("/api/ready", get(readiness_check))
}

async fn health_check(State(state): State<SharedState>) -> Json<ApiResponse<serde_json::Value>> {
    let aria2 = state.media.aria2.is_available().await;
    // 优先使用用户设置中的 FFmpeg 模式/自定义路径（与 ctl `sys ffmpeg-test`、
    // /api/settings/ffmpeg-path 同口径），不再硬编码 "auto" 探测——否则用户
    // 配好自定义 FFmpeg 后健康检查仍按 PATH 探测，误报 degraded。
    let settings = state.infra.settings_service.current();
    let mode = settings.ffmpeg.mode.clone();
    let custom_path = (!settings.ffmpeg.custom_path.trim().is_empty())
        .then(|| settings.ffmpeg.custom_path.clone());
    let cached = ffmpeg_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let ffmpeg = match cached.filter(|(at, cache_mode, cache_path, _)| {
        at.elapsed() < FFMPEG_CACHE_TTL && cache_mode == &mode && cache_path == &custom_path
    }) {
        Some((.., ok)) => ok,
        None => {
            let (ffmpeg_path, _) = state
                .media
                .video_processor
                .detect_ffmpeg(&mode, custom_path.as_deref())
                .await;
            let ok = match ffmpeg_path {
                Some(path) => state.media.video_processor.check_ffmpeg(&path).await.0,
                None => false,
            };
            *ffmpeg_cache().lock().unwrap() = Some((Instant::now(), mode, custom_path, ok));
            ok
        }
    };
    Json(ApiResponse::with_message(
        json!({
            "status": if aria2 && ffmpeg { "ok" } else { "degraded" },
            "aria2": aria2,
            "ffmpeg": ffmpeg,
        }),
        if aria2 && ffmpeg {
            "运行时依赖正常"
        } else {
            "运行时依赖不完整，请查看 aria2/FFmpeg 状态"
        },
    ))
}

async fn readiness_check(State(state): State<SharedState>) -> Json<ApiResponse<serde_json::Value>> {
    let db_healthy = state
        .infra
        .db
        .ping()
        .await
        .inspect_err(|e| tracing::warn!("ready check db ping failed: {e}"))
        .is_ok();
    let aria2_ready = state.media.aria2.is_available().await;
    Json(ApiResponse::success(json!({
        "status": if db_healthy && aria2_ready { "ok" } else { "degraded" },
        "db": db_healthy,
        "aria2": aria2_ready,
    })))
}
