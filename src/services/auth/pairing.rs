//! 设备配对：一次性配对码窗口的开启/关闭与配对握手（限流、GeoIP 区域判定、
//! 常量时间码比较、会话创建），以及首次启动的 Owner 引导配对。

use super::{generate_code, AttemptBook, AuthService, PairAttempt, PairWindow, PAIR_TTL_SECONDS};
use crate::error::{AppError, AppResult};
use crate::services::security_config::SecurityConfigService;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

impl AuthService {
    pub async fn new(
        db: DatabaseConnection,
        security: Arc<SecurityConfigService>,
        login_rate_limit_global: usize,
    ) -> AppResult<(Self, Option<String>)> {
        let geo_reader = security.effective_geo_db().and_then(|path| {
            match maxminddb::Reader::open_readfile(path) {
                Ok(reader) => Some(reader),
                Err(error) => {
                    tracing::warn!(%error, "GeoIP 数据库加载失败，新配对将被拒绝");
                    None
                }
            }
        });
        let service = Self {
            db,
            security,
            geo_reader: Arc::new(geo_reader),
            pairing: Arc::new(Mutex::new(None)),
            attempts: Arc::new(Mutex::new(AttemptBook::default())),
            authentication_locks: Arc::new(Mutex::new(HashMap::new())),
            login_attempts: Arc::new(Mutex::new(HashMap::new())),
            state_probes: Arc::new(Mutex::new(HashMap::new())),
            login_rate_limit_global: login_rate_limit_global.max(1),
        };
        let initial = if service.bootstrap_needed().await? {
            let code = service.open_pairing().await;
            service.mark_bootstrapped().await?;
            Some(code)
        } else {
            None
        };
        Ok((service, initial))
    }

    pub async fn pairing_state(&self) -> super::PairingState {
        let now = Utc::now().timestamp();
        let mut guard = self.pairing.lock().await;
        if guard
            .as_ref()
            .is_some_and(|window| window.expires_at <= now)
        {
            *guard = None;
        }
        super::PairingState {
            open: guard.is_some(),
            expires_at: guard.as_ref().map(|window| window.expires_at),
        }
    }

    pub async fn open_pairing(&self) -> String {
        self.open_pairing_for_role(super::SessionRole::Owner).await
    }

    /// Owner-only 调用方用此方法创建受限的设备邀请。
    /// 新邀请会替换旧的未使用码，保持配对码一次性且短时有效。
    pub async fn open_operator_invitation(&self) -> String {
        self.open_pairing_for_role(super::SessionRole::Operator)
            .await
    }

    async fn open_pairing_for_role(&self, role: super::SessionRole) -> String {
        let code = generate_code();
        *self.pairing.lock().await = Some(PairWindow {
            code: code.clone(),
            expires_at: Utc::now().timestamp() + PAIR_TTL_SECONDS,
            role,
        });
        code
    }

    pub async fn close_pairing(&self) {
        *self.pairing.lock().await = None;
    }

    pub async fn pair(
        &self,
        submitted: &str,
        device_name: &str,
        ip: IpAddr,
        user_agent: &str,
        explicit_allow: bool,
    ) -> AppResult<String> {
        let now = Utc::now().timestamp();
        if let Err(error) = self.check_attempt_allowed(ip, now).await {
            tracing::warn!(%ip, %error, "配对请求被限流拒绝");
            return Err(error);
        }
        if !explicit_allow && self.security.current().geo_cn {
            match self.ip_is_cn(ip) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(%ip, "配对失败：来源网络不在允许区域");
                    self.record_attempt(ip, now).await;
                    return Err(AppError::Unauthorized(
                        "当前网络区域不允许发起新设备配对".to_string(),
                    ));
                }
                Err(error) => {
                    tracing::warn!(%ip, %error, "配对失败：GeoIP 判定不可用");
                    self.record_attempt(ip, now).await;
                    return Err(error);
                }
            }
        }
        let normalized = submitted.replace('-', "").trim().to_ascii_uppercase();
        let attempt = {
            let mut window = self.pairing.lock().await;
            let outcome = match window.as_ref() {
                None => PairAttempt::WindowUnavailable,
                Some(current) if current.expires_at <= now => PairAttempt::WindowUnavailable,
                Some(current) => {
                    let equal = normalized.len() == current.code.len()
                        && normalized.as_bytes().ct_eq(current.code.as_bytes()).into();
                    if equal {
                        PairAttempt::Granted(current.role)
                    } else {
                        PairAttempt::CodeMismatch
                    }
                }
            };
            // 过期与成功都要作废窗口（一次性码不得重复使用）；码不匹配时保留窗口供重试。
            if !matches!(outcome, PairAttempt::CodeMismatch) {
                *window = None;
            }
            outcome
        };
        match attempt {
            PairAttempt::Granted(role) => {
                self.attempts.lock().await.per_ip.remove(&ip);
                self.create_session(device_name, ip, user_agent, role).await
            }
            PairAttempt::WindowUnavailable => {
                tracing::warn!(%ip, "配对失败：当前未开放设备配对或配对码已过期");
                Err(AppError::Unauthorized("当前未开放设备配对".to_string()))
            }
            PairAttempt::CodeMismatch => {
                tracing::warn!(%ip, "配对失败：配对码不匹配");
                self.record_attempt(ip, now).await;
                self.record_failure(ip, now).await;
                Err(AppError::Unauthorized("配对码无效，请稍后重试".to_string()))
            }
        }
    }

    fn ip_is_cn(&self, ip: IpAddr) -> AppResult<bool> {
        let reader =
            self.geo_reader.as_ref().as_ref().ok_or_else(|| {
                AppError::Unauthorized("GeoIP 数据不可用，已拒绝新配对".to_string())
            })?;
        let country: maxminddb::geoip2::Country = reader
            .lookup(ip)
            .map_err(|_| AppError::Unauthorized("无法判断网络区域，已拒绝新配对".to_string()))?
            .decode()
            .map_err(|_| AppError::Unauthorized("无法判断网络区域，已拒绝新配对".to_string()))?
            .ok_or_else(|| AppError::Unauthorized("无法判断网络区域，已拒绝新配对".to_string()))?;
        Ok(country.country.iso_code.is_some_and(|code| code == "CN"))
    }

    async fn bootstrap_needed(&self) -> AppResult<bool> {
        let row = self
            .db
            .query_one_raw(Statement::from_string(
                self.db.get_database_backend(),
                "SELECT value FROM security_meta WHERE key = 'pairing_bootstrapped'".to_string(),
            ))
            .await?;
        Ok(row.is_none())
    }

    async fn mark_bootstrapped(&self) -> AppResult<()> {
        let transaction = self.db.begin().await?;
        transaction
            .execute_raw(Statement::from_string(
                transaction.get_database_backend(),
                "INSERT OR REPLACE INTO security_meta (key, value)
                 VALUES ('pairing_bootstrapped', 'true')"
                    .to_string(),
            ))
            .await?;
        transaction.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{generate_code, CODE_ALPHABET};

    #[test]
    fn pairing_code_uses_unambiguous_alphabet() {
        for _ in 0..100 {
            let code = generate_code();
            assert_eq!(code.len(), 8);
            assert!(code.bytes().all(|byte| CODE_ALPHABET.contains(&byte)));
        }
    }
}
