//! 登录/配对的速率限制与失败记账：每 IP 每分钟失败额度、全局每分钟额度、
//! 连续失败后的指数退避封禁，以及 `/api/auth/state` 未认证探测限流。

use super::{
    apply_failure, evict_stalest_ip, AuthService, LOGIN_RATE_LIMIT_PER_IP, STATE_PROBE_LIMIT_PER_IP,
};
use crate::error::{AppError, AppResult};
use chrono::Utc;
use std::net::IpAddr;

impl AuthService {
    /// 只读检查：本次登录/配对尝试是否被限流拦截。
    pub(super) async fn check_attempt_allowed(&self, ip: IpAddr, now: i64) -> AppResult<()> {
        // 检查每 IP 登录速率限制（每分钟最多 LOGIN_RATE_LIMIT_PER_IP 次失败）。
        // 只读不记账：成功配对不该消耗额度，否则连配几台设备就会被 409 锁住。
        {
            let login_attempts = self.login_attempts.lock().await;
            let recent = login_attempts
                .get(&ip)
                .map(|timestamps| {
                    timestamps
                        .iter()
                        .filter(|timestamp| **timestamp > now - 60)
                        .count()
                })
                .unwrap_or(0);
            if recent >= LOGIN_RATE_LIMIT_PER_IP {
                return Err(AppError::Conflict(
                    "该 IP 登录尝试过于频繁（每 IP 每分钟限 5 次），请稍后重试".to_string(),
                ));
            }
        }

        // 检查全局速率限制（每分钟最多 LOGIN_RATE_LIMIT_GLOBAL 次失败）
        let attempts = self.attempts.lock().await;
        let recent_global = attempts
            .global
            .iter()
            .filter(|timestamp| **timestamp > now - 60)
            .count();
        if recent_global >= self.login_rate_limit_global {
            return Err(AppError::Conflict(
                "登录尝试过于频繁，请稍后重试".to_string(),
            ));
        }
        if attempts
            .per_ip
            .get(&ip)
            .is_some_and(|failed| failed.blocked_until > now)
        {
            return Err(AppError::Conflict(
                "该 IP 因多次失败已被临时封禁，请稍后重试".to_string(),
            ));
        }
        Ok(())
    }

    /// 登记一次失败的登录/配对尝试（每 IP 与全局每分钟额度各 +1）。
    /// `/api/auth/pair` 是公开端点：攻击者轮换 IPv6 源地址可让 map 无限增长，
    /// 插入前先淘汰最久未活动的 IP，总量有上界。
    pub(super) async fn record_attempt(&self, ip: IpAddr, now: i64) {
        {
            let mut login_attempts = self.login_attempts.lock().await;
            evict_stalest_ip(&mut login_attempts, |timestamps| {
                timestamps.back().copied().unwrap_or(0)
            });
            let timestamps = login_attempts.entry(ip).or_default();
            timestamps.retain(|timestamp| *timestamp > now - 60);
            timestamps.push_back(now);
        }
        let mut attempts = self.attempts.lock().await;
        attempts.global.retain(|timestamp| *timestamp > now - 60);
        attempts.global.push_back(now);
    }

    /// `/api/auth/state` 未认证探测限流：每 IP 每分钟最多 STATE_PROBE_LIMIT_PER_IP 次。
    /// 已认证会话不消耗配额（由调用方判断）。抑制未认证访问者定向探测配对窗口。
    pub async fn check_state_probe_allowed(&self, ip: IpAddr) -> AppResult<()> {
        let now = Utc::now().timestamp();
        let mut probes = self.state_probes.lock().await;
        evict_stalest_ip(&mut probes, |timestamps| {
            timestamps.back().copied().unwrap_or(0)
        });
        let timestamps = probes.entry(ip).or_default();
        while timestamps
            .front()
            .is_some_and(|timestamp| *timestamp <= now - 60)
        {
            timestamps.pop_front();
        }
        if timestamps.len() >= STATE_PROBE_LIMIT_PER_IP {
            return Err(AppError::Conflict("请求过于频繁，请稍后重试".to_string()));
        }
        timestamps.push_back(now);
        Ok(())
    }

    /// 记录一次失败并推进指数退避封禁；供 `pairing` 模块调用。
    pub(super) async fn record_failure(&self, ip: IpAddr, now: i64) {
        let mut attempts = self.attempts.lock().await;
        evict_stalest_ip(&mut attempts.per_ip, |failed| failed.last_seen);
        let failed = attempts.per_ip.entry(ip).or_default();
        apply_failure(failed, now);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{apply_failure, FailedAttempts};

    #[test]
    fn failure_backoff_caps_at_one_hour() {
        let mut failed = FailedAttempts::default();
        for _ in 0..4 {
            apply_failure(&mut failed, 1000);
            assert_eq!(failed.blocked_until, 0, "前 4 次失败不封禁");
        }
        // 同一时刻连续失败（封禁未过期不触发重置）：第 5 次起指数退避 1s, 2s, 4s ... 封顶 3600s。
        let mut delays = Vec::new();
        for failures in 5..=20 {
            apply_failure(&mut failed, 1000);
            assert_eq!(failed.failures, failures);
            delays.push(failed.blocked_until - 1000);
        }
        assert_eq!(delays[0], 1);
        assert_eq!(delays[1], 2);
        assert_eq!(delays[2], 4);
        assert!(delays.iter().all(|delay| *delay <= 3600));
        assert_eq!(*delays.last().unwrap(), 3600, "退避必须封顶 1 小时");
    }

    #[test]
    fn failure_counter_resets_after_block_expires() {
        let mut failed = FailedAttempts::default();
        for _ in 0..6 {
            apply_failure(&mut failed, 1000);
        }
        assert!(failed.blocked_until > 1000);
        // 封禁过期后的首个失败：计数从 1 重新开始，而不是在旧值上继续累加。
        let expired_at = failed.blocked_until + 1;
        apply_failure(&mut failed, expired_at);
        assert_eq!(failed.failures, 1);
        assert_eq!(failed.blocked_until, 0);
    }
}
