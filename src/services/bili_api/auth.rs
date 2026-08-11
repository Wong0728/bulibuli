//! 登录态与扫码登录：nav 信息、二维码生成与轮询（全程强类型）。

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::debug;

use super::models::auth::{NavData, NavStatus, QrcodeGenerate, QrcodePoll, QrcodePollData};
use super::BiliApi;

impl BiliApi {
    /// 获取当前登录态信息（头像/昵称/大会员/等级），基于 /x/web-interface/nav。
    /// 未登录时返回 `NavStatus { is_login: false, .. }`（B 站对未登录 Cookie 常返回 -101，
    /// 由统一入口 classify 转成 Err；调用方据此判定）。
    pub async fn get_nav_info(&self, cookies: &str) -> Result<NavStatus> {
        let enriched = self
            .enrich_cookies(cookies)
            .await
            .context("get_nav_info: 设备指纹富化失败，请检查 CookieManager 初始化")?;
        let url = "https://api.bilibili.com/x/web-interface/nav";
        let mut params = HashMap::new();
        self.inject_risk_params(&mut params, "333.999");
        let referer = "https://www.bilibili.com/";
        let request = self
            .build_get_request(url, &params, referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        debug!(url, "B站 API 请求: get_nav_info");
        let data: NavData = self.parse_data(resp, "get_nav_info").await?;
        if !data.is_login {
            return Ok(NavStatus::default());
        }
        Ok(NavStatus::from(data))
    }

    pub async fn get_qrcode_url(&self) -> Result<QrcodeGenerate> {
        // enrich 失败时显式报错，避免静默退化为空 Cookie 导致风控。
        let enriched = self
            .enrich_cookies("")
            .await
            .context("get_qrcode_url: 设备指纹富化失败")?;
        let url = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
        let mut params = HashMap::new();
        self.inject_risk_params(&mut params, "333.999");
        let request = self
            .build_get_request(
                url,
                &params,
                "https://passport.bilibili.com/login",
                &enriched,
            )
            .await;
        let resp = self.send_with_retry(request).await?;
        debug!(url, "B站 API 请求: get_qrcode_url");
        let result = self
            .parse_data::<QrcodeGenerate>(resp, "get_qrcode_url")
            .await?;
        if !result.qrcode_key.is_empty() {
            self.qrcode_sessions
                .lock()
                .await
                .insert(result.qrcode_key.clone(), (HashMap::new(), Instant::now()));
        }
        Ok(result)
    }

    pub async fn check_qrcode_status(&self, qrcode_key: &str) -> Result<QrcodePoll> {
        // enrich 失败时显式报错
        let enriched = self
            .enrich_cookies("")
            .await
            .context("check_qrcode_status: 设备指纹富化失败")?;
        let url = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
        let mut params = HashMap::new();
        params.insert("qrcode_key".to_string(), qrcode_key.to_string());
        self.inject_risk_params(&mut params, "333.999");
        let request = self
            .build_get_request(
                url,
                &params,
                "https://passport.bilibili.com/login",
                &enriched,
            )
            .await;
        let resp = self.send_with_retry(request).await?;
        debug!(url, qrcode_key, "B站 API 请求: check_qrcode_status");
        // 注意：必须先收集 Cookie，再读取 body，否则可能丢失。
        let response_cookies: HashMap<String, String> = resp
            .cookies()
            .map(|c| (c.name().to_string(), c.value().to_string()))
            .collect();
        // 外层 envelope code 已由 parse_data 保证为 0；poll.code 才是扫码轮询状态
        let poll: QrcodePollData = self.parse_data(resp, "check_qrcode_status").await?;
        let mut result = QrcodePoll {
            code: poll.code,
            message: poll.message,
            cookies: None,
        };
        let mut sessions = self.qrcode_sessions.lock().await;
        if sessions
            .get(qrcode_key)
            .is_some_and(|(_, created_at)| created_at.elapsed() >= Duration::from_secs(5 * 60))
        {
            sessions.remove(qrcode_key);
        }
        let entry = sessions
            .entry(qrcode_key.to_string())
            .or_insert_with(|| (HashMap::new(), Instant::now()));
        if entry.1.elapsed() >= Duration::from_secs(5 * 60) {
            entry.0.clear();
            entry.1 = Instant::now();
        }
        entry.0.extend(response_cookies);
        if poll.code == 0 {
            if entry.0.contains_key("SESSDATA") {
                let cookie_str = entry
                    .0
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                result.cookies = Some(cookie_str);
                sessions.remove(qrcode_key);
            } else {
                result.code = -1;
                result.message = "登录成功但未获取到有效 Cookies".to_string();
            }
        } else if matches!(poll.code, 86038 | 86039) {
            sessions.remove(qrcode_key);
        }
        Ok(result)
    }
}
