// B站设备指纹 cookie 管理。
//
// 参考 Bili23-Downloader 的 CookieManager 实现：
// 1. 本地生成 _uuid / b_lsid / b_nut / buvid_fp（见 `fingerprint`）
// 2. 在线获取 buvid3 / buvid4（GET /x/frontend/finger/spi，见 `device_api`）
// 3. 在线获取 bili_ticket（POST /bapis/bilibili.api.ticket.v1.Ticket/GenWebTicket，HMAC-SHA256 签名）
// 4. 激活 buvid3（POST /x/internal/gaia-gateway/ExClimbWuzhi，固定设备指纹 payload）
// 5. 注入 CURRENT_FNVAL=4048 / CURRENT_QUALITY=0
// 6. 与用户登录 cookie 合并后返回（缓存与持久化见 `lifecycle`）

mod device_api;
mod fingerprint;
mod lifecycle;

use crate::services::secret_store::SecretStore;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// 内存缓存有效期：1 小时。到期后重新校验 DB 中的过期时间。
const CACHE_TTL: Duration = Duration::from_secs(3600);
/// buvid3 提前刷新窗口：到期前 1 小时即视为过期，避免边界请求被风控
const BUVID_REFRESH_LEAD: i64 = 3600;
/// DB 中 device_cookies 键名
const SECRET_KEY: &str = "bili_device_cookies";

/// 持久化到 DB 的设备 cookie 状态（不含合成结果，运行时合并用户 cookie）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCookies {
    pub buvid3: String,
    pub buvid4: String,
    pub buvid_expires: i64,
    pub bili_ticket: String,
    pub bili_ticket_expires: i64,
    pub uuid: String,
    pub b_lsid: String,
    pub b_nut: i64,
    pub buvid_fp: String,
}

/// 内存缓存条目。
#[derive(Debug, Clone)]
struct CacheEntry {
    device: DeviceCookies,
    fetched_at: Instant,
}

#[derive(Clone)]
pub struct CookieManager {
    secret_store: Arc<SecretStore>,
    client: reqwest::Client,
    user_agent: String,
    referer: String,
    cache: Arc<RwLock<Option<CacheEntry>>>,
    /// 用于串行化 init 流程，避免并发首次调用重复初始化
    init_lock: Arc<tokio::sync::Mutex<()>>,
}

impl CookieManager {
    pub fn new(
        secret_store: Arc<SecretStore>,
        user_agent: String,
        referer: String,
        _tls_verify: bool,
        api_timeout: u64,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(api_timeout))
            .build()
            .context("创建 CookieManager HTTP 客户端失败")?;
        Ok(Self {
            secret_store,
            client,
            user_agent,
            referer,
            cache: Arc::new(RwLock::new(None)),
            init_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// 主入口：返回合并了设备指纹 + 用户登录态的 cookie 字符串。
    /// 内部带 DB 持久化 + 内存缓存，过期才触发 init。
    pub async fn enrich(&self, user_cookies: &str) -> Result<String> {
        let device = self.get_or_init().await?;
        Ok(Self::merge_cookies(&device, user_cookies))
    }
}
