//! B 站流不可下载的业务性错误（区别于网络/风控等瞬时错误）：
//! - `permission = true`：充电专属/付费内容无观看权限，playurl 只回试看片段（durl）。
//!   下载试看片段会产出时长正确但后半段为空的坏文件，必须拒绝。
//! - `permission = false`：有权限但只有暂不支持下载语义的流（如多分段 durl）。
//!
//! 由 `AppError::From<anyhow>` 映射为 HTTP 402/400 并透传真实文案；
//! 此前用裸 anyhow 报错会被归为 Internal(500)，前端只能看到"服务器内部错误"。

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct StreamUnavailableError {
    pub message: String,
    pub permission: bool,
}

impl StreamUnavailableError {
    pub fn permission(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permission: true,
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permission: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StreamUnavailableError;

    #[test]
    fn stream_unavailable_error_flags_permission() {
        let permission = StreamUnavailableError::permission("无权限");
        assert!(permission.permission);
        let unsupported = StreamUnavailableError::unsupported("多分段");
        assert!(!unsupported.permission);
    }
}
