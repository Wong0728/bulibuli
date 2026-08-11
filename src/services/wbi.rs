//! WBI 签名共享模块。
//!
//! 提取自 `BiliApi`，供 `BiliApi` 与 `DanmakuService` 共用，
//! 避免签名逻辑重复实现。

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn};

/// WBI keys 缓存 30 分钟，避免频繁请求 nav。
pub const WBI_KEYS_CACHE_TTL: Duration = Duration::from_secs(30 * 60);

const MIXIN_KEY_ENC_TAB: &[usize] = &[
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 从 img_key+sub_key 派生 32 字符 mixin_key。
pub fn mixin_key(orig: &str) -> Result<String> {
    // 一次收集后按索引重排，避免重复遍历字符序列。
    let chars: Vec<char> = orig.chars().collect();
    MIXIN_KEY_ENC_TAB
        .iter()
        .take(32)
        .map(|&i| chars.get(i).copied().context("WBI 密钥长度不足"))
        .collect::<Result<String>>()
}

/// 对参数进行 WBI 签名。
///
/// # 副作用
/// 该函数会**就地修改**传入的 `params`：
/// 1. 注入当前 Unix 时间戳 `wts`；
/// 2. 过滤所有 value 中的 `!'()*` 特殊字符（与 Python 实现一致）；
/// 3. 注入最终计算出的 `w_rid` 签名。
///
/// 调用方传入的 `params` 在函数返回时已包含 `wts` / `w_rid`，可直接用于 HTTP 请求。
pub fn enc_wbi(params: &mut HashMap<String, String>, img_key: &str, sub_key: &str) -> Result<()> {
    let mixin = mixin_key(&(img_key.to_string() + sub_key))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    params.insert("wts".to_string(), now.to_string());
    // 清理特殊字符（与 Python filter(lambda chr: chr not in "!'()*") 一致）
    let cleaned: HashMap<String, String> = params
        .iter()
        .map(|(k, v)| {
            let cleaned: String = v.chars().filter(|c| !"!'()*".contains(*c)).collect();
            (k.clone(), cleaned)
        })
        .collect();
    let mut keys: Vec<&String> = cleaned.keys().collect();
    keys.sort();
    let query: Vec<String> = keys
        .iter()
        .map(|k| {
            format!(
                "{}={}",
                encode_uri_component(k),
                encode_uri_component(&cleaned[*k])
            )
        })
        .collect();
    let query = query.join("&");
    let sign = format!("{:x}", md5::compute(query.clone() + &mixin));
    for (k, v) in &cleaned {
        params.insert(k.clone(), v.clone());
    }
    params.insert("w_rid".to_string(), sign);
    Ok(())
}

/// 与官方 WBI（encodeURIComponent）一致：空格编码为 `%20`，
/// 除字母、数字、`-_.~` 外的字符均编码为 %XX。
pub fn encode_uri_component(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// 从 WBI 图片 URL 提取 key（如 https://.../abc.png → abc）。
pub fn extract_key(url: &str) -> Option<String> {
    let key = url.rsplit('/').next()?.split('.').next()?;
    if key.is_empty() {
        None
    } else {
        Some(key.to_string())
    }
}

/// WBI keys 缓存：避免每次签名都请求 nav。
#[derive(Clone)]
pub struct WbiKeysCache {
    inner: Arc<RwLock<Option<(String, String, Instant)>>>,
    refresh_lock: Arc<Mutex<()>>,
}

impl WbiKeysCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            refresh_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 获取 img_key/sub_key，带 TTL 缓存。
    ///
    /// `cookies` 应为已富化（含设备指纹）的 Cookie 字符串：B 站在新风控策略下的
    /// 匿名/冷设备请求 `/x/web-interface/nav` 会直接返回 -101「账号未登录」，
    /// 必须携带登录态 Cookie 才能拿到 wbi_img keys。
    pub async fn get(
        &self,
        client: &Client,
        user_agent: &str,
        referer: &str,
        cookies: &str,
    ) -> Result<(String, String)> {
        {
            let cache = self.inner.read().await;
            if let Some((img, sub, fetched_at)) = cache.as_ref() {
                if fetched_at.elapsed() < WBI_KEYS_CACHE_TTL {
                    return Ok((img.clone(), sub.clone()));
                }
            }
        }
        let _refresh = self.refresh_lock.lock().await;
        {
            let cache = self.inner.read().await;
            if let Some((img, sub, fetched_at)) = cache.as_ref() {
                if fetched_at.elapsed() < WBI_KEYS_CACHE_TTL {
                    return Ok((img.clone(), sub.clone()));
                }
            }
        }
        let url = "https://api.bilibili.com/x/web-interface/nav";
        debug!(url, "WBI keys 请求: nav");
        let mut req = client
            .get(url)
            .header("User-Agent", user_agent)
            .header("Referer", referer)
            .header("Origin", "https://www.bilibili.com")
            .header("Accept", "application/json, text/plain, */*");
        if !cookies.is_empty() {
            req = req.header("Cookie", cookies);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body_preview = resp.text().await.context("读取WBI keys错误响应体失败")?;
            let preview: String = body_preview.chars().take(500).collect();
            warn!(status = %status, "WBI keys 请求返回非2xx: {preview}");
            return Err(anyhow!("WBI keys请求返回HTTP {status}: {preview}"));
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if !content_type.is_empty() && !content_type.to_ascii_lowercase().contains("json") {
            return Err(anyhow!(
                "WBI keys 鍝嶅簲 Content-Type 闈炴湁鏁?JSON: {content_type}"
            ));
        }
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
            let chunk = chunk.context("读取WBI keys响应体失败")?;
            if body.len().saturating_add(chunk.len()) > 2 * 1024 * 1024 {
                return Err(anyhow!("WBI keys响应体超过 2 MiB 上限"));
            }
            body.extend_from_slice(&chunk);
        }
        let bytes = body;
        let data: crate::services::bili_api::models::auth::NavResponse =
            match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(500)]);
                    warn!(content_type = %content_type, "WBI keys 响应JSON解析失败: {e}，前500字节: {preview}");
                    return Err(anyhow!(
                        "WBI keys响应解析失败: {e}，Content-Type: {content_type}，前500字节: {preview}"
                    ));
                }
            };
        debug!(code = data.code, "WBI keys 响应: nav");
        if data.code != 0 {
            // -101/-352 必须分类，供前端区分登录失效与风控。
            let error = crate::error::BiliApiError::classify(data.code, data.message);
            self.invalidate().await;
            return Err(error.into());
        }
        let wbi = data
            .data
            .ok_or_else(|| anyhow!("响应中无 data 数据"))?
            .wbi_img;
        let img_key = extract_key(&wbi.img_url).context("img_key 为空或格式错误")?;
        let sub_key = extract_key(&wbi.sub_url).context("sub_key 为空或格式错误")?;
        let mut cache = self.inner.write().await;
        *cache = Some((img_key.clone(), sub_key.clone(), Instant::now()));
        Ok((img_key, sub_key))
    }

    pub async fn invalidate(&self) {
        *self.inner.write().await = None;
    }
}

impl Default for WbiKeysCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mixin_key_length() {
        // img_key + sub_key 通常为 64 个字符，mixin_key 取前 32 位。
        let orig = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mk = mixin_key(orig).unwrap();
        assert_eq!(mk.len(), 32);
    }

    #[test]
    fn test_extract_key() {
        assert_eq!(
            extract_key("https://example.com/abc.png"),
            Some("abc".to_string())
        );
        assert_eq!(
            extract_key("https://example.com/sub_key"),
            Some("sub_key".to_string())
        );
        assert_eq!(extract_key(""), None);
    }

    #[test]
    fn test_enc_wbi_injects_wts_and_w_rid() {
        // img_key+sub_key 需 ≥64 字符以供 mixin_key 取 32 位（MIXIN_KEY_ENC_TAB 最大索引 63）
        let (img, sub) = (
            "7cd3e0c46f4154c7895abce07cd3e0c4".to_string(),
            "4932cab9b6eb0f2aa4e9c4ee4932cab9".to_string(),
        );
        let mut params = HashMap::new();
        params.insert("foo".to_string(), "bar".to_string());
        enc_wbi(&mut params, &img, &sub).unwrap();
        assert!(params.contains_key("wts"));
        assert!(params.contains_key("w_rid"));
    }
}
