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
        // csrf token 不经公开的 /api/auth/state 暴露；持有效会话 Cookie 时经此认证端点获取。
        .route("/api/auth/csrf", get(auth_csrf))
        .route("/api/auth/pair", post(pair))
        .route("/api/auth/logout", post(logout))
        .route(
            "/api/auth/invitations/operator",
            post(create_operator_invitation),
        )
}

/// 会话 Cookie 名：Proxy 模式使用带 `__Host-` 前缀的名字（浏览器会强制
/// Secure + Path=/ + 无 Domain），其它模式使用普通名字。
const HOST_SESSION_NAME: &str = "__Host-bili-session";
const SESSION_NAME: &str = "bili-session";

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
    let token = session_cookie(
        &headers,
        state.bili.security.current().mode == AccessMode::Proxy,
    )
    .unwrap_or_default();
    let session = state.bili.auth.authenticate(&token, client.ip).await?;
    if session.is_none() {
        // 未认证访问：限流探测，抑制远程枚举配对窗口（已配对设备不受影响）。
        state.bili.auth.check_state_probe_allowed(client.ip).await?;
    }
    let mut response = Json(ApiResponse::success(json!({
        "authenticated": session.is_some(),
        "pairing_open": pairing.open,
        "pairing_expires_at": pairing.expires_at,
        // 服务器时间（Unix 秒）：前端据此校准倒计时，消除客户端时钟偏差。
        "server_time": chrono::Utc::now().timestamp(),
        "role": session.as_ref().map(|value| value.role),
    })))
    .into_response();
    if let Some(token) = session.and_then(|value| value.rotated_token) {
        set_session_cookie(&state.bili, &mut response, &token)?;
    }
    Ok(response)
}

/// 认证端点（不在 is_public_path 中，中间件强制有效会话）：
/// 返回当前会话的 CSRF token，供前端发起写请求时携带。
async fn auth_csrf(
    Extension(session): Extension<Option<SessionAuth>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    let session = session
        .ok_or_else(|| AppError::Unauthorized("需要有效会话才能获取 CSRF Token".to_string()))?;
    Ok(Json(ApiResponse::success(json!({
        "csrf_token": session.csrf_token,
    }))))
}

async fn create_operator_invitation(
    State(state): State<SharedState>,
    Extension(session): Extension<Option<SessionAuth>>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // auth_bypass_ips 命中时中间件不产生会话：显式提示而非 MissingExtension 500。
    let session = session.ok_or_else(|| {
        AppError::Unauthorized("认证跳过白名单模式下无会话上下文，无法创建邀请".to_string())
    })?;
    if !session.role.is_owner() {
        return Err(AppError::Unauthorized(
            "只有 Owner 可以创建 Operator 邀请".to_string(),
        ));
    }
    let code = state.bili.auth.open_operator_invitation().await;
    // 按字符边界安全切分（不依赖 generate_code 恒为 8 字节 ASCII 的隐式约定）。
    let split = code
        .char_indices()
        .nth(4)
        .map(|(index, _)| index)
        .unwrap_or(code.len());
    Ok(Json(ApiResponse::with_message(
        json!({"role": "operator", "pairing_code": format!("{}-{}", &code[..split], &code[split..])}),
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
    Extension(session): Extension<Option<SessionAuth>>,
) -> Result<Response, AppError> {
    let session = session.ok_or_else(|| {
        AppError::Unauthorized("认证跳过白名单模式下无会话上下文，无需注销".to_string())
    })?;
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

/// 读取会话 Cookie。`proxy` 为当前访问模式是否为 Proxy——该模式下发的是
/// `__Host-bili-session`，其它模式是 `bili-session`。
///
/// 优先取与模式匹配的名字：否则任何能在父域种 `bili-session` 的子站（反代域名下的
/// 其它站点）都能遮蔽合法会话，把请求固定到攻击者的会话上。只有在缺少匹配名字时才
/// 回退到另一个，避免模式切换后旧 Cookie 直接失效、迫使已配对设备重新走配对。
pub(crate) fn session_cookie(headers: &HeaderMap, proxy: bool) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    let preferred = if proxy {
        HOST_SESSION_NAME
    } else {
        SESSION_NAME
    };
    let fallback = if proxy {
        SESSION_NAME
    } else {
        HOST_SESSION_NAME
    };
    let mut fallback_value = None;
    for part in raw.split(';') {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if name == preferred {
            return Some(value.to_string());
        }
        if name == fallback {
            fallback_value = Some(value.to_string());
        }
    }
    fallback_value
}

pub(crate) fn set_session_cookie(
    state: &BiliState,
    response: &mut Response,
    token: &str,
) -> Result<(), AppError> {
    let proxy = state.security.current().mode == AccessMode::Proxy;
    let name = if proxy {
        HOST_SESSION_NAME
    } else {
        SESSION_NAME
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

#[cfg(test)]
mod tests {
    use super::{session_cookie, HOST_SESSION_NAME, SESSION_NAME};
    use axum::http::{header::COOKIE, HeaderMap};

    fn cookie_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let raw = pairs
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, raw.parse().expect("cookie header"));
        headers
    }

    #[test]
    fn mode_matching_cookie_wins_regardless_of_order() {
        // 遮蔽攻击：父域其它子站能种 `bili-session`，它排在合法 Cookie 之前也不能生效。
        let headers = cookie_headers(&[(SESSION_NAME, "planted"), (HOST_SESSION_NAME, "owner")]);
        assert_eq!(
            session_cookie(&headers, true).as_deref(),
            Some("owner"),
            "Proxy 模式必须优先 __Host- 名字"
        );
        let reversed = cookie_headers(&[(HOST_SESSION_NAME, "owner"), (SESSION_NAME, "planted")]);
        assert_eq!(session_cookie(&reversed, true).as_deref(), Some("owner"));
        assert_eq!(session_cookie(&headers, false).as_deref(), Some("planted"));
    }

    #[test]
    fn other_name_only_used_as_fallback_after_mode_switch() {
        // 模式切换（需要重启）后旧 Cookie 不应立刻失效、迫使已配对设备重新配对。
        let only_plain = cookie_headers(&[(SESSION_NAME, "token")]);
        assert_eq!(session_cookie(&only_plain, true).as_deref(), Some("token"));
        let only_host = cookie_headers(&[(HOST_SESSION_NAME, "token")]);
        assert_eq!(session_cookie(&only_host, false).as_deref(), Some("token"));
        assert_eq!(session_cookie(&HeaderMap::new(), false), None);
        assert_eq!(
            session_cookie(&cookie_headers(&[("other", "x")]), false).as_deref(),
            None
        );
    }
}
