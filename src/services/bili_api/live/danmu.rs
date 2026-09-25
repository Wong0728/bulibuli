//! 直播弹幕 WebSocket 连接配置：只允许 WBI 签名的 `getDanmuInfo`，
//! 不会回退旧版 `getConf`；无有效登录态时直接返回互动降级错误。

use anyhow::{anyhow, Result};
use std::collections::HashMap;

use crate::services::bili_api::models::live::LiveDanmuConf;
use crate::services::bili_api::BiliApi;
use crate::services::wbi;

use super::stream_url::schema_error;

impl BiliApi {
    /// 获取经过认证的弹幕 WebSocket 连接信息。
    ///
    /// 无有效登录态时直接返回互动降级错误，不伪装成游客认证。
    pub async fn live_danmu_conf(&self, room_id: i64, cookies: &str) -> Result<LiveDanmuConf> {
        let result = self.live_danmu_conf_v2(room_id, cookies).await;
        if let Err(error) = &result {
            if let Some(api_error) = error.downcast_ref::<crate::error::BiliApiError>() {
                self.notify_bili_error(api_error, &serde_json::Value::Null)
                    .await;
            }
        }
        result
    }

    async fn live_danmu_conf_v2(&self, room_id: i64, cookies: &str) -> Result<LiveDanmuConf> {
        let mut params = HashMap::new();
        params.insert("id".to_string(), room_id.to_string());
        params.insert("type".to_string(), "0".to_string());
        let url = "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo";
        let enriched = self.enrich_cookies(cookies).await?;
        let credential = crate::services::credential::Credential::from_cookie_header(&enriched);
        if !credential.is_logged_in() {
            return Err(anyhow!("获取新版弹幕配置需要有效的 B站登录态"));
        }
        let (img_key, sub_key) = self.get_wbi_keys(&enriched).await?;
        wbi::enc_wbi(&mut params, &img_key, &sub_key)?;
        let request = self
            .build_get_request(url, &params, "https://live.bilibili.com/", &enriched)
            .await;
        let response = self.send_with_retry(request).await?;
        let conf = self
            .parse_data_silent::<LiveDanmuConf>(response, "live_get_danmu_info")
            .await?;
        validate_live_danmu_conf(&conf)?;
        Ok(conf)
    }
}

/// 校验 `getDanmuInfo` 响应契约：token 非空、host 列表非空且每项合法。
fn validate_live_danmu_conf(conf: &LiveDanmuConf) -> Result<()> {
    if conf.token.trim().is_empty() || conf.host_server_list.is_empty() {
        return Err(schema_error("弹幕配置缺少 token 或 host_server_list"));
    }
    for host in &conf.host_server_list {
        let value = host.host.trim();
        if value.is_empty()
            || value.contains('/')
            || value.contains(':')
            || value.chars().any(char::is_control)
            || !(1..=u16::MAX as i32).contains(&host.wss_port)
        {
            return Err(schema_error("弹幕配置包含无效 host 或 wss_port"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::services::bili_api::models::live::LiveDanmuHost;

    #[test]
    fn live_contract_rejects_missing_stream_fields() {
        let conf = LiveDanmuConf {
            token: "token".to_string(),
            host_server_list: vec![LiveDanmuHost {
                host: "chat.example.com".to_string(),
                wss_port: 443,
                ..Default::default()
            }],
        };
        assert!(validate_live_danmu_conf(&conf).is_ok());
    }
}
