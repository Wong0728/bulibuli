//! 直播间 API 客户端：房间信息、流地址、弹幕连接配置。
//!
//! 播放地址保留旧版接口作为播放协议兼容路径；弹幕配置只允许
//! WBI 签名的 `getDanmuInfo`，不会回退到旧版 `getConf`。
//! 接口域名：`api.live.bilibili.com`，走 `api_client`（严格 TLS）。
/// Fetch authenticated WebSocket information from WBI-signed `getDanmuInfo`.
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tracing::{debug, warn};

use crate::services::wbi;

use super::models::live::{
    LiveBatchStatus, LiveBatchStatusMap, LiveDanmuConf, LivePlayUrl, LiveRoomInfo, LiveRoomInit,
    LiveRoomPlayInfo, LiveStreamUrl, LiveUrlInfo,
};
use super::BiliApi;

/// 直播清晰度代码：原画（1080P）。
const LIVE_QN_RAW: i32 = 10000;

impl BiliApi {
    /// Batch status probe used by the monitor. One request covers all saved
    /// anchors, preventing a synchronized per-room polling spike.
    pub async fn live_status_by_uids(
        &self,
        uids: &[i64],
        cookies: &str,
    ) -> Result<HashMap<i64, LiveBatchStatus>> {
        let uids = uids
            .iter()
            .copied()
            .filter(|uid| *uid > 0)
            .collect::<Vec<_>>();
        if uids.is_empty() {
            return Ok(HashMap::new());
        }
        let url = "https://api.live.bilibili.com/room/v1/Room/get_status_info_by_uids";
        let enriched = self.enrich_cookies(cookies).await?;
        self.rate_limiter.until_ready().await;
        let credential = crate::services::credential::Credential::from_cookie_header(&enriched);
        let request = self
            .client_for(url)
            .post(url)
            .json(&serde_json::json!({"uids": uids}))
            .header("User-Agent", &self.config.user_agent)
            .header("Referer", "https://live.bilibili.com/")
            .header("Origin", "https://www.bilibili.com")
            .header("Accept", "application/json, text/plain, */*")
            .header("Cookie", credential.to_cookie_header());
        let response = self.send_with_retry(request).await?;
        let data: LiveBatchStatusMap = self
            .parse_data_silent(response, "live_status_info_by_uids")
            .await?;
        Ok(data
            .into_values()
            .filter(|item| item.uid > 0)
            .map(|item| (item.uid, item))
            .collect())
    }
    /// Fetch the small recent-message window used to backfill reconnect gaps.
    pub async fn live_recent_danmaku(
        &self,
        room_id: i64,
        cookies: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let mut params = HashMap::new();
        params.insert("roomid".to_owned(), room_id.to_string());
        let enriched = self.enrich_cookies(cookies).await?;
        let request = self
            .build_get_request(
                "https://api.live.bilibili.com/xlive/web-room/v1/dM/gethistory",
                &params,
                "https://live.bilibili.com/",
                &enriched,
            )
            .await;
        let response = self.send_with_retry(request).await?;
        let data = self
            .parse_data_silent::<serde_json::Value>(response, "live_recent_danmaku")
            .await?;
        Ok(data
            .get("room")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(10)
            .collect())
    }

    /// 短号换长号 + 直播状态（`room_init`）。
    ///
    /// 用户输入的直播间号可能是短号，必须先调此接口拿到真实 `room_id`。
    /// 后续所有直播 API 均使用 `room_id`（长号）。
    pub async fn live_room_init(&self, room_id: i64, cookies: &str) -> Result<LiveRoomInit> {
        let mut params = HashMap::new();
        params.insert("id".to_string(), room_id.to_string());

        let url = "https://api.live.bilibili.com/room/v1/Room/room_init";
        let referer = "https://live.bilibili.com/";
        debug!(url, room_id, "B站直播 API 请求: room_init");

        let enriched = self.enrich_cookies(cookies).await?;
        let request = self
            .build_get_request(url, &params, referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        self.parse_data::<LiveRoomInit>(resp, "live_room_init")
            .await
    }

    /// 获取直播间详细信息（`get_info`）。
    ///
    /// 返回标题、主播 UID、封面、在线人数、分区等。
    pub async fn live_get_info(&self, room_id: i64, cookies: &str) -> Result<LiveRoomInfo> {
        let mut params = HashMap::new();
        params.insert("room_id".to_string(), room_id.to_string());

        let url = "https://api.live.bilibili.com/room/v1/Room/get_info";
        let referer = "https://live.bilibili.com/";
        debug!(url, room_id, "B站直播 API 请求: get_info");

        let enriched = self.enrich_cookies(cookies).await?;
        let request = self
            .build_get_request(url, &params, referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        self.parse_data::<LiveRoomInfo>(resp, "live_get_info").await
    }

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
                None
            }
            Err(error) => {
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
        let data = self
            .parse_data_silent::<LiveRoomPlayInfo>(response, "live_get_room_play_info")
            .await?;
        if data.live_status != 1 {
            return Err(anyhow!("直播间 {room_id} 当前未开播"));
        }
        let durl = flatten_play_info(&data);
        if durl.is_empty() {
            return Err(anyhow!("新版 getRoomPlayInfo 返回空流地址"));
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
        self.parse_data_silent::<LiveDanmuConf>(response, "live_get_danmu_info")
            .await
    }
}

fn flatten_play_info(data: &LiveRoomPlayInfo) -> Vec<LiveStreamUrl> {
    let mut result = Vec::new();
    let Some(playurl_info) = data.playurl_info.as_ref() else {
        return result;
    };
    for stream in &playurl_info.playurl.stream {
        for format in &stream.format {
            for codec in &format.codec {
                if codec.durl.is_empty() {
                    for (index, url_info) in codec.url_info.iter().enumerate() {
                        if !url_info.host.is_empty() && !codec.base_url.trim().is_empty() {
                            result.push(LiveStreamUrl {
                                url: resolve_live_url(
                                    &codec.base_url,
                                    std::slice::from_ref(url_info),
                                ),
                                order: index as i32 + 1,
                                stream_type: 0,
                                protocol_name: stream.protocol_name.clone(),
                                format_name: format.format_name.clone(),
                                codec_name: codec.codec_name.clone(),
                                current_qn: codec.current_qn,
                            });
                        }
                    }
                } else {
                    result.extend(codec.durl.iter().enumerate().map(|(index, durl)| {
                        let url = resolve_live_url(&durl.url, &codec.url_info);
                        LiveStreamUrl {
                            url,
                            order: if durl.order == 0 {
                                index as i32 + 1
                            } else {
                                durl.order
                            },
                            stream_type: durl.stream_type,
                            protocol_name: stream.protocol_name.clone(),
                            format_name: format.format_name.clone(),
                            codec_name: codec.codec_name.clone(),
                            current_qn: codec.current_qn,
                        }
                    }));
                }
            }
        }
    }
    result
}

fn resolve_live_url(raw: &str, url_info: &[LiveUrlInfo]) -> String {
    let raw = raw.trim();
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return raw.to_string();
    }
    if raw.starts_with("//") {
        return format!("https:{raw}");
    }
    let Some(info) = url_info.iter().find(|item| !item.host.is_empty()) else {
        return raw.to_string();
    };
    let host = info.host.trim_end_matches('/');
    let path = if raw.is_empty() {
        String::new()
    } else if raw.starts_with('/') {
        raw.to_string()
    } else {
        format!("/{raw}")
    };
    format!("{host}{path}{}", info.extra)
}

fn is_usable_live_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw.trim()) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return false;
    }
    !url.query_pairs()
        .any(|(key, value)| key.eq_ignore_ascii_case("sche") && value == "ban")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_stream_paths_with_cdn_metadata() {
        let info = LiveUrlInfo {
            host: "https://cdn.example.test/".to_string(),
            extra: "?token=abc".to_string(),
            ..Default::default()
        };
        let infos = [info];
        assert_eq!(
            resolve_live_url("/live/stream.flv", &infos),
            "https://cdn.example.test/live/stream.flv?token=abc"
        );
        assert_eq!(
            resolve_live_url("/live-bvc/stream.flv", &infos),
            "https://cdn.example.test/live-bvc/stream.flv?token=abc"
        );
    }

    #[test]
    fn flattens_realistic_base_url_response() {
        let data: LiveRoomPlayInfo = serde_json::from_str(
            r#"{
            "room_id": 100,
            "live_status": 1,
            "playurl_info": {"playurl": {"stream": [{
                "protocol_name": "http_hls",
                "format": [{"format_name": "fmp4", "codec": [{
                    "codec_name": "avc",
                    "current_qn": 250,
                    "accept_qn": [10000, 400, 250],
                    "base_url": "/live-bvc/stream/index.m3u8",
                    "url_info": [{"host": "https://cdn.example.test", "extra": "?token=abc"}],
                    "durl": []
                }]}]
            }]}}
        }"#,
        )
        .expect("realistic play info");
        let urls = flatten_play_info(&data);
        assert_eq!(urls.len(), 1);
        assert_eq!(
            urls[0].url,
            "https://cdn.example.test/live-bvc/stream/index.m3u8?token=abc"
        );
        assert_eq!(urls[0].current_qn, 250);
        assert_eq!(urls[0].format_name, "fmp4");
    }

    #[test]
    fn keeps_absolute_and_protocol_relative_stream_urls() {
        let info = [LiveUrlInfo {
            host: "https://unused.example.test".to_string(),
            ..Default::default()
        }];
        assert_eq!(
            resolve_live_url("https://cdn.example.test/live", &info),
            "https://cdn.example.test/live"
        );
        assert_eq!(
            resolve_live_url("//cdn.example.test/live", &info),
            "https://cdn.example.test/live"
        );
    }

    #[test]
    fn rejects_legacy_ban_and_malformed_stream_urls() {
        assert!(!is_usable_live_url(
            "https://cdn.example.test/live.flv?sche=ban&len=0"
        ));
        assert!(!is_usable_live_url("not-a-url"));
        assert!(is_usable_live_url(
            "https://cdn.example.test/live.flv?deadline=123"
        ));
    }
}
