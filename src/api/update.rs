//! 应用内更新 API：状态查询（已认证可读）、检查更新与立即更新（owner-only）。
//!
//! 权限边界在 `security_server.rs::authorize_session` 的 owner_only 列表：
//! `/api/update/check` 与 `/api/update/apply` 会写设置缓存与程序文件，仅 Owner 可调。

use crate::error::{ApiResponse, AppError};
use crate::services::update;
use crate::state::SharedState;
use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::cmp::Ordering;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/update/status", get(status))
        .route("/api/update/check", post(check))
        .route("/api/update/apply", post(apply))
}

async fn status(State(state): State<SharedState>) -> Result<Json<ApiResponse<Value>>, AppError> {
    let settings = state.infra.settings_service.current();
    let current_version = env!("CARGO_PKG_VERSION");
    let latest = settings.update.latest_version.as_deref().unwrap_or("");
    let has_update = !latest.is_empty()
        && update::compare_versions(latest, current_version) == Ordering::Greater;
    Ok(Json(ApiResponse::success(json!({
        "current_version": current_version,
        "latest_version": settings.update.latest_version,
        "has_update": has_update,
        "policy": settings.update.policy,
        "last_checked_at": settings.update.last_checked_at,
    }))))
}

/// 立即检查：联网查询最新版本并缓存到设置（显式操作，不受 policy=off 限制）。
async fn check(State(state): State<SharedState>) -> Result<Json<ApiResponse<Value>>, AppError> {
    let (latest, assets) = update::fetch_latest(update::REPO)
        .await
        .map_err(|error| AppError::Upstream(format!("检查更新失败: {error}")))?;
    let current_version = env!("CARGO_PKG_VERSION");
    let has_update = update::compare_versions(&latest, current_version) == Ordering::Greater;
    let mut updated = (*state.infra.settings_service.current()).clone();
    updated.update.latest_version = Some(latest.clone());
    updated.update.last_checked_at = Some(chrono::Utc::now().timestamp());
    let saved = state
        .infra
        .settings_service
        .save(updated)
        .await
        .map_err(|error| AppError::Internal(format!("缓存检查结果失败: {error}")))?;
    Ok(Json(ApiResponse::success(json!({
        "current_version": current_version,
        "latest_version": latest,
        "has_update": has_update,
        "policy": saved.update.policy,
        "updatable": update::matching_asset(&assets).is_some(),
    }))))
}

/// 立即更新：下载当前平台匹配的 portable 包 → 校验 → 暂存 → 替换程序文件
/// （不触碰 data/）。Unix 直接替换；Windows 运行中写暂存标记，退出后自动完成。
async fn apply(State(state): State<SharedState>) -> Result<Json<ApiResponse<Value>>, AppError> {
    let (latest, assets) = update::fetch_latest(update::REPO)
        .await
        .map_err(|error| AppError::Upstream(format!("检查更新失败: {error}")))?;
    let current_version = env!("CARGO_PKG_VERSION");
    if update::compare_versions(&latest, current_version) != Ordering::Greater {
        return Ok(Json(ApiResponse::with_message(
            json!({"applied": false, "current_version": current_version}),
            "当前已是最新版本",
        )));
    }
    let asset = update::matching_asset(&assets)
        .ok_or_else(|| AppError::NotFound("当前平台没有可用的更新包".to_string()))?;
    let staged = update::download_and_stage(&state.infra.paths, asset, &latest)
        .await
        .map_err(|error| AppError::Internal(format!("下载更新包失败: {error}")))?;
    let outcome = update::apply_staged(&state.infra.paths, &staged)
        .map_err(|error| AppError::Internal(format!("替换程序文件失败: {error}")))?;
    let message = match outcome {
        update::ApplyOutcome::Applied => "更新完成，重启程序后生效",
        #[cfg(windows)]
        update::ApplyOutcome::Staged => {
            "程序正在运行，更新已暂存；退出程序后将自动完成替换，下次启动生效"
        }
    };
    Ok(Json(ApiResponse::with_message(
        json!({"applied": true, "version": latest}),
        message,
    )))
}
