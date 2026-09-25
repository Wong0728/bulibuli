//! BiliApi 请求管线：WBI 签名获取、风控参数注入、Cookie 富化、
//! 统一 GET 构建、网络重试与 JSON 响应解析。
//!
//! 按职责拆分为兄弟模块：
//! - 本文件：发送前的请求准备——WBI keys 获取、风控参数注入、Cookie 富化
//! - [`request`]：统一 GET 构建、限流扣减、429/5xx/网络错误重试发送
//! - [`envelope`]：响应信封读取校验（HTTP 状态/Content-Type/code）与强类型反序列化
//! - [`notify`]：风控/登录失效系统事件的前端推送与节流

use crate::error::BiliApiError;
use anyhow::Result;
use std::collections::HashMap;

use super::BiliApi;

#[path = "client/envelope.rs"]
mod envelope;
#[path = "client/notify.rs"]
mod notify;
#[path = "client/request.rs"]
mod request;

impl BiliApi {
    /// 获取 WBI img_key/sub_key。
    ///
    /// `enriched_cookies` 必须是已经过 `enrich_cookies` 合并设备指纹后的 Cookie 字符串。
    /// `/x/web-interface/nav` 在新风控策略下要求携带登录态 Cookie，否则返回 -101。
    pub(super) async fn get_wbi_keys(&self, enriched_cookies: &str) -> Result<(String, String)> {
        // WBI keys 来自 api.bilibili.com，必须走严格 TLS 的 api_client。
        const NAV_URL: &str = "https://api.bilibili.com/x/web-interface/nav";
        let result = self
            .wbi_keys
            .get(|| async {
                // nav 请求复用统一请求管线（限流在发送处统一扣减、网络重试、
                // 超时与请求头均与其他 B站 API 一致），不再绕过重试直连。
                let params = HashMap::new();
                let request = self
                    .build_get_request(NAV_URL, &params, &self.config.referer, enriched_cookies)
                    .await;
                self.send_with_retry(request).await
            })
            .await;
        // WbiKeysCache 只负责分类与失效，拿不到 ws：登录态在签名环节失效（nav -101/-352）
        // 时前端收不到 bili:auth-expired，表现就是"什么都拉不到"却毫无提示。
        if let Some(error) = result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<BiliApiError>())
        {
            self.notify_bili_error(error, &serde_json::Value::Null)
                .await;
        }
        result
    }

    /// 公开 WBI keys 获取方法，供 DanmakuService 等复用。
    ///
    /// 调用方需先 `cookie_manager.enrich(...)` 合并设备指纹，再把结果传入。
    pub async fn get_wbi_keys_public(&self, enriched_cookies: &str) -> Result<(String, String)> {
        self.get_wbi_keys(enriched_cookies).await
    }

    /// 注入 B 站风控必需参数（与 Bili23-Downloader 一致）。
    /// 这些固定值是 B 站前端在 WebGL/Canvas 等环境采集后编码的"指纹"，几乎所有 wbi 接口都需要。
    /// `web_location` 由调用方在调用前设置（若已存在则不覆盖）。
    pub(super) fn inject_risk_params(
        &self,
        params: &mut HashMap<String, String>,
        web_location: &str,
    ) {
        // dm_img_str: WebGL 版本信息（base64）
        params
            .entry("dm_img_str".to_string())
            .or_insert_with(|| "V2ViR0wgMS4wIChPcGVuR0wgRVMgMi4wIENocm9taXVtKQ".to_string());
        // dm_cover_img_str: ANGLE 后端信息（base64）
        params.entry("dm_cover_img_str".to_string()).or_insert_with(|| {
            "QU5HTEUgKE5WSURJQSwgTlZJRElBIEdlRm9yY2UgUlRYIDQwNjAgTGFwdG9wIEdQVSAoMHgwMDAwMjhFMCkgRGlyZWN0M0QxMSB2c181XzAgcHNfNV8wLCBEM0QxMSlHb29nbGUgSW5jLiAoTlZJRElBKQ".to_string()
        });
        // dm_img_list: 空数组
        params
            .entry("dm_img_list".to_string())
            .or_insert_with(|| "[]".to_string());
        // dm_img_inter: 设备交互指纹（ds/wh/of 字段）
        params
            .entry("dm_img_inter".to_string())
            .or_insert_with(|| r#"{"ds":[],"wh":[5073,6031,29],"of":[206,412,206]}"#.to_string());
        // gaia_source: 来源标识
        params
            .entry("gaia_source".to_string())
            .or_insert_with(|| "web_main".to_string());
        // web_location: 页面位置（调用方指定）
        if !web_location.is_empty() {
            params
                .entry("web_location".to_string())
                .or_insert_with(|| web_location.to_string());
        }
    }

    /// 富化用户 Cookie：合并设备指纹 Cookie（buvid3/bili_ticket/...）。
    pub(super) async fn enrich_cookies(&self, user_cookies: &str) -> Result<String> {
        self.cookie_manager.enrich(user_cookies).await
    }

    /// 公开 Cookie 富化入口，供 DownloadManager.add_to_aria2 复用。
    /// 下载流（m4s/m4a）需要带设备指纹 Cookie 才能绕过 B 站风控（403/-799 等）。
    pub async fn enrich_cookies_public(&self, user_cookies: &str) -> Result<String> {
        self.enrich_cookies(user_cookies).await
    }
}
