//! 风控冷却闸门：按 (endpoint, cookie 指纹) 记录整批失败冷却（30s）与账号级
//! 风控冷却（60s，全端点拦截）；L1/L2 worker 与手动刷新共用，避免风控期间继续打请求。

use anyhow::Result;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Default)]
pub(super) struct RefreshReport {
    pub(super) success: usize,
    pub(super) risk_limited: bool,
}

#[derive(Clone, Default)]
pub(super) struct RefreshGate {
    state: Arc<Mutex<RefreshGateState>>,
}

#[derive(Default)]
struct RefreshGateState {
    endpoint_until: HashMap<String, Instant>,
    risk_until: Option<Instant>,
}

impl RefreshGate {
    fn key(endpoint: &str, cookies: &str) -> String {
        format!(
            "{endpoint}:{}",
            crate::services::bili_api::session_fingerprint(cookies)
        )
    }

    pub(super) async fn is_blocked(&self, endpoint: &str, cookies: &str) -> bool {
        let key = Self::key(endpoint, cookies);
        let now = Instant::now();
        let mut state = self.state.lock().await;
        state.endpoint_until.retain(|_, until| *until > now);
        state.risk_until = state.risk_until.filter(|until| *until > now);
        state.risk_until.is_some_and(|until| until > now)
            || state
                .endpoint_until
                .get(&key)
                .is_some_and(|until| *until > now)
    }

    pub(super) async fn record(&self, endpoint: &str, cookies: &str, report: RefreshReport) {
        let key = Self::key(endpoint, cookies);
        let now = Instant::now();
        let mut state = self.state.lock().await;
        if report.risk_limited {
            state.risk_until = Some(now + StdDuration::from_secs(60));
            state
                .endpoint_until
                .insert(key, now + StdDuration::from_secs(5 * 60));
        } else if report.success == 0 {
            state
                .endpoint_until
                .insert(key, now + StdDuration::from_secs(30));
        } else {
            state.endpoint_until.remove(&key);
        }
    }
}

/// 被闸门拦下时跳过刷新（返回空报告）；否则执行并记账。
pub(super) async fn run_refresh<F>(
    gate: &RefreshGate,
    endpoint: &str,
    cookies: &str,
    refresh: F,
) -> Result<RefreshReport>
where
    F: Future<Output = Result<RefreshReport>>,
{
    if gate.is_blocked(endpoint, cookies).await {
        return Ok(RefreshReport::default());
    }
    let report = refresh.await?;
    gate.record(endpoint, cookies, report).await;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{RefreshGate, RefreshReport};

    #[tokio::test]
    async fn gate_blocks_endpoint_after_all_failure_and_lifts_on_success() {
        let gate = RefreshGate::default();
        assert!(!gate.is_blocked("space.bilibili.com", "cookie-a").await);
        // 整批失败（success=0）：该端点 30s 冷却
        gate.record(
            "space.bilibili.com",
            "cookie-a",
            RefreshReport {
                success: 0,
                risk_limited: false,
            },
        )
        .await;
        assert!(gate.is_blocked("space.bilibili.com", "cookie-a").await);
        // 冷却按 (endpoint, cookie 指纹) 隔离：不同 cookie / 不同端点不受影响
        assert!(!gate.is_blocked("space.bilibili.com", "cookie-b").await);
        assert!(!gate.is_blocked("api.bilibili.com", "cookie-a").await);
        // 成功后解除
        gate.record(
            "space.bilibili.com",
            "cookie-a",
            RefreshReport {
                success: 3,
                risk_limited: false,
            },
        )
        .await;
        assert!(!gate.is_blocked("space.bilibili.com", "cookie-a").await);
    }

    #[tokio::test]
    async fn gate_blocks_all_endpoints_on_risk_control() {
        let gate = RefreshGate::default();
        gate.record(
            "space.bilibili.com",
            "cookie-a",
            RefreshReport {
                success: 0,
                risk_limited: true,
            },
        )
        .await;
        // 风控是账号级：所有端点都被闸门拦下
        assert!(gate.is_blocked("space.bilibili.com", "cookie-a").await);
        assert!(gate.is_blocked("api.bilibili.com", "cookie-a").await);
        // 过期条目被清理（依赖短冷却不可行，仅验证风险窗口存在性判断本身）
        let state = gate.state.lock().await;
        assert!(state.risk_until.is_some());
        drop(state);
    }
}
