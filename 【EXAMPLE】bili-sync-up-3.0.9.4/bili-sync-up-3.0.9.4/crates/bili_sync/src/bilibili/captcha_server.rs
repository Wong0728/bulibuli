use axum::{
    response::{Html, IntoResponse},
    Json,
};
use serde::{Deserialize, Serialize};

use super::{CaptchaResult, VERIFICATION_COORDINATOR};

/// 验证结果提交参数
#[derive(Debug, Deserialize)]
pub struct SubmitCaptchaParams {
    pub geetest_challenge: String,
    pub geetest_validate: String,
    pub geetest_seccode: String,
}

/// API响应
#[derive(Debug, Serialize)]
pub struct ApiResponse {
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub message: String,
}

impl ApiResponse {
    pub fn success<T: Serialize>(data: T) -> Self {
        Self {
            success: true,
            data: Some(serde_json::to_value(data).unwrap()),
            message: "操作成功".to_string(),
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            success: false,
            data: None,
            message,
        }
    }
}

/// 验证码页面
pub async fn serve_captcha_page() -> impl IntoResponse {
    Html(include_str!("captcha_page.html"))
}

/// 获取验证码信息API
pub async fn get_captcha_info() -> impl IntoResponse {
    if let Some(captcha_info) = VERIFICATION_COORDINATOR.get_captcha_info().await {
        Json(ApiResponse::success(captcha_info))
    } else {
        Json(ApiResponse::error("验证码信息未找到".to_string()))
    }
}

/// 提交验证结果API
pub async fn submit_captcha_result(Json(params): Json<SubmitCaptchaParams>) -> impl IntoResponse {
    tracing::info!("收到验证码结果提交");

    // 获取验证码信息以构造完整的结果
    let captcha_info = match VERIFICATION_COORDINATOR.get_captcha_info().await {
        Some(info) => info,
        None => {
            return Json(ApiResponse::error("验证码信息未找到".to_string()));
        }
    };

    // 构造验证结果
    let captcha_result = CaptchaResult {
        challenge: params.geetest_challenge,
        token: captcha_info.token.clone(),
        validate: params.geetest_validate,
        seccode: params.geetest_seccode,
    };

    // 提交结果到协调器
    match VERIFICATION_COORDINATOR.submit_captcha_result(captcha_result).await {
        Ok(_) => {
            tracing::info!("验证码结果已成功提交");
            Json(ApiResponse::success("验证完成，窗口将自动关闭"))
        }
        Err(e) => {
            tracing::error!("验证码结果提交失败: {}", e);
            Json(ApiResponse::error(format!("提交失败: {}", e)))
        }
    }
}
