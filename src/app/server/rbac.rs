//! 服务端 RBAC 边界：隐藏 UI 仅改善使用体验，此检查阻止已配对的
//! Operator/Viewer 会话通过直接调用 API 越权。
//!
//! 包含 `auth_bypass_ips` 白名单注入的合成 Owner 会话构造。

use crate::services::auth::{SessionAuth, SessionRole};
use axum::{body::Body, http::Request, response::Response};

use super::request_filter::is_mutating;

/// 主 Web 界面的服务端 RBAC 边界。隐藏 UI 仅改善使用体验；
/// 此检查阻止已配对的 Operator/Viewer 会话通过直接调用 API 越权。
pub(super) fn authorize_session(
    request: &Request<Body>,
    session: &SessionAuth,
) -> Result<(), Box<Response>> {
    let path = request.uri().path();
    let mutating = is_mutating(request.method());

    // 账号凭证、设置写入/敏感运维接口和设备邀请仅限 Owner 操作。
    // 业务设置的精确 GET 允许 Operator/Viewer 只读查看真实值。
    // /api/setup/* 能改写访问模式与默认策略（提权面），且暴露本机网络拓扑，
    // 全部端点仅限 Owner；未完成 onboarding 的新用户走仅回环的独立 setup 端口。
    let owner_only = path.starts_with("/api/cookies/")
        || path.starts_with("/api/settings/")
        || (path == "/api/settings" && mutating)
        || path.starts_with("/api/auth/invitations")
        || path.starts_with("/api/update/check")
        || path.starts_with("/api/update/apply")
        || path.starts_with("/api/setup/")
        || path.starts_with("/api/backup");
    if owner_only && !session.role.is_owner() {
        return Err(Box::new(super::net_headers::api_error(
            axum::http::StatusCode::FORBIDDEN,
            403,
            "此操作需要 Owner 权限",
        )));
    }

    // Viewer 可查看状态、历史和日志，但不能修改任务、
    // bloggers, rules, or recordings.  Logging out is the single safe write.
    if mutating && !session.role.can_operate() && path != "/api/auth/logout" {
        return Err(Box::new(super::net_headers::api_error(
            axum::http::StatusCode::FORBIDDEN,
            403,
            "Viewer 仅可查看内容",
        )));
    }
    Ok(())
}

/// auth_bypass_ips 命中时注入的合成会话：跳过凭证校验但保留 Owner 角色的
/// RBAC 边界（owner-only 接口放行、Viewer/Operator 限制不适用——白名单本意
/// 就是完全信任该来源）。无 csrf_token：写请求走 sec-fetch-site 同源校验分支。
pub(super) fn bypass_session() -> SessionAuth {
    SessionAuth {
        id: "auth-bypass".to_string(),
        csrf_token: String::new(),
        rotated_token: None,
        role: SessionRole::Owner,
    }
}

#[cfg(test)]
mod role_tests {
    use super::*;
    use crate::services::auth::SessionRole;
    use axum::body::Body;
    use axum::http::{Method, Request};

    fn session(role: SessionRole) -> SessionAuth {
        SessionAuth {
            id: "test".to_string(),
            csrf_token: "csrf".to_string(),
            rotated_token: None,
            role,
        }
    }

    #[test]
    fn delegated_sessions_cannot_access_credentials_or_settings() {
        let credentials = Request::builder()
            .uri("/api/cookies/status")
            .body(Body::empty())
            .unwrap();
        let settings = Request::builder()
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        assert!(authorize_session(&credentials, &session(SessionRole::Operator)).is_err());
        assert!(authorize_session(&settings, &session(SessionRole::Viewer)).is_ok());
        assert!(authorize_session(&settings, &session(SessionRole::Operator)).is_ok());
        let settings_write = Request::builder()
            .method(Method::PUT)
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        assert!(authorize_session(&settings_write, &session(SessionRole::Viewer)).is_err());
        assert!(authorize_session(&settings_write, &session(SessionRole::Operator)).is_err());
        assert!(authorize_session(&credentials, &session(SessionRole::Owner)).is_ok());
    }

    #[test]
    fn viewer_cannot_mutate_but_operator_can_manage_business_actions() {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/download/pause")
            .body(Body::empty())
            .unwrap();
        assert!(authorize_session(&request, &session(SessionRole::Viewer)).is_err());
        assert!(authorize_session(&request, &session(SessionRole::Operator)).is_ok());
    }

    #[test]
    fn auth_bypass_injects_owner_identity_through_rbac() {
        // 白名单命中：合成 Owner 会话必须通过 owner-only 路径的授权检查。
        let setup = Request::builder()
            .method(Method::POST)
            .uri("/api/setup/apply")
            .body(Body::empty())
            .unwrap();
        let credentials = Request::builder()
            .method(Method::POST)
            .uri("/api/cookies/save")
            .body(Body::empty())
            .unwrap();
        let bypass = bypass_session();
        assert_eq!(bypass.role, SessionRole::Owner);
        assert!(authorize_session(&setup, &bypass).is_ok());
        assert!(authorize_session(&credentials, &bypass).is_ok());
        // 对照：同样的请求换成 Operator 会话必须被拒——bypass 不得降低 RBAC 强度，
        // 也不能让非 Owner 角色借白名单越权。
        let operator = session(SessionRole::Operator);
        assert!(authorize_session(&setup, &operator).is_err());
        // Viewer 的只读限制同样照常生效。
        let viewer_read = Request::builder()
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        assert!(authorize_session(&viewer_read, &bypass).is_ok());
    }
}
