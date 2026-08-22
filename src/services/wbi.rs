//! WBI 签名共享模块。
//!
//! 提取自 `BiliApi`，供 `BiliApi` 与 `DanmakuService` 共用，
//! 避免签名逻辑重复实现。

use anyhow::{anyhow, Context, Result};
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
    // 签名串与最终发送的 URL 查询串共用同一编码函数（build_query），
    // 保证「签名串 == 发送串」，避免 reqwest query() 的 `+` 编码导致校验失败。
    let query = build_query(&cleaned);
    let sign = format!("{:x}", md5::compute(query + &mixin));
    for (k, v) in &cleaned {
        params.insert(k.clone(), v.clone());
    }
    params.insert("w_rid".to_string(), sign);
    Ok(())
}

/// 把参数编码为查询串：按键排序，值与键均使用百分号编码（空格→`%20`）。
///
/// WBI 签名计算与请求 URL 构造**必须共用此函数**：
/// reqwest 的 `query()` 走 form-urlencoded 编码（空格→`+`），
/// 与签名的 `encodeURIComponent` 编码不一致，含空格关键词会签名校验失败。
pub fn build_query(params: &HashMap<String, String>) -> String {
    let mut pairs: Vec<(&String, &String)> = params.iter().collect();
    // 与官方实现一致：按原始 key 排序（非编码后的 key）。
    pairs.sort_by(|left, right| left.0.cmp(right.0));
    pairs
        .into_iter()
        .map(|(k, v)| format!("{}={}", encode_uri_component(k), encode_uri_component(v)))
        .collect::<Vec<_>>()
        .join("&")
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
    /// `send_request` 由调用方提供，应复用统一的请求管线
    /// （限流、网络重试、超时、请求头与 Cookie 注入），
    /// 避免 nav 刷新绕过重试管线：nav 抖动时直接失败会导致 WBI 签名连锁失败。
    /// B 站在新风控策略下，匿名/冷设备请求 `/x/web-interface/nav` 会直接
    /// 返回 -101「账号未登录」，闭包内必须携带登录态 Cookie。
    pub async fn get<F, Fut>(&self, send_request: F) -> Result<(String, String)>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<reqwest::Response>>,
    {
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
        debug!("WBI keys 请求: nav");
        // 发送走调用方提供的统一管线（含限流与网络重试），不再直连。
        let resp = send_request().await?;
        let status = resp.status();
        if !status.is_success() {
            warn!(status = %status, "WBI keys 请求返回非2xx");
            return Err(anyhow!("WBI keys请求返回HTTP {status}"));
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if !content_type.is_empty() && !content_type.to_ascii_lowercase().contains("json") {
            return Err(anyhow!(
                "WBI keys 响应 Content-Type 非有效 JSON: {content_type}"
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
                    warn!(content_type = %content_type, "WBI keys 响应JSON解析失败: {e}");
                    return Err(anyhow!(
                        "WBI keys响应解析失败: {e}，Content-Type: {content_type}"
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

    #[test]
    fn test_build_query_percent_encodes_space_and_cjk() {
        // B1 回归：查询串必须是百分号编码（与 encodeURIComponent 一致），
        // 空格→%20，绝不能出现 form-urlencoded 的 `+`。
        let mut params = HashMap::new();
        params.insert("keyword".to_string(), "三体 群星 (beta)".to_string());
        params.insert("foo".to_string(), "bar baz".to_string());
        let query = build_query(&params);
        assert!(!query.contains('+'), "查询串不得包含 `+`: {query}");
        assert!(query.contains("foo=bar%20baz"));
        // 中文按 UTF-8 百分号编码（"三" = E4 B8 89）
        assert!(query.contains("%E4%B8%89"));
        // 括号属于保留字符，必须编码
        assert!(query.contains("%28beta%29"));
    }

    #[test]
    fn test_enc_wbi_signs_exactly_the_sent_query() {
        // B1 回归：签名计算所用查询串必须与最终发送 URL 的查询串完全一致。
        // 发送侧（client.rs build_get_request）用同一 build_query 构造 URL，
        // 此处用 build_query 重建签名源串并复算 md5，验证 w_rid 与之吻合。
        let (img, sub) = (
            "7cd3e0c46f4154c7895abce07cd3e0c4".to_string(),
            "4932cab9b6eb0f2aa4e9c4ee4932cab9".to_string(),
        );
        let mut params = HashMap::new();
        // 覆盖含空格 + 中文 + 中英混排的搜索关键词
        params.insert("keyword".to_string(), "原神 启动 test".to_string());
        params.insert("search_type".to_string(), "video".to_string());
        enc_wbi(&mut params, &img, &sub).unwrap();

        // 模拟发送侧：用 build_query 构造的查询串不含 `+`
        let sent_query = build_query(&params);
        assert!(
            !sent_query.contains('+'),
            "发送串不得包含 `+`: {sent_query}"
        );
        assert!(sent_query.contains("%20"));

        // 复算签名：去掉 w_rid 后的参数按 build_query 编码 + mixin_key 的 md5
        let w_rid = params.get("w_rid").unwrap().clone();
        params.remove("w_rid");
        let sign_source = build_query(&params);
        let mixin = mixin_key(&(img.clone() + &sub)).unwrap();
        let expected = format!("{:x}", md5::compute(format!("{sign_source}{mixin}")));
        assert_eq!(w_rid, expected, "w_rid 必须基于与发送串一致的编码计算");
    }
}
