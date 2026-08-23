use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

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
            -352 | -403 | -412 | -799 => BiliErrorKind::RiskControl,
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

#[derive(Clone, Debug, thiserror::Error)]
#[error("反序列化 B站 API 响应失败: {0}")]
/// 带类型标记的上游响应解析错误：anyhow 链中用它替代字符串匹配
/// （`detail.contains("反序列化 B站 API")`）做分类，`From<anyhow::Error>`
/// 通过 downcast 识别。
pub struct BiliDeserializeError(pub String);

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

    #[error("禁止访问: {0}")]
    Forbidden(String),

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

    /// 充电专属/付费内容无观看权限等确定性拦截：HTTP 402 + 真实文案。
    /// 不用 403，因为前端把所有 403 统一按"B站风控"弹窗处理。
    #[error("需要充电或付费权限: {0}")]
    PaymentRequired(String),

    #[error("JSON 解析/序列化错误: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        if let Some(error) = value.downcast_ref::<BiliApiError>() {
            return error.clone().into();
        }
        // 类型化标记优先：bili_api 解析失败统一抛 BiliDeserializeError。
        if value.downcast_ref::<BiliDeserializeError>().is_some() {
            return Self::Upstream(format!("{value:#}"));
        }
        // 流不可下载的业务性错误（充电/付费无权限、多分段 durl）：
        // 确定性失败，透传真实文案给前端（此前裸 anyhow 会归为 Internal(500)，
        // 用户只能看到"服务器内部错误"，也是充电视频"报错两次"的噪声来源之一）。
        if let Some(error) =
            value.downcast_ref::<crate::services::bili_api::StreamUnavailableError>()
        {
            return if error.permission {
                Self::PaymentRequired(error.message.clone())
            } else {
                Self::BadRequest(error.message.clone())
            };
        }
        let detail = format!("{value:#}");
        if value.chain().any(|cause| cause.is::<reqwest::Error>()) {
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
                // 内部错误码统一落在 1xxx 区间，避免与 B 站原始业务码（-352/-101/
                // 86038 等）及 HTTP 状态码语义混淆；前端只对 0/502/-101/-352/-403
                // 做特殊分支，1xxx 走通用错误提示，不受影响。
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    1500,
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
            Self::Config(message) => {
                // 配置错误消息通常含本地路径（security.toml 等）：完整信息进日志，
                // 响应只回固定文案，避免向客户端泄漏文件系统布局。
                error!(error = %message, "配置错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    1501,
                    "配置无效，请检查设置后重试".to_string(),
                )
            }
            Self::HeaderValue(err) => (StatusCode::BAD_REQUEST, 400, err.to_string()),
            Self::Io(err) => {
                error!(error = %err, "IO错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    1502,
                    "文件操作失败，请稍后重试".to_string(),
                )
            }
            Self::NotFound(message) => (StatusCode::NOT_FOUND, 404, message.clone()),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, 403, message.clone()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, 400, message.clone()),
            Self::Conflict(message) => (StatusCode::CONFLICT, 409, message.clone()),
            Self::Unauthorized(message) => (StatusCode::UNAUTHORIZED, 401, message.clone()),
            Self::RiskControl(message) => (StatusCode::FORBIDDEN, 403, message.clone()),
            Self::AiSkillDisabled(message) => (StatusCode::FORBIDDEN, 403, message.clone()),
            // B 站未登录必须与设备会话失效（Unauthorized，code 401）区分开：
            // envelope code 复用 B 站原始业务码 -101，前端据此走"登录已过期"
            // 而非"设备会话已失效，请重新配对"。
            Self::BiliNotLoggedIn(message) => (StatusCode::UNAUTHORIZED, -101, message.clone()),
            Self::ExternalProcess(message) => {
                // aria2/FFmpeg 诊断可能含 RPC endpoint、URL 签名或本地路径：
                // 脱敏后回传（保留 filter 不支持等有价值的非敏感诊断），全文进日志。
                error!(error = %message, "外部进程错误");
                (
                    StatusCode::BAD_GATEWAY,
                    502,
                    crate::services::live_recorder::ffmpeg_session::redact_diagnostics(message),
                )
            }
            Self::Internal(message) => {
                error!(error = %message, "内部错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    1503,
                    "服务器内部错误，请稍后重试".to_string(),
                )
            }
            Self::PaymentRequired(message) => {
                info!(message = %message, "下载被充电/付费权限拦截");
                // 402 Payment Required：语义精确且不会触发前端的 403 风控弹窗分支
                (StatusCode::PAYMENT_REQUIRED, 402, message.clone())
            }
            Self::Json(err) => {
                error!(error = %err, "JSON错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    1504,
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
    fn typed_deserialize_error_downcasts_to_upstream() {
        let error = AppError::from(anyhow::Error::new(BiliDeserializeError(
            "playurl data".to_string(),
        )));
        assert!(matches!(error, AppError::Upstream(_)));
    }

    #[test]
    fn stream_unavailable_maps_to_client_visible_errors() {
        use crate::services::bili_api::StreamUnavailableError;
        // 权限型 → 402 + 真实文案（不触发前端风控弹窗）
        let permission = AppError::from(anyhow::anyhow!(StreamUnavailableError::permission(
            "该视频为充电专属内容，当前账号没有观看权限，仅能获取试看片段",
        )));
        assert!(matches!(permission, AppError::PaymentRequired(_)));
        let (status, code, message) = permission.response_parts();
        assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
        assert_eq!(code, 402);
        assert!(message.contains("充电专属"));
        // 格式不支持 → 400 + 真实文案
        let unsupported = AppError::from(anyhow::anyhow!(StreamUnavailableError::unsupported(
            "该视频仅提供 2 段分段流（durl），暂不支持分段下载拼接",
        )));
        let (status, _, message) = unsupported.response_parts();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(message.contains("分段"));
    }

    #[test]
    fn internal_error_codes_live_in_1xxx_range() {
        for error in [
            AppError::Database(sea_orm::DbErr::Custom("x".into())),
            AppError::Config("x".into()),
            AppError::Io(std::io::Error::other("x")),
            AppError::Internal("x".into()),
            AppError::Json(serde_json::from_str::<serde_json::Value>("").unwrap_err()),
        ] {
            let (_, code, _) = error.response_parts();
            assert!((1000..2000).contains(&code), "code {code} 应落在 1xxx 区间");
        }
    }

    #[test]
    fn malformed_bili_payload_is_reported_as_bad_gateway() {
        let error = AppError::from(anyhow::Error::new(BiliDeserializeError(
            "反序列化 B站 API playurl data 失败，字段路径=dash.dolby.audio".to_string(),
        )));
        assert!(matches!(error, AppError::Upstream(_)));
        let (status, code, message) = error.response_parts();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(code, 502);
        assert_eq!(message, "B站服务响应异常，请稍后重试");
    }
}
