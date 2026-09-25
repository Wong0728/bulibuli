//! 直播播放地址：新版 `getRoomPlayInfo`（v2）与旧版 `playUrl`（legacy）的
//! 请求解析与回退策略。新版失败仅限端点/契约原因时才回退旧版。

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tracing::{debug, warn};

use crate::services::bili_api::models::live::{LivePlayUrl, LiveRoomPlayInfo};
use crate::services::bili_api::BiliApi;

use super::stream_url::{
    flatten_play_info, is_usable_live_url, schema_error, validate_live_play_info, LiveSchemaError,
};

/// 直播清晰度代码：原画（1080P）。
const LIVE_QN_RAW: i32 = 10000;

impl BiliApi {
    /// 获取直播流地址（旧版 `playUrl`）。
    ///
    /// 返回 FLV 流 URL 列表（多 CDN 线路），`order=1` 为主线。
    /// 默认请求原画清晰度（qn=10000），B 站自动降级到可用最高清晰度。
    ///
    /// **注意**：返回的 URL 有时效性（`expires` 参数），过期后需重新请求。
    pub async fn live_playurl(
        &self,
        room_id: i64,
        qn: Option<i32>,
        cookies: &str,
    ) -> Result<LivePlayUrl> {
        let primary_error = match self.live_playurl_v2(room_id, qn, cookies).await {
            Ok(data) if !data.durl.is_empty() => return Ok(data),
            Ok(_) => {
                debug!(
                    room_id,
                    "新版 getRoomPlayInfo 未返回流地址，回退旧版 playUrl"
                );
                Some(schema_error("新版 getRoomPlayInfo 未返回可用流地址"))
            }
            Err(error) => {
                if !should_fallback_live_playurl(&error) {
                    return Err(error);
                }
                warn!(room_id, %error, "新版 getRoomPlayInfo 失败，回退旧版 playUrl");
                Some(error)
            }
        };
        let fallback = self.live_playurl_legacy(room_id, qn, cookies).await;
        if let (Some(primary), Err(fallback_error)) = (&primary_error, &fallback) {
            let fallback_notified = fallback_error
                .downcast_ref::<crate::error::BiliApiError>()
                .is_some_and(|error| {
                    matches!(
                        error.kind,
                        crate::error::BiliErrorKind::RiskControl
                            | crate::error::BiliErrorKind::Unauthorized
                    )
                });
            if !fallback_notified {
                if let Some(error) = primary.downcast_ref::<crate::error::BiliApiError>() {
                    self.notify_bili_error(error, &serde_json::Value::Null)
                        .await;
                }
            }
        }
        fallback
    }

    async fn live_playurl_v2(
        &self,
        room_id: i64,
        qn: Option<i32>,
        cookies: &str,
    ) -> Result<LivePlayUrl> {
        let quality = qn.unwrap_or(LIVE_QN_RAW);
        let mut params = HashMap::new();
        params.insert("room_id".to_string(), room_id.to_string());
        params.insert("protocol".to_string(), "0,1".to_string());
        params.insert("format".to_string(), "0,1,2".to_string());
        params.insert("codec".to_string(), "0,1".to_string());
        params.insert("qn".to_string(), quality.to_string());
        params.insert("platform".to_string(), "web".to_string());
        params.insert("ptype".to_string(), "8".to_string());

        let url = "https://api.live.bilibili.com/xlive/web-room/v2/index/getRoomPlayInfo";
        let enriched = self.enrich_cookies(cookies).await?;
        let request = self
            .build_get_request(url, &params, "https://live.bilibili.com/", &enriched)
            .await;
        let response = self.send_with_retry(request).await?;
        let data = match self
            .parse_data_silent::<LiveRoomPlayInfo>(response, "live_get_room_play_info")
            .await
        {
            Ok(data) => data,
            Err(error) if is_live_playurl_schema_error(&error) => {
                return Err(schema_error(error.to_string()));
            }
            Err(error) => return Err(error),
        };
        validate_live_play_info(room_id, &data)?;
        if data.live_status != 1 {
            return Err(anyhow!("直播间 {room_id} 当前未开播"));
        }
        let durl = flatten_play_info(&data);
        if durl.is_empty() {
            return Err(schema_error("新版 getRoomPlayInfo 未返回可用流地址"));
        }
        let current_quality = durl
            .iter()
            .map(|url| url.current_qn)
            .max()
            .unwrap_or_default();
        let mut accept_quality = data
            .playurl_info
            .as_ref()
            .into_iter()
            .flat_map(|info| &info.playurl.stream)
            .flat_map(|stream| &stream.format)
            .flat_map(|format| &format.codec)
            .flat_map(|codec| codec.accept_qn.iter().copied())
            .collect::<Vec<_>>();
        accept_quality.sort_unstable();
        accept_quality.dedup();
        accept_quality.reverse();
        Ok(LivePlayUrl {
            current_quality,
            accept_quality: accept_quality
                .into_iter()
                .map(|qn| qn.to_string())
                .collect(),
            quality_description: Vec::new(),
            durl,
        })
    }

    async fn live_playurl_legacy(
        &self,
        room_id: i64,
        qn: Option<i32>,
        cookies: &str,
    ) -> Result<LivePlayUrl> {
        let quality = qn.unwrap_or(LIVE_QN_RAW);
        let mut params = HashMap::new();
        params.insert("cid".to_string(), room_id.to_string());
        params.insert("qn".to_string(), quality.to_string());
        params.insert("platform".to_string(), "web".to_string());

        let url = "https://api.live.bilibili.com/room/v1/Room/playUrl";
        let referer = "https://live.bilibili.com/";
        debug!(url, room_id, qn = quality, "B站直播 API 请求: playUrl");

        let enriched = self.enrich_cookies(cookies).await?;
        let request = self
            .build_get_request(url, &params, referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let mut data = self.parse_data::<LivePlayUrl>(resp, "live_playurl").await?;
        data.durl.retain(|stream| is_usable_live_url(&stream.url));
        for stream in &mut data.durl {
            stream.current_qn = data.current_quality;
            stream.protocol_name = "http_stream".to_string();
            stream.format_name = "flv".to_string();
        }

        if data.durl.is_empty() {
            return Err(anyhow!(
                "直播间 {room_id} 未返回任何流地址，可能未开播或直播间不可用"
            ));
        }

        Ok(data)
    }
}

/// 新版播放接口失败是否应当回退旧版：仅限端点不存在/契约不兼容，
/// 风控、登录失效等信号必须原样上抛，不能靠旧版接口二次探测。
fn should_fallback_live_playurl(error: &anyhow::Error) -> bool {
    if error.downcast_ref::<LiveSchemaError>().is_some() {
        return true;
    }
    if let Some(api_error) = error.downcast_ref::<crate::error::BiliApiError>() {
        return matches!(
            api_error.kind,
            crate::error::BiliErrorKind::NotFound | crate::error::BiliErrorKind::InvalidResponse
        );
    }
    error.to_string().contains("HTTP 404")
}

/// 判断解析错误是否属于响应契约漂移（用于把解析失败归一为契约错误）。
fn is_live_playurl_schema_error(error: &anyhow::Error) -> bool {
    let detail = error.to_string();
    detail.contains("反序列化 B站 API live_get_room_play_info")
        || detail.contains("解析 B站 API live_get_room_play_info JSON")
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn fallback_only_accepts_endpoint_or_schema_failures() {
        assert!(should_fallback_live_playurl(&schema_error("schema")));
        assert!(should_fallback_live_playurl(&anyhow::Error::new(
            crate::error::BiliApiError::classify(-404, "missing")
        )));
        assert!(!should_fallback_live_playurl(&anyhow::anyhow!("HTTP 401")));
    }
}
