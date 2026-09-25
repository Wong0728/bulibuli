//! 主题 5：凭证命令（`cred`）：扫码登录二维码、轮询扫码状态、登录状态查询。
//!
//! 扫码登录链路涉及 Cookie 落盘，属敏感操作：审计走 `record_silent`，不广播事件。

use crate::app::control::helpers::{ctl_audit_ctx, error_code_for, required};
use crate::error::{AppError, AppResult};
use crate::models::operation_log::{OperationOutcome, OperationTarget};
use crate::services::credential::Credential;
use crate::state::SharedState;
use serde_json::{json, Value};

pub(super) async fn cred_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：cred qrcode|qrcode-poll|status".to_string(),
        ));
    };
    match sub {
        "qrcode" => {
            // 生成扫码二维码是凭证操作链起点，审计走 record_silent 不广播
            let ctx = ctl_audit_ctx(
                "cred qrcode",
                OperationTarget::Cookie,
                None,
                "qrcode_generate",
                None,
            );
            let bili_api = state.bili.bili_api.clone();
            let result: AppResult<serde_json::Value> = async {
                let qrcode = bili_api
                    .get_qrcode_url()
                    .await
                    .map_err(|e| AppError::Internal(format!("获取二维码失败: {e}")))?;
                Ok(json!({
                    "qrcode_url": qrcode.url,
                    "qrcode_key": qrcode.qrcode_key,
                }))
            }
            .await;
            match result {
                Ok(value) => {
                    state
                        .infra
                        .audit_log
                        .record_silent(&ctx, OperationOutcome::Success, None, None, None)
                        .await;
                    Ok(value)
                }
                Err(error) => {
                    let code = error_code_for(&error);
                    state
                        .infra
                        .audit_log
                        .record_silent(
                            &ctx,
                            OperationOutcome::Error,
                            None,
                            Some(code),
                            Some(json!({"error": error.to_string()})),
                        )
                        .await;
                    Err(error)
                }
            }
        }
        "qrcode-poll" => {
            let key = required(args, 2, "cred qrcode-poll 需要 qrcode_key")?;
            // 扫码登录是敏感操作（Cookie 落盘）：审计走 record_silent，不广播事件。
            let ctx = ctl_audit_ctx(
                &format!("cred qrcode-poll {key}"),
                OperationTarget::Cookie,
                None,
                "qrcode_poll",
                None,
            );
            let bili_api = state.bili.bili_api.clone();
            let settings = state.infra.settings_service.clone();
            let result: AppResult<serde_json::Value> = async {
                let poll = bili_api
                    .check_qrcode_status(key)
                    .await
                    .map_err(|e| AppError::Internal(format!("轮询扫码状态失败: {e}")))?;
                // code=0 成功时落盘 Cookie。
                if poll.code == 0 {
                    if let Some(cookies) = poll.cookies.as_deref().filter(|c| !c.trim().is_empty())
                    {
                        let nav = bili_api.get_nav_info(cookies).await.map_err(|e| {
                            AppError::Unauthorized(format!("扫码凭证校验失败: {e}"))
                        })?;
                        if !nav.is_login {
                            return Err(AppError::Unauthorized(
                                "扫码凭证未通过 B 站登录校验".to_string(),
                            ));
                        }
                        settings.save_cookie_header(cookies).await?;
                        bili_api.invalidate_session_caches().await;
                    }
                }
                Ok(json!({
                    "code": poll.code,
                    "message": poll.message,
                    "logged_in": poll.code == 0,
                }))
            }
            .await;
            match result {
                Ok(value) => {
                    state
                        .infra
                        .audit_log
                        .record_silent(&ctx, OperationOutcome::Success, None, None, None)
                        .await;
                    Ok(value)
                }
                Err(error) => {
                    let code = error_code_for(&error);
                    state
                        .infra
                        .audit_log
                        .record_silent(
                            &ctx,
                            OperationOutcome::Error,
                            None,
                            Some(code),
                            Some(json!({"error": error.to_string()})),
                        )
                        .await;
                    Err(error)
                }
            }
        }
        "status" => cred_status_value(state).await,
        _ => Err(AppError::BadRequest(format!(
            "未知 cred 子命令 `{sub}`；用法：cred qrcode|qrcode-poll|status"
        ))),
    }
}

async fn cred_status_value(state: &SharedState) -> AppResult<Value> {
    let cookies = state
        .infra
        .settings_service
        .cookie_header()
        .await
        .unwrap_or_default();
    let logged_in = Credential::from_cookie_header(&cookies).is_logged_in();
    if !logged_in {
        return Ok(json!({
            "logged_in": false,
            "uid": null,
            "hint": "未登录，使用 cred qrcode 扫码登录",
        }));
    }
    let nav = state.bili.bili_api.get_nav_info(&cookies).await;
    let (uid, is_login) = match nav {
        Ok(n) => (Some(n.mid), n.is_login),
        Err(_) => (None, false),
    };
    Ok(json!({
        "logged_in": is_login,
        "uid": uid,
    }))
}
