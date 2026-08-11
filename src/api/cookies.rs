use crate::error::{ApiResponse, AppError, BiliApiError, BiliErrorKind};
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
            "state": "unauthenticated",
            "business_code": null,
            "error_kind": null,
        }))));
    }
    match state.bili.bili_api.get_nav_info(&cookies).await {
        Ok(nav) => {
            let mut data = serde_json::to_value(&nav)?;
            if let Some(object) = data.as_object_mut() {
                object.insert("has_cookies".to_string(), Value::Bool(true));
                object.insert("valid".to_string(), Value::Bool(nav.is_login));
                object.insert(
                    "state".to_string(),
                    Value::String(
                        if nav.is_login {
                            "authenticated"
                        } else {
                            "unauthenticated"
                        }
                        .to_string(),
                    ),
                );
                object.insert("business_code".to_string(), Value::Number(0.into()));
                object.insert("error_kind".to_string(), Value::Null);
            }
            Ok(Json(ApiResponse::success(data)))
        }
        Err(error) => {
            let (state_name, business_code, error_kind) = classify_cookie_status_error(&error);
            warn!(%error, "获取登录信息失败");
            Ok(Json(ApiResponse::success(json!({
                "valid": false,
                "has_cookies": true,
                "state": state_name,
                "business_code": business_code,
                "error_kind": error_kind,
            }))))
        }
    }
}

fn classify_cookie_status_error(
    error: &anyhow::Error,
) -> (&'static str, Option<i64>, &'static str) {
    if let Some(error) = error.downcast_ref::<BiliApiError>() {
        let state = match error.kind {
            BiliErrorKind::Unauthorized => "unauthenticated",
            BiliErrorKind::RiskControl => "risk_control",
            BiliErrorKind::RateLimited | BiliErrorKind::Server => "unreachable",
            _ => "malformed",
        };
        return (state, Some(error.code), bili_error_kind_name(&error.kind));
    }
    if error.chain().any(|cause| cause.is::<reqwest::Error>())
        || error.to_string().contains("HTTP ")
    {
        return ("unreachable", None, "network");
    }
    ("malformed", None, "invalid_response")
}

fn bili_error_kind_name(kind: &BiliErrorKind) -> &'static str {
    match kind {
        BiliErrorKind::RiskControl => "risk_control",
        BiliErrorKind::Unauthorized => "unauthorized",
        BiliErrorKind::NotFound => "not_found",
        BiliErrorKind::BadRequest => "bad_request",
        BiliErrorKind::RateLimited => "rate_limited",
        BiliErrorKind::Server => "server",
        BiliErrorKind::InvalidResponse => "invalid_response",
        BiliErrorKind::ChargeRequired => "charge_required",
        BiliErrorKind::GeoRestricted => "geo_restricted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_status_classifies_bili_errors_without_expiration() {
        let risk = anyhow::Error::new(BiliApiError::classify(-352, "risk"));
        assert_eq!(classify_cookie_status_error(&risk).0, "risk_control");
        let auth = anyhow::Error::new(BiliApiError::classify(-101, "auth"));
        assert_eq!(classify_cookie_status_error(&auth).0, "unauthenticated");
        assert_eq!(
            classify_cookie_status_error(&anyhow::anyhow!("HTTP 503")).0,
            "unreachable"
        );
    }
}

async fn save_cookies(
    State(state): State<SharedState>,
    Json(request): Json<CookiesRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let cookies = request.cookies.trim();
    if !cookies.is_empty() {
        let nav = state.bili.bili_api.get_nav_info(cookies).await?;
        if !nav.is_login {
            return Err(AppError::Unauthorized(
                "Cookie 未通过 B 站登录校验".to_string(),
            ));
        }
    }
    state
        .infra
        .settings_service
        .save_cookie_header(cookies)
        .await?;
    state.bili.bili_api.invalidate_session_caches().await;
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
    let mut authenticated = false;
    let mut status = if matches!(poll.code, 86038 | 86039) {
        "expired"
    } else {
        "pending"
    };
    if poll.code == 0 {
        if let Some(cookies) = poll.cookies.as_deref().filter(|c| !c.trim().is_empty()) {
            if let Ok(nav) = state.bili.bili_api.get_nav_info(cookies).await {
                if nav.is_login {
                    state
                        .infra
                        .settings_service
                        .save_cookie_header(cookies)
                        .await?;
                    state.bili.bili_api.invalidate_session_caches().await;
                    authenticated = true;
                    status = "authenticated";
                    info!("扫码登录成功，凭证已保存");
                } else {
                    status = "partial";
                }
            } else {
                status = "partial";
            }
        } else {
            status = "partial";
        }
    }
    Ok(Json(ApiResponse::success(json!({
        "code": poll.code,
        "message": poll.message,
        "status": status,
        "authenticated": authenticated,
    }))))
}
