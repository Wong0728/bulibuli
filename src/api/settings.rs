use crate::error::{ApiResponse, AppError};
use crate::services::file_safety::render_path_template;
use crate::services::settings::{RuntimeSettings, SECRET_MASK};
use crate::state::SharedState;
use axum::{
    extract::Query,
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

fn aria2_runtime_changed(before: &RuntimeSettings, after: &RuntimeSettings) -> bool {
    before.download_mode != after.download_mode
        || before.aria2_rpc != after.aria2_rpc
        || before.aria2c_basic != after.aria2c_basic
        || before.parallel_download.max_parallel != after.parallel_download.max_parallel
}

async fn reload_aria2_if_needed(
    state: &SharedState,
    before: &RuntimeSettings,
    after: &RuntimeSettings,
) {
    if !aria2_runtime_changed(before, after) {
        return;
    }
    if let Err(error) = state.media.aria2.stop().await {
        warn!("应用新设置前停止 Aria2 失败: {error}");
    }
    if let Err(error) = state.media.aria2.init(after).await {
        warn!("应用新设置后重新初始化 Aria2 失败: {error}");
    }
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/settings", get(get_settings).put(save_settings))
        .route("/api/settings/reset", post(reset_settings))
        .route("/api/settings/aria2-restart", post(restart_aria2))
        .route("/api/settings/ffmpeg-path", get(get_ffmpeg_path))
        .route("/api/settings/ffmpeg-test", post(test_ffmpeg))
        .route("/api/settings/path-preview", post(path_preview))
}

async fn restart_aria2(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let mut error_message = None;
    if let Err(error) = state.media.aria2.stop().await {
        error_message = Some(format!("停止现有 Aria2 失败: {error}"));
    }
    if error_message.is_none() {
        let settings = state.infra.settings_service.current();
        if let Err(error) = state.media.aria2.init(settings.as_ref()).await {
            error_message = Some(error.to_string());
        }
    }
    let diagnostics = state.media.aria2.diagnostics().await;
    let restarted = error_message.is_none() && diagnostics["state"] == "connected";
    Ok(Json(ApiResponse::with_message(
        json!({
            "restarted": restarted,
            "error": error_message,
            "aria2_diagnostics": diagnostics,
        }),
        if restarted {
            "Aria2 已重新连接"
        } else {
            "Aria2 重新连接失败"
        },
    )))
}

#[derive(Deserialize)]
struct PathPreviewRequest {
    template: Option<String>,
    title: Option<String>,
    uid: Option<String>,
    up: Option<String>,
    bvid: Option<String>,
    quality: Option<String>,
    codec: Option<String>,
    page: Option<String>,
    part: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
}

async fn path_preview(
    State(state): State<SharedState>,
    Json(request): Json<PathPreviewRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let settings = state.infra.settings_service.current();
    let template = request
        .template
        .as_deref()
        .unwrap_or(&settings.download_path.path_template);
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let variables = std::collections::HashMap::from([
        (
            "title",
            request.title.unwrap_or_else(|| "示例视频".to_string()),
        ),
        ("uid", request.uid.unwrap_or_else(|| "123456".to_string())),
        ("up", request.up.unwrap_or_else(|| "示例UP主".to_string())),
        (
            "bvid",
            request.bvid.unwrap_or_else(|| "BV1xx411c7mD".to_string()),
        ),
        (
            "quality",
            request.quality.unwrap_or_else(|| "1080p".to_string()),
        ),
        ("codec", request.codec.unwrap_or_else(|| "av1".to_string())),
        ("page", request.page.unwrap_or_else(|| "1".to_string())),
        (
            "part",
            request.part.unwrap_or_else(|| "示例分P".to_string()),
        ),
        (
            "type",
            request.task_type.unwrap_or_else(|| "video".to_string()),
        ),
        ("date", date),
    ]);
    let path = render_path_template(&state.infra.paths.download_dir, template, &variables)?;
    Ok(Json(ApiResponse::success(
        json!({ "path": path.to_string_lossy() }),
    )))
}

#[derive(Serialize)]
struct SettingsPayload {
    current: RuntimeSettings,
    defaults: RuntimeSettings,
    constraints: serde_json::Value,
    secret_configured: bool,
    aria2_secret_configured: bool,
}

fn settings_for_response(settings: &RuntimeSettings) -> RuntimeSettings {
    let mut response = settings.clone();
    response.aria2_rpc.secret = if settings.aria2_rpc.secret.is_empty() {
        String::new()
    } else {
        SECRET_MASK.to_string()
    };
    response
}

async fn get_settings(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<SettingsPayload>>, AppError> {
    Ok(Json(ApiResponse::success(SettingsPayload {
        current: settings_for_response(state.infra.settings_service.current().as_ref()),
        defaults: RuntimeSettings::default(),
        constraints: json!({
            "parallel_download.max_parallel": { "min": 1, "max": 32 },
            "parallel_download.wait_slot_timeout": { "min": 60, "max": 3600 },
            "query.manual_query_limit": { "min": 1, "max": 100 },
            "query.auto_query_limit": { "min": 1, "max": 100 },
        }),
        secret_configured: !state
            .infra
            .settings_service
            .current()
            .aria2_rpc
            .secret
            .is_empty(),
        aria2_secret_configured: !state
            .infra
            .settings_service
            .current()
            .aria2_rpc
            .secret
            .is_empty(),
    })))
}

async fn save_settings(
    State(state): State<SharedState>,
    Json(settings): Json<RuntimeSettings>,
) -> Result<Json<ApiResponse<RuntimeSettings>>, AppError> {
    let before = state.infra.settings_service.current();
    validate_sensitive_settings(&state, before.as_ref(), &settings)?;
    let saved = state.infra.settings_service.save(settings).await?;
    // DownloadManager 直接读取 SettingsService 快照（无独立缓存），无需失效通知；
    // MonitorService 有 TTL 缓存，需显式失效。
    state
        .business
        .monitor_service
        .invalidate_settings_cache()
        .await;
    reload_aria2_if_needed(&state, before.as_ref(), saved.as_ref()).await;
    Ok(Json(ApiResponse::with_message(
        settings_for_response(saved.as_ref()),
        "设置已保存",
    )))
}

fn validate_sensitive_settings(
    state: &SharedState,
    before: &RuntimeSettings,
    requested: &RuntimeSettings,
) -> Result<(), AppError> {
    if requested.download_mode.mode == "external" {
        let host = requested.aria2_rpc.host.trim();
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");
        if !loopback {
            let endpoint = format!("http://{host}:{}/jsonrpc", requested.aria2_rpc.port);
            if state
                .bili
                .security
                .current()
                .trusted_aria2_endpoint
                .as_deref()
                != Some(endpoint.as_str())
            {
                return Err(AppError::BadRequest(
                    "该外部 aria2 地址尚未由本机 trust aria2 批准".to_string(),
                ));
            }
        }
        let endpoint_changed = before.aria2_rpc.host != requested.aria2_rpc.host
            || before.aria2_rpc.port != requested.aria2_rpc.port;
        if endpoint_changed && requested.aria2_rpc.secret == SECRET_MASK {
            return Err(AppError::BadRequest(
                "更换 aria2 地址时必须重新输入 RPC 密钥".to_string(),
            ));
        }
    }
    if requested.ffmpeg.mode == "custom" {
        let requested_path = std::path::PathBuf::from(&requested.ffmpeg.custom_path);
        if !state
            .bili
            .security
            .current()
            .trusted_ffmpeg_paths
            .contains(&requested_path)
        {
            return Err(AppError::BadRequest(
                "该 FFmpeg 路径尚未由本机 trust ffmpeg 批准".to_string(),
            ));
        }
    }
    Ok(())
}

async fn reset_settings(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<RuntimeSettings>>, AppError> {
    let before = state.infra.settings_service.current();
    let settings = state.infra.settings_service.reset().await?;
    // 仅 MonitorService 有 TTL 缓存需失效（见 save_settings 说明）。
    state
        .business
        .monitor_service
        .invalidate_settings_cache()
        .await;
    reload_aria2_if_needed(&state, before.as_ref(), settings.as_ref()).await;
    Ok(Json(ApiResponse::with_message(
        settings_for_response(settings.as_ref()),
        "已恢复默认设置",
    )))
}

#[derive(Deserialize)]
struct FfmpegQuery {
    mode: Option<String>,
}

#[derive(Serialize)]
struct FfmpegStatus {
    available: bool,
    path: Option<String>,
    source: String,
    version: Option<String>,
}

async fn detect_ffmpeg_status(
    state: &SharedState,
    mode: &str,
    custom_path: Option<&str>,
) -> FfmpegStatus {
    let (path, source) = state
        .media
        .video_processor
        .detect_ffmpeg(mode, custom_path)
        .await;
    let (available, version) = if let Some(path) = path.as_deref() {
        state.media.video_processor.check_ffmpeg(path).await
    } else {
        (false, None)
    };
    FfmpegStatus {
        available,
        path: path.map(|path| path.to_string_lossy().to_string()),
        source,
        version,
    }
}

async fn get_ffmpeg_path(
    State(state): State<SharedState>,
    Query(query): Query<FfmpegQuery>,
) -> Result<Json<ApiResponse<FfmpegStatus>>, AppError> {
    let settings = state.infra.settings_service.current();
    let mode = query.mode.as_deref().unwrap_or(&settings.ffmpeg.mode);
    let custom_path = (!settings.ffmpeg.custom_path.trim().is_empty())
        .then_some(settings.ffmpeg.custom_path.as_str());
    Ok(Json(ApiResponse::success(
        detect_ffmpeg_status(&state, mode, custom_path).await,
    )))
}

#[derive(Deserialize)]
struct FfmpegTestRequest {
    mode: Option<String>,
    custom_path: Option<String>,
}

async fn test_ffmpeg(
    State(state): State<SharedState>,
    Json(request): Json<FfmpegTestRequest>,
) -> Result<Json<ApiResponse<FfmpegStatus>>, AppError> {
    let mode = request.mode.as_deref().unwrap_or("auto");
    if mode == "custom" {
        let path = request
            .custom_path
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("缺少 FFmpeg 路径".to_string()))?;
        if !state
            .bili
            .security
            .current()
            .trusted_ffmpeg_paths
            .contains(&std::path::PathBuf::from(path))
        {
            return Err(AppError::BadRequest(
                "该 FFmpeg 路径尚未由本机 trust ffmpeg 批准".to_string(),
            ));
        }
    }
    Ok(Json(ApiResponse::success(
        detect_ffmpeg_status(&state, mode, request.custom_path.as_deref()).await,
    )))
}
