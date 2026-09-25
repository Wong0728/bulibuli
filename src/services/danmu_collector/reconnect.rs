//! 重连策略：WebSocket 端点校验、指数退避 + 抖动，以及致命错误 / 鉴权错误的分类。

use super::protocol;
use tokio::time::Duration;

pub(super) fn websocket_url(host: &str, wss_port: i32) -> Option<String> {
    #[cfg(test)]
    if host.starts_with("ws://") {
        return Some(format!("{host}:{wss_port}/sub"));
    }
    let raw = if host.starts_with("wss://") {
        format!("{host}:{wss_port}/sub")
    } else {
        format!("wss://{host}:{wss_port}/sub")
    };
    crate::services::bili_url_policy::validate_live_endpoint_syntax(&raw, true)
        .ok()
        .map(|url| url.to_string())
}

pub(super) fn reconnect_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    #[cfg(test)]
    {
        Duration::from_millis(20u64 * 2u64.pow(exponent))
    }
    #[cfg(not(test))]
    {
        with_reconnect_jitter(Duration::from_secs(2u64.pow(exponent).min(30)))
    }
}

/// 给退避时长加 ±20% 抖动：多个直播间同时断网（如本地网络闪断）后，
/// 避免同频重连形成对弹幕服务器的请求风暴。测试构建下保持原值以稳定时序。
pub(super) fn with_reconnect_jitter(base: Duration) -> Duration {
    #[cfg(test)]
    {
        base
    }
    #[cfg(not(test))]
    {
        use rand::Rng;
        let factor = rand::rng().random_range(-0.2..=0.2);
        let millis = (base.as_millis() as f64 * (1.0 + factor)).max(1.0);
        Duration::from_millis(millis as u64)
    }
}

pub(super) fn is_fatal_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("操作码非法")
        || message.contains("缺少 code")
        || message.contains("协议版本")
        || message.contains("包头")
}

pub(super) fn is_auth_error(error: &anyhow::Error) -> bool {
    // 优先结构化错误：认证回复携带 B 站错误码（AuthRejected 仅在 code!=0 时构造），
    // 精确判定后再退回错误文案匹配兜底，文案变更不影响分类行为。
    if error.downcast_ref::<protocol::AuthRejected>().is_some() {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    message.contains("认证失败")
        || message.contains("认证回复")
        || message.contains("操作码非法")
        || message.contains("-101")
        || message.contains("-352")
        || message.contains("-412")
        || message.contains("-799")
        || message.contains("unauthorized")
}
