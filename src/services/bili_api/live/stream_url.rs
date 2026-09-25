//! 直播流地址契约与 URL 处理：契约错误类型、播放信息校验、
//! 播放信息树展平为流地址列表、CDN URL 解析与合法性过滤。

use anyhow::{anyhow, Result};

use crate::services::bili_api::models::live::{LiveRoomPlayInfo, LiveStreamUrl, LiveUrlInfo};

/// 直播接口返回结构与本仓库模型不兼容（缺字段/结构漂移）时使用的契约错误：
/// 与网络/风控错误区分开，用于决定新版接口失败是否回退旧版接口。
#[derive(Debug, thiserror::Error)]
#[error("直播接口契约不兼容: {0}")]
pub(super) struct LiveSchemaError(String);

/// 构造契约错误（anyhow 包装，便于在 Result 链中透传与 downcast）。
pub(super) fn schema_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(LiveSchemaError(message.into()))
}

/// 校验新版 `getRoomPlayInfo` 响应的基本契约：room_id 匹配、已开播、playurl_info 存在。
pub(super) fn validate_live_play_info(room_id: i64, data: &LiveRoomPlayInfo) -> Result<()> {
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

/// 把新版 `getRoomPlayInfo` 的协议/封装/编码/CDN 分层树展平为
/// 与旧版 `playUrl` 一致的 `LiveStreamUrl` 列表，供旧版回退与录制器复用。
pub(super) fn flatten_play_info(data: &LiveRoomPlayInfo) -> Vec<LiveStreamUrl> {
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

/// 按响应里的 CDN 元数据把相对路径解析成完整流地址。
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

/// 过滤不可用的直播流地址：语法非法，或带 `sche=ban`（B 站封禁标记）的直链。
pub(super) fn is_usable_live_url(raw: &str) -> bool {
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
    fn live_contract_rejects_missing_stream_fields() {
        let data = LiveRoomPlayInfo {
            room_id: 100,
            live_status: 1,
            playurl_info: None,
        };
        assert!(validate_live_play_info(100, &data).is_err());
    }

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
