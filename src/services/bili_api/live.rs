//! 直播间 API 客户端：房间信息、流地址、弹幕连接配置。
//!
//! 播放地址保留旧版接口作为播放协议兼容路径；弹幕配置只允许
//! WBI 签名的 `getDanmuInfo`，不会回退到旧版 `getConf`。
//! 接口域名：`api.live.bilibili.com`，走 `api_client`（严格 TLS）。
/// 从带 WBI 签名的 `getDanmuInfo` 获取带登录态的 WebSocket 信息。
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

/// uid 无效（<=0）警告的进程级闸门：只告警一次，避免每个监控周期刷屏。
static WARNED_INVALID_LIVE_UID: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn warn_invalid_live_uid_once() {
    use std::sync::atomic::Ordering;
    if !WARNED_INVALID_LIVE_UID.swap(true, Ordering::Relaxed) {
        warn!(
            "存在 uid<=0 的直播源：UID 批量探测永远无法覆盖（已跳过），\
             请重新保存直播源以解析真实 uid（本警告仅记录一次）"
        );
    }
}

impl BiliApi {
    /// 监控服务使用的批量状态探测。
    /// 一次请求覆盖所有已保存的主播，避免按房间同步轮询造成请求尖峰。
    pub async fn live_status_by_uids(
        &self,
        uids: &[i64],
        cookies: &str,
    ) -> Result<HashMap<i64, LiveBatchStatus>> {
        let uids_len = uids.len();
        // uid<=0 的直播源（历史脏数据/解析失败遗留）永远无法通过 UID 批量探测：
        // 跳过且只警告一次，而不是每个监控周期（约 30s）重复报错。
        let uids = uids
            .iter()
            .copied()
            .filter(|uid| *uid > 0)
            .collect::<Vec<_>>();
        if uids.len() != uids_len {
            warn_invalid_live_uid_once();
        }
        if uids.is_empty() {
            return Ok(HashMap::new());
        }
        let url = "https://api.live.bilibili.com/room/v1/Room/get_status_info_by_uids";
        let enriched = self.enrich_cookies(cookies).await?;
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
        // 后台批量探测走专用限流器：仅在发送处扣减后台配额，
        // 不再先过 background limiter 又在发送时扣前台配额（双扣减）。
        let response = self
            .send_with_retry_limited(request, &self.background_rate_limiter)
            .await?;
        let data: LiveBatchStatusMap = self
            .parse_data_silent(response, "live_status_info_by_uids")
            .await?;
        Ok(data
            .into_values()
            .filter(|item| item.uid > 0)
            .map(|item| (item.uid, item))
            .collect())
    }
    /// 获取用于补齐重连间隔的小窗口最新消息。
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

#[derive(Debug, thiserror::Error)]
#[error("直播接口契约不兼容: {0}")]
struct LiveSchemaError(String);

fn schema_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(LiveSchemaError(message.into()))
}

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

fn is_live_playurl_schema_error(error: &anyhow::Error) -> bool {
    let detail = error.to_string();
    detail.contains("反序列化 B站 API live_get_room_play_info")
        || detail.contains("解析 B站 API live_get_room_play_info JSON")
}

fn validate_live_play_info(room_id: i64, data: &LiveRoomPlayInfo) -> Result<()> {
    if data.room_id <= 0 || data.room_id != room_id {
        return Err(schema_error("直播播放信息缺少匹配的 room_id"));
    }
    if data.live_status != 1 {
        return Err(anyhow!("直播间 {room_id} 当前未开播"));
    }
    if data.playurl_info.is_none() {
        return Err(schema_error("直播播放信息缺少 playurl_info"));
    }
    Ok(())
}

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
    fn fallback_only_accepts_endpoint_or_schema_failures() {
        assert!(should_fallback_live_playurl(&schema_error("schema")));
        assert!(should_fallback_live_playurl(&anyhow::Error::new(
            crate::error::BiliApiError::classify(-404, "missing")
        )));
        assert!(!should_fallback_live_playurl(&anyhow::anyhow!("HTTP 401")));
    }

    #[test]
    fn live_contract_rejects_missing_stream_fields() {
        let data = LiveRoomPlayInfo {
            room_id: 100,
            live_status: 1,
            playurl_info: None,
        };
        assert!(validate_live_play_info(100, &data).is_err());
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
    let Ok(url) = crate::services::bili_url_policy::validate_live_endpoint_syntax(raw, false)
    else {
        return false;
    };
    !url.query_pairs()
        .any(|(key, value)| key.eq_ignore_ascii_case("sche") && value == "ban")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_stream_paths_with_cdn_metadata() {
        let info = LiveUrlInfo {
            host: "https://cdn.bilivideo.com/".to_string(),
            extra: "?token=abc".to_string(),
            ..Default::default()
        };
        let infos = [info];
        assert_eq!(
            resolve_live_url("/live/stream.flv", &infos),
            "https://cdn.bilivideo.com/live/stream.flv?token=abc"
        );
        assert_eq!(
            resolve_live_url("/live-bvc/stream.flv", &infos),
            "https://cdn.bilivideo.com/live-bvc/stream.flv?token=abc"
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
                    "url_info": [{"host": "https://cdn.bilivideo.com", "extra": "?token=abc"}],
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
            "https://cdn.bilivideo.com/live-bvc/stream/index.m3u8?token=abc"
        );
        assert_eq!(urls[0].current_qn, 250);
        assert_eq!(urls[0].format_name, "fmp4");
    }

    #[test]
    fn keeps_absolute_and_protocol_relative_stream_urls() {
        let info = [LiveUrlInfo {
            host: "https://unused.bilivideo.com".to_string(),
            ..Default::default()
        }];
        assert_eq!(
            resolve_live_url("https://cdn.bilivideo.com/live", &info),
            "https://cdn.bilivideo.com/live"
        );
        assert_eq!(
            resolve_live_url("//cdn.bilivideo.com/live", &info),
            "https://cdn.bilivideo.com/live"
        );
    }

    #[test]
    fn rejects_legacy_ban_and_malformed_stream_urls() {
        assert!(!is_usable_live_url(
            "https://cdn.bilivideo.com/live.flv?sche=ban&len=0"
        ));
        assert!(!is_usable_live_url("not-a-url"));
        assert!(is_usable_live_url(
            "https://cdn.bilivideo.com/live.flv?deadline=123"
        ));
    }
}
