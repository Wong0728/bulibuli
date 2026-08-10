use crate::error::{ApiResponse, AppError};
use crate::services::credential::Credential;
use crate::state::SharedState;
use axum::{
    extract::Query,
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/cookies/status", get(cookies_status))
        .route("/api/cookies/save", post(save_cookies))
        .route("/api/cookies/qrcode/generate", get(generate_qrcode))
        .route("/api/cookies/qrcode/poll", get(poll_qrcode))
}

#[derive(Deserialize)]
struct CookiesRequest {
    cookies: String,
}

async fn cookies_status(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let cookies = state.infra.settings_service.cookie_header().await?;
    if !Credential::from_cookie_header(&cookies).is_logged_in() {
        return Ok(Json(ApiResponse::success(json!({
            "valid": false,
            "has_cookies": false,
        }))));
    }
    match state.bili.bili_api.get_nav_info(&cookies).await {
        Ok(nav) => {
            let mut data = serde_json::to_value(&nav)?;
            if let Some(object) = data.as_object_mut() {
                object.insert("has_cookies".to_string(), Value::Bool(true));
                object.insert("valid".to_string(), Value::Bool(nav.is_login));
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(error) => {
            warn!(%error, "获取登录信息失败");
            Ok(Json(ApiResponse::success(json!({
                "valid": false,
                "has_cookies": true,
            }))))
        }
    }
}

async fn save_cookies(
    State(state): State<SharedState>,
    Json(request): Json<CookiesRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    state
        .infra
        .settings_service
        .save_cookie_header(request.cookies.trim())
        .await?;
    info!("B站凭证已保存");
    Ok(Json(ApiResponse::with_message(
        json!({ "configured": !request.cookies.trim().is_empty() }),
        "B站凭证已保存",
    )))
}

async fn generate_qrcode(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let result = state.bili.bili_api.get_qrcode_url().await?;
    Ok(Json(ApiResponse::success(serde_json::to_value(result)?)))
}

#[derive(Deserialize)]
struct QrPollQuery {
    qrcode_key: String,
}

async fn poll_qrcode(
    State(state): State<SharedState>,
    Query(query): Query<QrPollQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if query.qrcode_key.trim().is_empty() {
        return Err(AppError::BadRequest("缺少 qrcode_key".to_string()));
    }
    let poll = state
        .bili
        .bili_api
        .check_qrcode_status(&query.qrcode_key)
        .await?;
    if let Some(cookies) = poll.cookies.as_deref() {
        if !cookies.trim().is_empty() {
            state
                .infra
                .settings_service
                .save_cookie_header(cookies)
                .await?;
            info!("扫码登录成功，凭证已保存");
        }
    }
    Ok(Json(ApiResponse::success(json!({
        "code": poll.code,
        "message": poll.message,
    }))))
}
