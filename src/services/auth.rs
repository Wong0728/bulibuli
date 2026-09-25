//! Web 会话认证：设备配对码换会话 token、token 轮换与撤销、会话角色，
//! 以及登录/配对的速率限制与失败退避。
//!
//! 子模块分工：`pairing` 配对窗口与配对握手（含 GeoIP 区域判定），
//! `session` 会话 token 的认证、轮换、查询与撤销，`rate_limit` 每 IP/全局限流。

use crate::services::security_config::{can_open_directory, AccessMode, SecurityConfigService};
use base64::Engine;
use rand::{Rng, RngCore};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

const PAIR_TTL_SECONDS: i64 = 600;
const SESSION_IDLE_SECONDS: i64 = 30 * 24 * 60 * 60;
/// LAN 模式空闲 TTL：HTTP 明文传输下会话 Cookie 被嗅探的窗口应尽量短，
/// 从 30 天收紧到 7 天（绝对上限 90 天不变）。
const LAN_SESSION_IDLE_SECONDS: i64 = 7 * 24 * 60 * 60;
const SESSION_ABSOLUTE_SECONDS: i64 = 90 * 24 * 60 * 60;
const ROTATE_AFTER_SECONDS: i64 = 24 * 60 * 60;
const PREVIOUS_TOKEN_GRACE_SECONDS: i64 = 120;
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
/// 每 IP 每分钟最大**失败**登录/配对尝试次数（成功配对不占额度）。
const LOGIN_RATE_LIMIT_PER_IP: usize = 5;
/// `/api/auth/state` 未认证探测限流：每 IP 每分钟上限。单个 PairView 5s 轮询 ≈ 12 次/分钟，
/// 主端口与 Setup 端口各开一个页签就是 24 次/分钟，20 会把正常用户挡在门外；
/// 放宽到 40（约 3 个页签）仍能拦下快速枚举配对窗口的脚本。
const STATE_PROBE_LIMIT_PER_IP: usize = 40;
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
    /// 每 IP `/api/auth/state` 未认证探测时间戳（抑制远程探测配对窗口）
    state_probes: Arc<Mutex<HashMap<IpAddr, VecDeque<i64>>>>,
    /// 全局每分钟最大登录/配对尝试次数（由 AppConfig 注入）
    login_rate_limit_global: usize,
}

struct PairWindow {
    code: String,
    expires_at: i64,
    role: SessionRole,
}

/// 一次性配对码的判定结果：「窗口未开放/已过期」是环境状态，「码不匹配」是凭证错误，
/// 两者的日志原因与失败计数处理不同。
enum PairAttempt {
    Granted(SessionRole),
    WindowUnavailable,
    CodeMismatch,
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
    last_seen: i64,
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
            "owner" => Self::Owner,
            "operator" => Self::Operator,
            "viewer" => Self::Viewer,
            // Corrupt or future role values must never gain the highest privilege.
            _ => Self::Viewer,
        }
    }

    /// 会话角色在数据库与 WebSocket 房间名里使用的稳定标识。
    pub(crate) fn as_db(self) -> &'static str {
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
    /// `ip` 是否真实代表发起请求的客户端。
    ///
    /// Local/Lan 模式下 TCP 对端就是客户端，恒为真。Proxy 模式只绑回环，对端永远是
    /// 本机反向代理：只有显式信任 `x-forwarded-for`（BILI__TRUST_PROXY_HEADERS）时
    /// 拿到的才是真实客户端地址，否则 `ip` 是反代的 127.0.0.1，不能用于「与服务同机」判定。
    pub ip_is_client: bool,
}

impl ClientInfo {
    /// 「打开所在目录/显示绝对路径」能力判定，见 [`crate::services::security_config::can_open_directory`]。
    pub fn can_open_directory(&self, mode: &AccessMode) -> bool {
        can_open_directory(mode, self.ip, self.ip_is_client)
    }
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

mod pairing;
mod rate_limit;
mod session;

/// 在字符边界处安全截断，避免切到组合字符中间。
/// `max_chars` 为最多保留的字符数（按 Unicode 标量值计数）。
pub(super) fn safe_truncate(input: &str, max_chars: usize) -> String {
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

/// 记录一次失败尝试：封禁过期后的首个失败重置计数；连续失败达到 5 次后
/// 按指数退避封禁，封顶 1 小时（持续爆破者每小时最多获得一次尝试机会）。
fn apply_failure(failed: &mut FailedAttempts, now: i64) {
    if failed.blocked_until > 0 && failed.blocked_until <= now {
        failed.failures = 0;
        failed.blocked_until = 0;
    }
    failed.last_seen = now;
    failed.failures = failed.failures.saturating_add(1);
    if failed.failures >= 5 {
        let delay = 1i64
            .checked_shl((failed.failures - 5).min(12))
            .unwrap_or(3600)
            .min(3600);
        failed.blocked_until = now + delay;
    }
}

/// 每 IP 跟踪表容量上限：两处限流 map 的条目此前永不删除，
/// IPv6 源地址轮换可无限耗尽内存。
const MAX_TRACKED_IPS: usize = 10_000;

/// 总量达到上限时淘汰最久未活动的 IP 条目（近似 LRU；仅在超限时执行，
/// 全局限流约束下触发频率低，O(n) 扫描可接受）。
fn evict_stalest_ip<V>(map: &mut HashMap<IpAddr, V>, last_seen_of: impl Fn(&V) -> i64) {
    while map.len() >= MAX_TRACKED_IPS {
        let Some(stalest) = map
            .iter()
            .min_by_key(|(ip, value)| (last_seen_of(value), *ip))
            .map(|(ip, _)| *ip)
        else {
            return;
        };
        map.remove(&stalest);
    }
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn bytes_value(value: Vec<u8>) -> sea_orm::Value {
    sea_orm::Value::Bytes(Some(value))
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn generate_code() -> String {
    let mut rng = rand::rng();
    (0..8)
        .map(|_| CODE_ALPHABET[rng.random_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_roles_round_trip_and_invalid_roles_fail_closed() {
        assert_eq!(SessionRole::from_db("owner"), SessionRole::Owner);
        assert_eq!(SessionRole::from_db("operator"), SessionRole::Operator);
        assert_eq!(SessionRole::from_db("viewer"), SessionRole::Viewer);
        assert_eq!(SessionRole::from_db("unexpected"), SessionRole::Viewer);
        assert!(SessionRole::Owner.can_operate());
        assert!(SessionRole::Operator.can_operate());
        assert!(!SessionRole::Viewer.can_operate());
    }
}
