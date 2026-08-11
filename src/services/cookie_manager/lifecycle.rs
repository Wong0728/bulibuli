//! 设备 Cookie 生命周期：内存缓存 → DB 缓存 → 完整初始化，以及 DB 持久化。

use anyhow::{Context, Result};
use chrono::Local;
use std::time::Instant;
use tracing::{debug, info, warn};

use super::{CacheEntry, CookieManager, DeviceCookies, BUVID_REFRESH_LEAD, CACHE_TTL, SECRET_KEY};

impl CookieManager {
    pub(super) async fn get_or_init(&self) -> Result<DeviceCookies> {
        // 1. 内存缓存命中？
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.as_ref() {
                if entry.fetched_at.elapsed() < CACHE_TTL && !self.is_expired(&entry.device) {
                    return Ok(entry.device.clone());
                }
            }
        }

        // 2. DB 命中且未过期？
        if let Some(d) = self.load_from_db().await? {
            if !self.is_expired(&d) {
                debug!("CookieManager: 命中 DB 缓存");
                let mut cache = self.cache.write().await;
                *cache = Some(CacheEntry {
                    device: d.clone(),
                    fetched_at: Instant::now(),
                });
                return Ok(d);
            }
        }

        // 3. 触发完整 init（串行化，避免并发重复）
        let _guard = self.init_lock.lock().await;
        // 双检：持锁后可能已被其他任务初始化
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.as_ref() {
                if entry.fetched_at.elapsed() < CACHE_TTL && !self.is_expired(&entry.device) {
                    return Ok(entry.device.clone());
                }
            }
        }
        if let Some(d) = self.load_from_db().await? {
            if !self.is_expired(&d) {
                let mut cache = self.cache.write().await;
                *cache = Some(CacheEntry {
                    device: d.clone(),
                    fetched_at: Instant::now(),
                });
                return Ok(d);
            }
        }

        info!("CookieManager: 初始化设备指纹");
        let device = self.init_cookie_info().await?;
        let mut cache = self.cache.write().await;
        *cache = Some(CacheEntry {
            device: device.clone(),
            fetched_at: Instant::now(),
        });
        Ok(device)
    }

    fn is_expired(&self, d: &DeviceCookies) -> bool {
        let now = Local::now().timestamp();
        now >= (d.buvid_expires - BUVID_REFRESH_LEAD)
            || now >= (d.bili_ticket_expires - BUVID_REFRESH_LEAD)
    }

    /// 完整初始化流程：buvid → bili_ticket → ExClimbWuzhi。
    /// 每步成功后立即持久化，断电可恢复。
    async fn init_cookie_info(&self) -> Result<DeviceCookies> {
        // 尝试加载已有状态（部分初始化的情况）
        let mut device = self
            .load_from_db()
            .await?
            .unwrap_or_else(|| self.fresh_skeleton());

        // 1. buvid3/buvid4 + 联动生成的本地 Cookie。
        let now = Local::now().timestamp();
        if device.buvid3.is_empty() || now >= (device.buvid_expires - BUVID_REFRESH_LEAD) {
            let (b3, b4) = self.get_buvid().await?;
            device.buvid3 = b3;
            device.buvid4 = b4;
            device.buvid_expires = now + 30 * 24 * 3600; // 30 天
                                                         // 联动生成
            device.uuid = Self::gen_uuid();
            device.b_lsid = Self::gen_b_lsid();
            device.b_nut = now;
            device.buvid_fp = Self::gen_buvid_fp(&self.user_agent);
            self.persist(&device).await?;
            // 立即激活 buvid3（必须，否则风控判定冷设备）
            if let Err(e) = self.exclimbwuzhi(&device).await {
                warn!("CookieManager: ExClimbWuzhi 失败（不阻塞，将在下次 enrich 重试）: {e}");
            }
        }

        // 2. bili_ticket
        if device.bili_ticket.is_empty() || now >= (device.bili_ticket_expires - BUVID_REFRESH_LEAD)
        {
            let (ticket, expires) = self.get_bili_ticket().await?;
            device.bili_ticket = ticket;
            device.bili_ticket_expires = expires;
            self.persist(&device).await?;
        }

        info!(
            buvid3_len = device.buvid3.len(),
            "CookieManager: 设备指纹初始化完成"
        );
        Ok(device)
    }

    /// 生成一个空骨架（仅在 DB 无任何记录时使用）。
    fn fresh_skeleton(&self) -> DeviceCookies {
        DeviceCookies {
            buvid3: String::new(),
            buvid4: String::new(),
            buvid_expires: 0,
            bili_ticket: String::new(),
            bili_ticket_expires: 0,
            uuid: String::new(),
            b_lsid: String::new(),
            b_nut: 0,
            buvid_fp: String::new(),
        }
    }

    // --- DB 持久化 ---

    async fn load_from_db(&self) -> Result<Option<DeviceCookies>> {
        match self.secret_store.get(SECRET_KEY).await? {
            Some(v) if !v.is_empty() => {
                let d: DeviceCookies =
                    serde_json::from_str(&v).context("解析 device_cookies 失败")?;
                Ok(Some(d))
            }
            _ => Ok(None),
        }
    }

    async fn persist(&self, device: &DeviceCookies) -> Result<()> {
        let value = serde_json::to_string(device)?;
        self.secret_store.set(SECRET_KEY, &value).await?;
        Ok(())
    }
}
