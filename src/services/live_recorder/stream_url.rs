//! 直播流 URL 管理：拼接、过期检测、刷新。
//!
//! 旧版 `playUrl` 返回的 FLV 流 URL 包含 `expires` 参数（Unix 时间戳），
//! 过期后 CDN 返回 403。录制服务需要在过期前主动刷新。

use anyhow::{anyhow, Result};
use tracing::debug;

/// 从流 URL 中提取 `expires` 参数的值（Unix 时间戳）。
pub fn extract_expires(url: &str) -> Option<i64> {
    url::Url::parse(url).ok().and_then(|parsed| {
        parsed
            .query_pairs()
            .find(|(key, _)| key == "expires" || key == "deadline")
            .and_then(|(_, value)| value.parse::<i64>().ok())
    })
}

/// 检查流 URL 是否即将过期（距过期不足 `margin_secs` 秒）。
///
/// 缺少 `expires` 时返回 false，避免定时器在无法判断过期时间时不停轮换分段；
/// 这类 URL 仍会响应弹幕协议明确发出的 `PLAYURL_RELOAD` 事件。
pub fn is_expiring_soon(url: &str, margin_secs: i64) -> bool {
    extract_expires(url)
        .is_some_and(|expires| expires - chrono::Utc::now().timestamp() < margin_secs)
}

/// 从 `LivePlayUrl` 响应中选择最佳流 URL。
///
/// 优先选择 `order=1`（主线），URL 中的 `\u0026` 等转义字符需要还原。
pub fn select_stream_candidates(
    durl: &[crate::services::bili_api::models::live::LiveStreamUrl],
) -> Result<Vec<crate::services::bili_api::models::live::LiveStreamUrl>> {
    let mut sorted = durl
        .iter()
        .filter_map(|stream| {
            let mut candidate = stream.clone();
            candidate.url = candidate.url.replace("\\u0026", "&").replace("\\/", "/");
            url::Url::parse(candidate.url.trim())
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
                .map(|_| candidate)
        })
        .collect::<Vec<_>>();
    sorted.sort_by_key(|stream| {
        let codec_rank = match stream.codec_name.to_ascii_lowercase().as_str() {
            "avc" | "avc1" | "h264" => 0,
            "hevc" | "h265" => 1,
            _ => 2,
        };
        let format_rank = match stream.format_name.to_ascii_lowercase().as_str() {
            "flv" => 0,
            "fmp4" => 1,
            "ts" => 2,
            _ => 3,
        };
        (
            std::cmp::Reverse(stream.current_qn),
            codec_rank,
            format_rank,
            stream.order,
        )
    });
    let mut seen = std::collections::HashSet::new();
    sorted.retain(|stream| seen.insert(stream.url.clone()));
    if sorted.is_empty() {
        return Err(anyhow!("娴佸湴鍧€鍒楄〃涓虹┖"));
    }
    Ok(sorted)
}

#[allow(dead_code)]
pub fn select_best_stream(
    durl: &[crate::services::bili_api::models::live::LiveStreamUrl],
) -> Result<crate::services::bili_api::models::live::LiveStreamUrl> {
    // Prefer the actual highest quality, broadly compatible AVC, direct FLV
    // when available, then the upstream CDN order. HLS/fMP4 remains available
    // as a fallback for partitions that do not expose a direct FLV stream.
    let mut sorted = durl
        .iter()
        .filter(|stream| {
            url::Url::parse(stream.url.trim()).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    sorted.sort_by_key(|stream| {
        let codec_rank = match stream.codec_name.to_ascii_lowercase().as_str() {
            "avc" | "avc1" | "h264" => 0,
            "hevc" | "h265" => 1,
            _ => 2,
        };
        let format_rank = match stream.format_name.to_ascii_lowercase().as_str() {
            "flv" => 0,
            "fmp4" => 1,
            "ts" => 2,
            _ => 3,
        };
        (
            std::cmp::Reverse(stream.current_qn),
            codec_rank,
            format_rank,
            stream.order,
        )
    });

    let mut best = sorted
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("流地址列表为空"))?;

    if best.url.is_empty() {
        return Err(anyhow!("最佳流地址为空"));
    }

    // B 站返回的 URL 可能包含 JSON 转义字符（\u0026 → &），需要还原
    best.url = best.url.replace("\\u0026", "&").replace("\\/", "/");

    debug!(
        order = best.order,
        quality = best.current_qn,
        protocol = %best.protocol_name,
        format = %best.format_name,
        codec = %best.codec_name,
        expires_soon = is_expiring_soon(&best.url, 60),
        "选择直播流线路"
    );
    Ok(best)
}

#[cfg(test)]
pub fn select_best_url(
    durl: &[crate::services::bili_api::models::live::LiveStreamUrl],
) -> Result<String> {
    Ok(select_best_stream(durl)?.url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::bili_api::models::live::LiveStreamUrl;

    #[test]
    fn extract_expires_from_url() {
        let url = "https://cdn.example.com/live.flv?expires=1723982800&sign=abc";
        assert_eq!(extract_expires(url), Some(1723982800));
    }

    #[test]
    fn extract_expires_missing() {
        let url = "https://cdn.example.com/live.flv?sign=abc";
        assert_eq!(extract_expires(url), None);
    }

    #[test]
    fn missing_expires_does_not_trigger_periodic_refresh() {
        let url = "https://cdn.example.com/live.flv?sign=abc";
        assert!(!is_expiring_soon(url, 60));
    }

    #[test]
    fn expires_inside_margin_triggers_refresh() {
        let expires = chrono::Utc::now().timestamp() + 30;
        let url = format!("https://cdn.example.com/live.flv?expires={expires}");
        assert!(is_expiring_soon(&url, 60));
    }

    #[test]
    fn select_best_picks_order_1() {
        let durl = vec![
            LiveStreamUrl {
                url: "https://backup/live.flv".into(),
                order: 2,
                stream_type: 0,
                ..Default::default()
            },
            LiveStreamUrl {
                url: "https://main/live.flv".into(),
                order: 1,
                stream_type: 0,
                ..Default::default()
            },
        ];
        let url = select_best_url(&durl).expect("select best");
        assert_eq!(url, "https://main/live.flv");
    }

    #[test]
    fn select_best_unescapes_url() {
        let durl = vec![LiveStreamUrl {
            url: "https://cdn/live.flv?expires=123\\u0026sign=abc\\u0026trid=xyz".into(),
            order: 1,
            stream_type: 0,
            ..Default::default()
        }];
        let url = select_best_url(&durl).expect("select best");
        assert!(url.contains("expires=123&sign=abc&trid=xyz"));
    }

    #[test]
    fn select_best_prefers_quality_then_compatible_codec() {
        let streams = vec![
            LiveStreamUrl {
                url: "https://cdn/low.flv".into(),
                order: 1,
                current_qn: 250,
                codec_name: "avc".into(),
                format_name: "flv".into(),
                ..Default::default()
            },
            LiveStreamUrl {
                url: "https://cdn/high-hevc.m3u8".into(),
                order: 1,
                current_qn: 10000,
                codec_name: "hevc".into(),
                format_name: "fmp4".into(),
                ..Default::default()
            },
            LiveStreamUrl {
                url: "https://cdn/high-avc.flv".into(),
                order: 2,
                current_qn: 10000,
                codec_name: "avc".into(),
                format_name: "flv".into(),
                ..Default::default()
            },
        ];
        let best = select_best_stream(&streams).expect("best stream");
        assert_eq!(best.url, "https://cdn/high-avc.flv");
    }

    #[test]
    fn stream_candidates_rotate_unique_valid_cdns() {
        let streams = vec![
            LiveStreamUrl {
                url: "https://cdn-a/live.flv?x=1\\u0026sign=a".into(),
                current_qn: 10000,
                codec_name: "avc".into(),
                format_name: "flv".into(),
                ..Default::default()
            },
            LiveStreamUrl {
                url: "https://cdn-a/live.flv?x=1&sign=a".into(),
                current_qn: 10000,
                codec_name: "avc".into(),
                format_name: "flv".into(),
                ..Default::default()
            },
            LiveStreamUrl {
                url: "https://cdn-b/live.flv".into(),
                current_qn: 9000,
                codec_name: "avc".into(),
                format_name: "flv".into(),
                ..Default::default()
            },
        ];
        let candidates = select_stream_candidates(&streams).expect("candidates");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].url, "https://cdn-a/live.flv?x=1&sign=a");
        assert_eq!(candidates[1].url, "https://cdn-b/live.flv");
    }
}
