use crate::error::{AppError, AppResult};
use crate::services::security_config::SecurityConfigService;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::Utc;
use rand::{Rng, RngCore};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

const PAIR_TTL_SECONDS: i64 = 600;
const SESSION_IDLE_SECONDS: i64 = 30 * 24 * 60 * 60;
const SESSION_ABSOLUTE_SECONDS: i64 = 90 * 24 * 60 * 60;
const ROTATE_AFTER_SECONDS: i64 = 24 * 60 * 60;
const PREVIOUS_TOKEN_GRACE_SECONDS: i64 = 120;
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
/// 每 IP 每分钟最大登录/配对尝试次数
const LOGIN_RATE_LIMIT_PER_IP: usize = 5;
const MAX_AUTHENTICATION_LOCKS: usize = 4096;

#[derive(Clone)]
pub struct AuthService {
    db: DatabaseConnection,
    security: Arc<SecurityConfigService>,
    geo_reader: Arc<Option<maxminddb::Reader<Vec<u8>>>>,
    pairing: Arc<Mutex<Option<PairWindow>>>,
    attempts: Arc<Mutex<AttemptBook>>,
    authentication_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// 每 IP 登录尝试时间戳，用于速率限制
    login_attempts: Arc<Mutex<HashMap<IpAddr, VecDeque<i64>>>>,
    /// 全局每分钟最大登录/配对尝试次数（由 AppConfig 注入）
    login_rate_limit_global: usize,
}

struct PairWindow {
    code: String,
    expires_at: i64,
    role: SessionRole,
}

struct AuthSessionRow {
    id: String,
    csrf_token: String,
    role: SessionRole,
    expires_at: i64,
    absolute_expires_at: i64,
    last_rotated_at: i64,
}

#[derive(Default)]
struct AttemptBook {
    per_ip: HashMap<IpAddr, FailedAttempts>,
    global: VecDeque<i64>,
}

#[derive(Default)]
struct FailedAttempts {
    failures: u32,
    blocked_until: i64,
}

#[derive(Clone, Debug)]
pub struct SessionAuth {
    pub id: String,
    pub csrf_token: String,
    pub rotated_token: Option<String>,
    pub role: SessionRole,
}

/// Web 会话能力。能力绑定到会话而不是 IP，避免 IPv6 或反向代理部署中
/// 因地址判断错误而将已配对的 Viewer 提升为管理员。
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    Owner,
    Operator,
    Viewer,
}

impl SessionRole {
    fn from_db(value: &str) -> Self {
        match value {
            "operator" => Self::Operator,
            "viewer" => Self::Viewer,
            _ => Self::Owner,
        }
    }

    fn as_db(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Operator => "operator",
            Self::Viewer => "viewer",
        }
    }

    pub fn is_owner(self) -> bool {
        self == Self::Owner
    }

    pub fn can_operate(self) -> bool {
        matches!(self, Self::Owner | Self::Operator)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClientInfo {
    pub ip: IpAddr,
    pub explicit_allow: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub device_name: String,
    pub created_at: i64,
    pub last_used_at: i64,
    pub last_ip: String,
    pub expires_at: i64,
    pub role: SessionRole,
}

#[derive(Clone, Debug, Serialize)]
pub struct PairingState {
    pub open: bool,
    pub expires_at: Option<i64>,
}

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

    pub async fn pairing_state(&self) -> PairingState {
        let now = Utc::now().timestamp();
        let mut guard = self.pairing.lock().await;
        if guard
            .as_ref()
            .is_some_and(|window| window.expires_at <= now)
        {
            *guard = None;
        }
        PairingState {
            open: guard.is_some(),
            expires_at: guard.as_ref().map(|window| window.expires_at),
        }
    }

    pub async fn open_pairing(&self) -> String {
        self.open_pairing_for_role(SessionRole::Owner).await
    }

    /// Owner-only 调用方用此方法创建受限的设备邀请。
    /// 新邀请会替换旧的未使用码，保持配对码一次性且短时有效。
    pub async fn open_operator_invitation(&self) -> String {
        self.open_pairing_for_role(SessionRole::Operator).await
    }

    async fn open_pairing_for_role(&self, role: SessionRole) -> String {
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
        self.check_attempt_allowed(ip, now).await?;
        if !explicit_allow && self.security.current().geo_cn && !self.ip_is_cn(ip)? {
            return Err(AppError::Unauthorized(
                "当前网络区域不允许发起新设备配对".to_string(),
            ));
        }
        let normalized = submitted.replace('-', "").trim().to_ascii_uppercase();
        let role = {
            let mut window = self.pairing.lock().await;
            let Some(current) = window.as_ref() else {
                return Err(AppError::Unauthorized("当前未开放设备配对".to_string()));
            };
            if current.expires_at <= now {
                *window = None;
                return Err(AppError::Unauthorized("当前未开放设备配对".to_string()));
            }
            let equal = normalized.len() == current.code.len()
                && normalized.as_bytes().ct_eq(current.code.as_bytes()).into();
            if equal {
                let role = current.role;
                *window = None;
                Some(role)
            } else {
                None
            }
        };
        let Some(role) = role else {
            self.record_failure(ip, now).await;
            return Err(AppError::Unauthorized("配对码无效，请稍后重试".to_string()));
        };
        self.attempts.lock().await.per_ip.remove(&ip);
        self.create_session(device_name, ip, user_agent, role).await
    }

    pub async fn authenticate(&self, token: &str, ip: IpAddr) -> AppResult<Option<SessionAuth>> {
        if token.is_empty() {
            return Ok(None);
        }
        let now = Utc::now().timestamp();
        let hash = token_hash(token);
        let Some(candidate) = self.find_session(&hash, now).await? else {
            return Ok(None);
        };
        let session_lock = self.session_lock(&candidate.id).await;
        let _guard = session_lock.lock().await;
        let Some(row) = self.find_session(&hash, now).await? else {
            return Ok(None);
        };
        if row.expires_at <= now || row.absolute_expires_at <= now {
            return Ok(None);
        }
        let next_expiry = (now + SESSION_IDLE_SECONDS).min(row.absolute_expires_at);
        let rotated_token = if now - row.last_rotated_at >= ROTATE_AFTER_SECONDS {
            Some(random_token())
        } else {
            None
        };
        let statement = if let Some(new_token) = rotated_token.as_ref() {
            Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "UPDATE auth_sessions SET
                   previous_token_hash = token_hash,
                   previous_valid_until = ?,
                   token_hash = ?,
                   expires_at = ?,
                   last_used_at = ?,
                   last_rotated_at = ?,
                   last_ip = ?
                 WHERE id = ?"
                    .to_string(),
                [
                    sea_orm::Value::from(now + PREVIOUS_TOKEN_GRACE_SECONDS),
                    bytes_value(token_hash(new_token)),
                    sea_orm::Value::from(next_expiry),
                    sea_orm::Value::from(now),
                    sea_orm::Value::from(now),
                    sea_orm::Value::from(ip.to_string()),
                    sea_orm::Value::from(row.id.clone()),
                ],
            )
        } else {
            Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "UPDATE auth_sessions SET expires_at = ?, last_used_at = ?, last_ip = ? WHERE id = ?"
                    .to_string(),
                [
                    sea_orm::Value::from(next_expiry),
                    sea_orm::Value::from(now),
                    sea_orm::Value::from(ip.to_string()),
                    sea_orm::Value::from(row.id.clone()),
                ],
            )
        };
        self.db.execute_raw(statement).await?;
        Ok(Some(SessionAuth {
            id: row.id,
            csrf_token: row.csrf_token,
            rotated_token,
            role: row.role,
        }))
    }

    async fn find_session(&self, hash: &[u8], now: i64) -> AppResult<Option<AuthSessionRow>> {
        let row = self
            .db
            .query_one_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "SELECT id, csrf_token, role, expires_at, absolute_expires_at, last_rotated_at
                 FROM auth_sessions
                 WHERE revoked_at IS NULL
                   AND (token_hash = ? OR (previous_token_hash = ? AND previous_valid_until > ?))
                 LIMIT 1"
                    .to_string(),
                [
                    bytes_value(hash.to_vec()),
                    bytes_value(hash.to_vec()),
                    sea_orm::Value::from(now),
                ],
            ))
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(AuthSessionRow {
            id: row.try_get("", "id")?,
            csrf_token: row.try_get("", "csrf_token")?,
            role: SessionRole::from_db(&row.try_get::<String>("", "role")?),
            expires_at: row.try_get("", "expires_at")?,
            absolute_expires_at: row.try_get("", "absolute_expires_at")?,
            last_rotated_at: row.try_get("", "last_rotated_at")?,
        }))
    }

    async fn session_lock(&self, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.authentication_locks.lock().await;
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        if locks.len() >= MAX_AUTHENTICATION_LOCKS {
            if let Some(key) = locks.keys().next().cloned() {
                locks.remove(&key);
            }
        }
        locks
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn list_sessions(&self) -> AppResult<Vec<SessionSummary>> {
        let rows = self
            .db
            .query_all_raw(Statement::from_string(
                self.db.get_database_backend(),
                "SELECT id, device_name, created_at, last_used_at, last_ip, expires_at, role
                 FROM auth_sessions
                 WHERE revoked_at IS NULL
                 ORDER BY last_used_at DESC"
                    .to_string(),
            ))
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(SessionSummary {
                    id: row.try_get("", "id")?,
                    device_name: row.try_get("", "device_name")?,
                    created_at: row.try_get("", "created_at")?,
                    last_used_at: row.try_get("", "last_used_at")?,
                    last_ip: row.try_get("", "last_ip")?,
                    expires_at: row.try_get("", "expires_at")?,
                    role: SessionRole::from_db(&row.try_get::<String>("", "role")?),
                })
            })
            .collect()
    }

    pub async fn revoke(&self, id: &str) -> AppResult<u64> {
        let now = Utc::now().timestamp();
        let result = if id == "all" {
            self.db
                .execute_raw(Statement::from_sql_and_values(
                    self.db.get_database_backend(),
                    "UPDATE auth_sessions SET revoked_at = ? WHERE revoked_at IS NULL".to_string(),
                    [sea_orm::Value::from(now)],
                ))
                .await?
        } else {
            self.db
                .execute_raw(Statement::from_sql_and_values(
                    self.db.get_database_backend(),
                    "UPDATE auth_sessions SET revoked_at = ?
                     WHERE id = ? AND revoked_at IS NULL"
                        .to_string(),
                    [sea_orm::Value::from(now), sea_orm::Value::from(id)],
                ))
                .await?
        };
        Ok(result.rows_affected())
    }

    async fn create_session(
        &self,
        device_name: &str,
        ip: IpAddr,
        user_agent: &str,
        role: SessionRole,
    ) -> AppResult<String> {
        let now = Utc::now().timestamp();
        let token = random_token();
        let id = uuid::Uuid::new_v4().to_string();
        let csrf = random_token();
        let device = summarize_device(device_name, user_agent);
        self.db
            .execute_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "INSERT INTO auth_sessions (
                   id, token_hash, csrf_token, device_name, created_at, expires_at,
                   absolute_expires_at, last_used_at, last_rotated_at, last_ip,
                   user_agent_summary, role
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                    .to_string(),
                [
                    sea_orm::Value::from(id),
                    bytes_value(token_hash(&token)),
                    sea_orm::Value::from(csrf),
                    sea_orm::Value::from(device),
                    sea_orm::Value::from(now),
                    sea_orm::Value::from(now + SESSION_IDLE_SECONDS),
                    sea_orm::Value::from(now + SESSION_ABSOLUTE_SECONDS),
                    sea_orm::Value::from(now),
                    sea_orm::Value::from(now),
                    sea_orm::Value::from(ip.to_string()),
                    sea_orm::Value::from(summarize_user_agent(user_agent)),
                    sea_orm::Value::from(role.as_db()),
                ],
            ))
            .await?;
        Ok(token)
    }

    async fn check_attempt_allowed(&self, ip: IpAddr, now: i64) -> AppResult<()> {
        // 检查每 IP 登录速率限制（每分钟最多 LOGIN_RATE_LIMIT_PER_IP 次）
        {
            let mut login_attempts = self.login_attempts.lock().await;
            let timestamps = login_attempts.entry(ip).or_default();
            while timestamps
                .front()
                .is_some_and(|timestamp| *timestamp <= now - 60)
            {
                timestamps.pop_front();
            }
            if timestamps.len() >= LOGIN_RATE_LIMIT_PER_IP {
                return Err(AppError::Conflict(
                    "该 IP 登录尝试过于频繁（每 IP 每分钟限 5 次），请稍后重试".to_string(),
                ));
            }
            timestamps.push_back(now);
        }

        // 检查全局速率限制（每分钟最多 LOGIN_RATE_LIMIT_GLOBAL 次）
        let mut attempts = self.attempts.lock().await;
        while attempts
            .global
            .front()
            .is_some_and(|timestamp| *timestamp <= now - 60)
        {
            attempts.global.pop_front();
        }
        if attempts.global.len() >= self.login_rate_limit_global {
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
        attempts.global.push_back(now);
        Ok(())
    }

    async fn record_failure(&self, ip: IpAddr, now: i64) {
        let mut attempts = self.attempts.lock().await;
        let failed = attempts.per_ip.entry(ip).or_default();
        failed.failures = failed.failures.saturating_add(1);
        if failed.failures >= 5 {
            let delay = 1i64
                .checked_shl((failed.failures - 5).min(6))
                .unwrap_or(60)
                .min(60);
            failed.blocked_until = now + delay;
        }
    }

    fn ip_is_cn(&self, ip: IpAddr) -> AppResult<bool> {
        let reader =
            self.geo_reader.as_ref().as_ref().ok_or_else(|| {
                AppError::Unauthorized("GeoIP 数据不可用，已拒绝新配对".to_string())
            })?;
        let country: maxminddb::geoip2::Country = reader
            .lookup(ip)
            .map_err(|_| AppError::Unauthorized("无法判断网络区域，已拒绝新配对".to_string()))?;
        Ok(country
            .country
            .and_then(|value| value.iso_code)
            .is_some_and(|code| code == "CN"))
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

fn generate_code() -> String {
    let mut rng = rand::rng();
    (0..8)
        .map(|_| CODE_ALPHABET[rng.random_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn bytes_value(value: Vec<u8>) -> sea_orm::Value {
    sea_orm::Value::Bytes(Some(value))
}

fn summarize_device(requested: &str, user_agent: &str) -> String {
    let trimmed = requested.trim();
    if !trimmed.is_empty() {
        return safe_truncate(trimmed, 80);
    }
    if user_agent.contains("Windows") {
        "浏览器 / Windows".to_string()
    } else if user_agent.contains("Android") {
        "浏览器 / Android".to_string()
    } else if user_agent.contains("Linux") {
        "浏览器 / Linux".to_string()
    } else {
        "浏览器设备".to_string()
    }
}

fn summarize_user_agent(user_agent: &str) -> String {
    safe_truncate(user_agent, 160)
}

/// 在字符边界处安全截断，避免切到组合字符中间。
/// `max_chars` 为最多保留的字符数（按 Unicode 标量值计数）。
fn safe_truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut end = 0;
    for (index, (byte_offset, _)) in input.char_indices().enumerate() {
        if index == max_chars {
            end = byte_offset;
            break;
        }
    }
    input[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_uses_unambiguous_alphabet() {
        for _ in 0..100 {
            let code = generate_code();
            assert_eq!(code.len(), 8);
            assert!(code.bytes().all(|byte| CODE_ALPHABET.contains(&byte)));
        }
    }

    #[test]
    fn tokens_are_256_bits_and_url_safe() {
        let token = random_token();
        assert_eq!(URL_SAFE_NO_PAD.decode(token).expect("decode").len(), 32);
    }

    #[test]
    fn legacy_or_invalid_role_is_owner_but_delegated_roles_round_trip() {
        assert_eq!(SessionRole::from_db("owner"), SessionRole::Owner);
        assert_eq!(SessionRole::from_db("operator"), SessionRole::Operator);
        assert_eq!(SessionRole::from_db("viewer"), SessionRole::Viewer);
        assert_eq!(SessionRole::from_db("unexpected"), SessionRole::Owner);
        assert!(SessionRole::Owner.can_operate());
        assert!(SessionRole::Operator.can_operate());
        assert!(!SessionRole::Viewer.can_operate());
    }
}
