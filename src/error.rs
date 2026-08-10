use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Clone, Debug, Serialize)]
pub struct ApiResponse<T> {
    pub code: i64,
    pub message: String,
    pub data: T,
}

impl<T> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            code: 0,
            message: "success".to_string(),
            data,
        }
    }

    pub fn with_message(data: T, message: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: message.into(),
            data,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BiliErrorKind {
    RiskControl,
    Unauthorized,
    NotFound,
    BadRequest,
    RateLimited,
    Server,
    InvalidResponse,
    ChargeRequired,
    GeoRestricted,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("B站API错误(code={code}, kind={kind:?}): {message}")]
pub struct BiliApiError {
    pub code: i64,
    pub kind: BiliErrorKind,
    pub message: String,
    pub retryable: bool,
}

impl BiliApiError {
    pub fn classify(code: i64, message: impl Into<String>) -> Self {
        let kind = match code {
            -352 | -403 => BiliErrorKind::RiskControl,
            -101 => BiliErrorKind::Unauthorized,
            -404 | 62002 => BiliErrorKind::NotFound,
            87007 => BiliErrorKind::ChargeRequired,
            100150 | 1001501 => BiliErrorKind::GeoRestricted,
            -400 => BiliErrorKind::BadRequest,
            429 => BiliErrorKind::RateLimited,
            500..=599 => BiliErrorKind::Server,
            _ => BiliErrorKind::InvalidResponse,
        };
        let retryable = matches!(kind, BiliErrorKind::RateLimited | BiliErrorKind::Server);
        Self {
            code,
            kind,
            message: message.into(),
            retryable,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    BiliApi(BiliApiError),

    #[error("数据库错误: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("网络请求错误: {0}")]
    Network(#[from] reqwest::Error),

    #[error("B站上游响应错误: {0}")]
    Upstream(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("HTTP 请求头错误: {0}")]
    HeaderValue(#[from] reqwest::header::InvalidHeaderValue),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("未找到: {0}")]
    NotFound(String),

    #[error("参数错误: {0}")]
    BadRequest(String),

    #[error("资源冲突: {0}")]
    Conflict(String),

    #[error("登录已失效: {0}")]
    Unauthorized(String),

    #[error("触发B站风控: {0}")]
    RiskControl(String),

    #[error("AI Skill 模式未启用: {0}")]
    AiSkillDisabled(String),

    #[error("B 站未登录: {0}")]
    BiliNotLoggedIn(String),

    #[error("外部进程错误: {0}")]
    ExternalProcess(String),

    #[error("内部错误: {0}")]
    Internal(String),

    #[error("JSON 解析/序列化错误: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        if let Some(error) = value.downcast_ref::<BiliApiError>() {
            return error.clone().into();
        }
        let detail = format!("{value:#}");
        if value.chain().any(|cause| cause.is::<reqwest::Error>())
            || detail.contains("反序列化 B站 API")
        {
            return Self::Upstream(detail);
        }
        Self::Internal(detail)
    }
}

impl From<BiliApiError> for AppError {
    fn from(error: BiliApiError) -> Self {
        match error.kind {
            BiliErrorKind::Unauthorized => Self::Unauthorized(error.message),
            BiliErrorKind::RiskControl => Self::RiskControl(error.message),
            _ => Self::BiliApi(error),
        }
    }
}

impl From<crate::services::aria2::Aria2Error> for AppError {
    fn from(error: crate::services::aria2::Aria2Error) -> Self {
        Self::ExternalProcess(error.to_string())
    }
}

impl AppError {
    fn response_parts(&self) -> (StatusCode, i64, String) {
        match self {
            Self::BiliApi(bili) => match bili.kind {
                BiliErrorKind::RiskControl => (
                    StatusCode::FORBIDDEN,
                    bili.code,
                    "B站风控拦截，请稍后重试或完成验证".to_string(),
                ),
                BiliErrorKind::Unauthorized => (
                    StatusCode::UNAUTHORIZED,
                    bili.code,
                    "B站登录已失效，请重新登录".to_string(),
                ),
                BiliErrorKind::NotFound => (StatusCode::NOT_FOUND, bili.code, bili.message.clone()),
                BiliErrorKind::BadRequest => {
                    (StatusCode::BAD_REQUEST, bili.code, bili.message.clone())
                }
                BiliErrorKind::RateLimited => (
                    StatusCode::TOO_MANY_REQUESTS,
                    bili.code,
                    bili.message.clone(),
                ),
                BiliErrorKind::Server | BiliErrorKind::InvalidResponse => (
                    StatusCode::BAD_GATEWAY,
                    bili.code,
                    "B站服务响应异常，请稍后重试".to_string(),
                ),
                BiliErrorKind::ChargeRequired => (
                    StatusCode::FORBIDDEN,
                    bili.code,
                    "该视频需要充电或付费权限，已跳过自动重试".to_string(),
                ),
                BiliErrorKind::GeoRestricted => (
                    StatusCode::FORBIDDEN,
                    bili.code,
                    "该视频受地区限制，已跳过自动重试".to_string(),
                ),
            },
            Self::Database(err) => {
                error!(error = %err, "数据库错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    500,
                    "服务器内部错误，请稍后重试".to_string(),
                )
            }
            Self::Network(err) => {
                error!(error = %err, "网络请求错误");
                (
                    StatusCode::BAD_GATEWAY,
                    502,
                    "网络请求失败，请稍后重试".to_string(),
                )
            }
            Self::Upstream(message) => {
                error!(error = %message, "B站上游响应错误");
                (
                    StatusCode::BAD_GATEWAY,
                    502,
                    "B站服务响应异常，请稍后重试".to_string(),
                )
            }
            Self::Config(message) => (StatusCode::INTERNAL_SERVER_ERROR, 500, message.clone()),
            Self::HeaderValue(err) => (StatusCode::BAD_REQUEST, 400, err.to_string()),
            Self::Io(err) => {
                error!(error = %err, "IO错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    500,
                    "文件操作失败，请稍后重试".to_string(),
                )
            }
            Self::NotFound(message) => (StatusCode::NOT_FOUND, 404, message.clone()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, 400, message.clone()),
            Self::Conflict(message) => (StatusCode::CONFLICT, 409, message.clone()),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, 401, message.clone()),
            Self::RiskControl(message) => (StatusCode::FORBIDDEN, 403, message.clone()),
            Self::AiSkillDisabled(message) => (StatusCode::FORBIDDEN, 403, message.clone()),
            Self::BiliNotLoggedIn(message) => (StatusCode::UNAUTHORIZED, 401, message.clone()),
            Self::ExternalProcess(message) => (StatusCode::BAD_GATEWAY, 502, message.clone()),
            Self::Internal(message) => {
                error!(error = %message, "内部错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    500,
                    "服务器内部错误，请稍后重试".to_string(),
                )
            }
            Self::Json(err) => {
                error!(error = %err, "JSON错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    500,
                    "数据处理失败，请稍后重试".to_string(),
                )
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.response_parts();
        (
            status,
            Json(ApiResponse {
                code,
                message,
                data: serde_json::Value::Null,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bili_codes_are_classified() {
        assert_eq!(
            BiliApiError::classify(-352, "risk").kind,
            BiliErrorKind::RiskControl
        );
        assert_eq!(
            BiliApiError::classify(-101, "auth").kind,
            BiliErrorKind::Unauthorized
        );
        assert!(!BiliApiError::classify(-400, "bad").retryable);
        assert!(BiliApiError::classify(503, "server").retryable);
    }

    #[test]
    fn success_envelope_is_stable() {
        let value = serde_json::to_value(ApiResponse::success(serde_json::json!({"id": 1})))
            .expect("serializable response");
        assert_eq!(value["code"], 0);
        assert_eq!(value["message"], "success");
        assert!(value.get("success").is_none());
    }

    #[test]
    fn malformed_bili_payload_is_reported_as_bad_gateway() {
        let error = AppError::from(anyhow::anyhow!(
            "反序列化 B站 API playurl data 失败，字段路径=dash.dolby.audio"
        ));
        assert!(matches!(error, AppError::Upstream(_)));
        let (status, code, message) = error.response_parts();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(code, 502);
        assert_eq!(message, "B站服务响应异常，请稍后重试");
    }
}
