//! B 站开放接口客户端。
//!
//! 子模块划分：
//! - `client`：请求管线（WBI 签名、风控参数、重试、响应解析）
//! - `user_space`：UP 主空间接口（投稿/合集/搜索/用户信息）
//! - `video_stream`：视频信息与音视频流解析（playurl/流选择）
//! - `auth`：登录态与扫码登录
//! - `pgc`：番剧（PGC）季/集信息与取流
//! - `cheese`：课程（Cheese / PUGV）季/集信息与取流
//! - `live`：直播间房间信息、流地址、弹幕连接配置（旧版接口，无需签名）

mod auth;
mod cheese;
mod client;
mod live;
pub mod models;
mod pgc;
mod user_space;
mod video_stream;

pub(crate) use video_stream::choose_video_stream;

use crate::config::AppConfig;
use crate::services::cdn_registry::BadCdnRegistry;
use crate::services::cookie_manager::CookieManager;
use crate::services::wbi::WbiKeysCache;
use crate::ws::WebSocketManager;
use anyhow::{Context, Result};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};

use self::models::{user::UserVideosPage, video::VideoInfo};

pub const QUALITY_NAMES: &[(i32, &str)] = &[
    (127, "8K 超高清"),
    (126, "杜比视界"),
    (125, "HDR 真彩"),
    (120, "4K 超清"),
    (116, "1080P60 高帧率"),
    (112, "1080P+ 高码率"),
    (80, "1080P 高清"),
    (74, "720P60 高帧率"),
    (64, "720P 高清"),
    (32, "480P 清晰"),
    (16, "360P 流畅"),
];

/// 视频列表缓存有效期（30 秒），避免短时间内对同一博主重复发起 HTTP 请求。
const VIDEO_LIST_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// 缓存 key：(uid, page, page_size)
type VideoListCacheKey = (String, i64, i32, i32);
type VideoInfoCacheKey = (String, String);
type QrCodeSession = (HashMap<String, String>, Instant);

#[derive(Clone)]
pub struct BiliApi {
    /// B 站 API 域名（api.bilibili.com / passport.bilibili.com 等）专用客户端，
    /// 始终严格校验 TLS，防止中间人窃取 Cookie/凭据。
    api_client: Client,
    /// 下载流 CDN 域名（*.bilivideo.com / *.hdslb.com）专用客户端，
    /// 始终严格校验 TLS；带 Cookie 的 CDN 请求不得关闭证书校验。
    stream_client: Client,
    /// 无 Cookie 的兼容客户端，仅用于公开 b23/资源解析；凭据请求不得使用它。
    anonymous_client: Client,
    config: Arc<AppConfig>,
    cookie_manager: Arc<CookieManager>,
    wbi_keys: WbiKeysCache,
    rate_limiter: Arc<governor::DefaultDirectRateLimiter>,
    ws: Arc<WebSocketManager>,
    bad_cdns: Arc<BadCdnRegistry>,
    /// 视频列表内存缓存：key = (UID, page, page_size)，value = (响应, 写入时刻)。
    video_list_cache: Arc<RwLock<HashMap<VideoListCacheKey, (UserVideosPage, Instant)>>>,
    video_info_cache: Arc<RwLock<HashMap<VideoInfoCacheKey, (VideoInfo, Instant)>>>,
    qrcode_sessions: Arc<Mutex<HashMap<String, QrCodeSession>>>,
}

impl BiliApi {
    pub fn new(
        config: Arc<AppConfig>,
        cookie_manager: Arc<CookieManager>,
        ws: Arc<WebSocketManager>,
    ) -> Result<Self> {
        let api_client = Client::builder()
            .cookie_store(true)
            .danger_accept_invalid_certs(false)
            .timeout(std::time::Duration::from_secs(config.bili_api_timeout))
            .build()
            .context("创建 B站 API HTTP 客户端失败")?;
        let stream_client = Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(false)
            .timeout(std::time::Duration::from_secs(config.bili_api_timeout))
            .build()
            .context("创建 B站流 CDN HTTP 客户端失败")?;
        let anonymous_client = Client::builder()
            .cookie_store(false)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(!config.tls_verify)
            .timeout(std::time::Duration::from_secs(config.bili_api_timeout))
            .build()
            .context("创建 B站匿名兼容 HTTP 客户端失败")?;
        Ok(Self {
            api_client,
            stream_client,
            anonymous_client,
            config,
            cookie_manager,
            ws,
            bad_cdns: Arc::new(BadCdnRegistry::default()),
            wbi_keys: WbiKeysCache::new(),
            rate_limiter: Arc::new(governor::RateLimiter::direct(
                governor::Quota::per_second(
                    NonZeroU32::new(5).expect("SAFETY: B站 API 限速常量固定为非零值"),
                )
                .allow_burst(
                    NonZeroU32::new(10).expect("SAFETY: B站 API 突发限额常量固定为非零值"),
                ),
            )),
            video_list_cache: Arc::new(RwLock::new(HashMap::new())),
            video_info_cache: Arc::new(RwLock::new(HashMap::new())),
            qrcode_sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 按 URL 所属域名选择合适的严格 TLS 客户端。
    pub fn client_for(&self, url: &str) -> &Client {
        if is_api_host(url) {
            &self.api_client
        } else {
            &self.stream_client
        }
    }

    /// 返回无 Cookie 的公开资源兼容客户端。
    /// 新代码请使用 `client_for(url)`，凭据请求不得使用此客户端。
    pub fn client(&self) -> &Client {
        &self.anonymous_client
    }

    pub(crate) async fn invalidate_session_caches(&self) {
        self.video_list_cache.write().await.clear();
        self.video_info_cache.write().await.clear();
        self.wbi_keys.invalidate().await;
    }

    /// 坏 CDN host 熔断注册表：与流选择共享同一实例，
    /// 记录传输层成败，使后续解析避开异常节点。
    pub(crate) fn bad_cdns(&self) -> &BadCdnRegistry {
        &self.bad_cdns
    }
}

pub(crate) fn session_fingerprint(cookies: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cookies.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 判断 URL 是否属于 B 站 API 域名（强制严格 TLS）。
/// 接受任意子域的 bilibili.com；非 API 域名（bilivideo.com / hdslb.com）走 stream_client。
fn is_api_host(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    host == "bilibili.com" || host.ends_with(".bilibili.com")
}

#[cfg(test)]
mod tests {
    use super::session_fingerprint;

    #[test]
    fn session_fingerprint_is_stable_and_one_way() {
        let first = session_fingerprint("SESSDATA=one");
        assert_eq!(first, session_fingerprint("SESSDATA=one"));
        assert_ne!(first, session_fingerprint("SESSDATA=two"));
        assert!(!first.contains("one"));
    }
}
