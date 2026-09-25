//! 会话生命周期：token 认证与滑动过期、24h 周期轮换（旧 token 120s 宽限）、
//! 会话列表与撤销、设备名/UA 摘要，以及会话创建（配对成功后调用）。

use super::{bytes_value, token_hash, AuthService, AuthSessionRow, SessionAuth, SessionSummary};
use crate::error::AppResult;
use chrono::Utc;
use sea_orm::{ConnectionTrait, Statement};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

/// LAN 模式下会话空闲 TTL：明文 HTTP 暴露面更大，收紧到 7 天。
fn session_idle_seconds(service: &AuthService) -> i64 {
    if service.security.current().mode == crate::services::security_config::AccessMode::Lan {
        super::LAN_SESSION_IDLE_SECONDS
    } else {
        super::SESSION_IDLE_SECONDS
    }
}

impl AuthService {
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
        let next_expiry = (now + session_idle_seconds(self)).min(row.absolute_expires_at);
        let rotated_token = if now - row.last_rotated_at >= super::ROTATE_AFTER_SECONDS {
            Some(super::random_token())
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
                    sea_orm::Value::from(now + super::PREVIOUS_TOKEN_GRACE_SECONDS),
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
            role: super::SessionRole::from_db(&row.try_get::<String>("", "role")?),
            expires_at: row.try_get("", "expires_at")?,
            absolute_expires_at: row.try_get("", "absolute_expires_at")?,
            last_rotated_at: row.try_get("", "last_rotated_at")?,
        }))
    }

    async fn session_lock(&self, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.authentication_locks.lock().await;
        if locks.len() >= super::MAX_AUTHENTICATION_LOCKS {
            // Only evict entries held solely by the map. An active lock must stay
            // in place or concurrent authentication for the same session can race.
            let idle_key = locks
                .iter()
                .find(|(_, lock)| Arc::strong_count(lock) == 1)
                .map(|(key, _)| key.clone());
            if let Some(key) = idle_key {
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
                    role: super::SessionRole::from_db(&row.try_get::<String>("", "role")?),
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

    /// 配对成功后创建会话；供 `pairing` 模块调用。
    pub(super) async fn create_session(
        &self,
        device_name: &str,
        ip: IpAddr,
        user_agent: &str,
        role: super::SessionRole,
    ) -> AppResult<String> {
        let now = Utc::now().timestamp();
        let token = super::random_token();
        let id = uuid::Uuid::new_v4().to_string();
        let csrf = super::random_token();
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
                    sea_orm::Value::from(now + session_idle_seconds(self)),
                    sea_orm::Value::from(now + super::SESSION_ABSOLUTE_SECONDS),
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
}

fn summarize_device(requested: &str, user_agent: &str) -> String {
    let trimmed = requested.trim();
    if !trimmed.is_empty() {
        return super::safe_truncate(trimmed, 80);
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
    super::safe_truncate(user_agent, 160)
}

#[cfg(test)]
mod tests {
    use super::super::{random_token, token_hash};
    use base64::Engine;
    use sha2::Digest;

    #[test]
    fn tokens_are_256_bits_and_url_safe() {
        let token = random_token();
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(token)
                .expect("decode")
                .len(),
            32
        );
    }

    #[test]
    fn token_hash_is_sha256_of_bytes() {
        assert_eq!(token_hash("a"), sha2::Sha256::digest(b"a").to_vec());
    }
}
