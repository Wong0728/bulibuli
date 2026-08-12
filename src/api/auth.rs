use crate::error::{ApiResponse, AppError};
use crate::services::auth::{ClientInfo, SessionAuth};
use crate::services::security_config::AccessMode;
use crate::state::bili::BiliState;
use crate::state::SharedState;
use axum::{
    extract::{Extension, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/auth/state", get(auth_state))
        .route("/api/auth/pair", post(pair))
        .route("/api/auth/logout", post(logout))
        .route(
            "/api/auth/invitations/operator",
            post(create_operator_invitation),
        )
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
    device_name: Option<String>,
}

async fn auth_state(
    State(state): State<SharedState>,
    Extension(client): Extension<ClientInfo>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let pairing = state.bili.auth.pairing_state().await;
    let token = session_cookie(&headers).unwrap_or_default();
    let session = state.bili.auth.authenticate(&token, client.ip).await?;
    let mut response = Json(ApiResponse::success(json!({
        "authenticated": session.is_some(),
        "pairing_open": pairing.open,
        "pairing_expires_at": pairing.expires_at,
        "csrf_token": session.as_ref().map(|value| value.csrf_token.as_str()),
        "role": session.as_ref().map(|value| value.role),
    })))
    .into_response();
    if let Some(token) = session.and_then(|value| value.rotated_token) {
        set_session_cookie(&state.bili, &mut response, &token)?;
    }
    Ok(response)
}

async fn create_operator_invitation(
    State(state): State<SharedState>,
    Extension(session): Extension<SessionAuth>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if !session.role.is_owner() {
        return Err(AppError::Unauthorized(
            "只有 Owner 可以创建 Operator 邀请".to_string(),
        ));
    }
    let code = state.bili.auth.open_operator_invitation().await;
    Ok(Json(ApiResponse::with_message(
        json!({"role": "operator", "pairing_code": format!("{}-{}", &code[..4], &code[4..])}),
        "Operator 邀请已创建，10 分钟内有效且只能使用一次",
    )))
}

async fn pair(
    State(state): State<SharedState>,
    Extension(client): Extension<ClientInfo>,
    headers: HeaderMap,
    Json(request): Json<PairRequest>,
) -> Result<Response, AppError> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let token = state
        .bili
        .auth
        .pair(
            &request.code,
            request.device_name.as_deref().unwrap_or_default(),
            client.ip,
            user_agent,
            client.explicit_allow,
        )
        .await?;
    crate::app::onboarding::clear_pairing_code(&state.infra.paths.data_dir);
    let mut response = Json(ApiResponse::with_message(
        json!({"paired": true}),
        "设备配对成功",
    ))
    .into_response();
    set_session_cookie(&state.bili, &mut response, &token)?;
    Ok(response)
}

async fn logout(
    State(state): State<SharedState>,
    Extension(session): Extension<SessionAuth>,
) -> Result<Response, AppError> {
    state.bili.auth.revoke(&session.id).await?;
    state
        .infra
        .ws
        .disconnect_session(&session.id)
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    let mut response = Json(ApiResponse::with_message(
        json!({"logged_out": true}),
        "会话已注销",
    ))
    .into_response();
    clear_session_cookies(&mut response)?;
    Ok(response)
}

pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        matches!(name, "__Host-bili-session" | "bili-session").then(|| value.to_string())
    })
}

pub(crate) fn set_session_cookie(
    state: &BiliState,
    response: &mut Response,
    token: &str,
) -> Result<(), AppError> {
    let proxy = state.security.current().mode == AccessMode::Proxy;
    let name = if proxy {
        "__Host-bili-session"
    } else {
        "bili-session"
    };
    let secure = if proxy { "; Secure" } else { "" };
    let value =
        format!("{name}={token}; Path=/; Max-Age=2592000; HttpOnly; SameSite=Strict{secure}");
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&value)
            .map_err(|error| AppError::Internal(format!("会话 Cookie 无效: {error}")))?,
    );
    Ok(())
}

fn clear_session_cookies(response: &mut Response) -> Result<(), AppError> {
    for name in ["__Host-bili-session", "bili-session"] {
        let secure = if name.starts_with("__Host-") {
            "; Secure"
        } else {
            ""
        };
        let value = format!("{name}=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict{secure}");
        response.headers_mut().append(
            header::SET_COOKIE,
            HeaderValue::from_str(&value)
                .map_err(|error| AppError::Internal(format!("清理 Cookie 失败: {error}")))?,
        );
    }
    Ok(())
}
