//! 视频信息与音视频流解析：get_video_info / playurl / 流选择（全程强类型）。

use crate::services::wbi;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tracing::debug;

use super::models::playurl::{
    AudioQuality, AudioStreams, PlayurlData, StreamQuality, VideoStreams,
};
use super::models::video::{SubtitleInfo, VideoInfo};
use super::{session_fingerprint, BiliApi, QUALITY_NAMES};

const VIDEO_INFO_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

impl BiliApi {
    /// 获取视频详情（`/x/web-interface/wbi/view`）。
    /// 业务失败（下架/风控/未登录等）一律走 `Err`（由统一入口 classify）。
    pub async fn get_video_info(&self, bvid: &str, cookies: &str) -> Result<VideoInfo> {
        let cache_key = (session_fingerprint(cookies), bvid.to_string());
        {
            let cache = self.video_info_cache.read().await;
            if let Some((info, fetched_at)) = cache.get(&cache_key) {
                if fetched_at.elapsed() < VIDEO_INFO_CACHE_TTL {
                    return Ok(info.clone());
                }
            }
        }
        let enriched = self.enrich_cookies(cookies).await?;

        let (img_key, sub_key) = self.get_wbi_keys(&enriched).await?;
        let mut params = HashMap::new();
        params.insert("bvid".to_string(), bvid.to_string());
        self.inject_risk_params(&mut params, "1315873");
        wbi::enc_wbi(&mut params, &img_key, &sub_key)?;

        let url = "https://api.bilibili.com/x/web-interface/wbi/view";
        let referer = format!("https://www.bilibili.com/video/{bvid}");
        debug!(url, bvid, "B站 API 请求: get_video_info");

        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let mut info = self.parse_data::<VideoInfo>(resp, "get_video_info").await?;
        info.pic = super::models::user::normalize_image_url(&info.pic);
        info.owner.face = super::models::user::normalize_image_url(&info.owner.face);
        let mut cache = self.video_info_cache.write().await;
        cache.retain(|_, (_, fetched_at)| fetched_at.elapsed() < VIDEO_INFO_CACHE_TTL);
        cache.insert(cache_key, (info.clone(), std::time::Instant::now()));
        Ok(info)
    }

    /// 获取视频 CC 字幕列表（`/x/player/wbi/v2`）。
    /// 返回 `data.subtitle.subtitles[]`，每项含 `lan`/`lan_doc`/`subtitle_url`。
    /// 无字幕视频返回空列表（不报错）。
    pub async fn get_subtitles(
        &self,
        bvid: &str,
        cid: i64,
        cookies: &str,
    ) -> Result<Vec<SubtitleInfo>> {
        let enriched = self.enrich_cookies(cookies).await?;
        let (img_key, sub_key) = self.get_wbi_keys(&enriched).await?;
        let mut params = HashMap::new();
        params.insert("bvid".to_string(), bvid.to_string());
        params.insert("cid".to_string(), cid.to_string());
        self.inject_risk_params(&mut params, "1315873");
        wbi::enc_wbi(&mut params, &img_key, &sub_key)?;

        let url = "https://api.bilibili.com/x/player/wbi/v2";
        let referer = format!("https://www.bilibili.com/video/{bvid}");
        debug!(url, bvid, cid, "B站 API 请求: get_subtitles");

        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let envelope = self.read_envelope(resp, "get_subtitles").await?;
        // data.subtitle.subtitles 可能缺失（无字幕视频），按空列表处理
        let subtitles_value = envelope
            .data
            .get("subtitle")
            .and_then(|s| s.get("subtitles"))
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Array(vec![]));
        let list: Vec<SubtitleInfo> = serde_json::from_value(subtitles_value).unwrap_or_default();
        Ok(list)
    }

    /// 统一的 playurl 请求：WBI 签名 + GET /x/player/wbi/playurl。
    /// 内部富化 cookie，返回强类型 data（含 dash / durl）。
    async fn request_playurl(
        &self,
        bvid: &str,
        cid: i64,
        cookies: &str,
        qn: i32,
        fnval: i32,
    ) -> Result<PlayurlData> {
        let enriched = self.enrich_cookies(cookies).await?;

        let (img_key, sub_key) = self.get_wbi_keys(&enriched).await?;
        let mut params = HashMap::new();
        params.insert("bvid".to_string(), bvid.to_string());
        params.insert("cid".to_string(), cid.to_string());
        params.insert("qn".to_string(), qn.to_string());
        params.insert("fnval".to_string(), fnval.to_string());
        params.insert("fnver".to_string(), "0".to_string());
        params.insert("fourk".to_string(), "1".to_string());
        params.insert("platform".to_string(), "web".to_string());
        params.insert("web_location".to_string(), "1550101".to_string());
        self.inject_risk_params(&mut params, ""); // web_location 已设置
        wbi::enc_wbi(&mut params, &img_key, &sub_key)?;

        let url = "https://api.bilibili.com/x/player/wbi/playurl";
        let referer = format!("https://www.bilibili.com/video/{bvid}");
        debug!(url, bvid, cid, qn, fnval, "B站 API 请求: playurl");

        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        self.parse_data::<PlayurlData>(resp, "playurl").await
    }

    /// 解析可下载视频流集合。
    /// dash/durl 均缺失时返回空 `qualities`（调用方据此判定“未找到视频流”）。
    /// `cid = Some(..)` 时按指定分P取流（多P场景）；`None` 时取默认 cid（P1，保持现状）。
    pub async fn get_video_urls(
        &self,
        bvid: &str,
        cookies: &str,
        fnval: i32,
        preferred_quality: Option<i32>,
        cid: Option<i64>,
    ) -> Result<VideoStreams> {
        let cid = match cid {
            Some(c) if c > 0 => c,
            _ => {
                let info = self.get_video_info(bvid, cookies).await?;
                if info.cid <= 0 {
                    return Err(anyhow!("未找到视频 cid"));
                }
                info.cid
            }
        };

        let qn_value = preferred_quality.unwrap_or(127);
        let fnval_value = if fnval <= 0 { 4048 } else { fnval };
        let data = self
            .request_playurl(bvid, cid, cookies, qn_value, fnval_value)
            .await?;

        if let Some(dash) = data.dash {
            let mut qualities: Vec<StreamQuality> = Vec::new();
            for stream in dash.video {
                let urls = stream.collect_urls();
                if urls.is_empty() {
                    continue;
                }
                let url = self
                    .bad_cdns
                    .choose_url(&urls)
                    .await
                    .unwrap_or(&urls[0])
                    .to_string();
                let quality = stream.id as i32;
                let quality_name = QUALITY_NAMES
                    .iter()
                    .find(|(q, _)| *q == quality)
                    .map(|(_, n)| n.to_string())
                    .unwrap_or_else(|| format!("{}x{}", stream.width, stream.height));
                qualities.push(StreamQuality {
                    quality,
                    quality_name,
                    width: stream.width,
                    height: stream.height,
                    url,
                    urls,
                    size: stream.size,
                    format: "m4s".to_string(),
                    codec: Some(
                        stream
                            .codecs
                            .as_deref()
                            .unwrap_or("unknown")
                            .to_ascii_lowercase(),
                    ),
                });
            }
            qualities.sort_by_key(|q| std::cmp::Reverse(q.quality));
            let selected = preferred_quality.and_then(|pq| {
                choose_video_stream(
                    &qualities,
                    pq,
                    16,
                    &["av1".to_string(), "hevc".to_string(), "avc".to_string()],
                    true,
                )
            });
            let available_qualities = qualities.iter().map(|q| i64::from(q.quality)).collect();
            return Ok(VideoStreams {
                cid,
                qualities,
                selected_quality: selected,
                available_qualities,
                accept_quality: data.accept_quality,
            });
        }

        if data.durl.is_some() {
            // 方案B 明确拒绝：durl(FLV) 分段流无分段下载+拼接语义，
            // 半成品回退路径会产出损坏文件（多段未拼接、画质硬编码 80/流畅/720p）。
            // 直接报错让用户感知，避免僵尸任务与损坏产物。
            return Err(anyhow!(
                "该视频仅提供 FLV 分段流（durl），暂不支持下载。请等待 DASH 流恢复或联系开发者"
            ));
        }

        // dash/durl 均缺失：返回空流集合，调用方按“未找到视频流”处理
        Ok(VideoStreams {
            cid,
            qualities: Vec::new(),
            selected_quality: None,
            available_qualities: Vec::new(),
            accept_quality: data.accept_quality,
        })
    }

    /// 获取音频下载流。`cid = Some(..)` 时直接复用，省一次 get_video_info 调用。
    /// 视频无音频流时返回 `Ok(None)`（调用方按“未找到音频流”处理）。
    /// `preference` 控制音轨偏好："m4a"（默认最高码率）/ "dolby"（杜比全景声优先）/
    /// "flac"（Hi-Res 无损优先）。命中时 ext 切换为 ec3/flac，未命中回退 m4a。
    pub async fn get_audio_url(
        &self,
        bvid: &str,
        cid: Option<i64>,
        cookies: &str,
        preference: &str,
    ) -> Result<Option<AudioStreams>> {
        let cid = match cid {
            Some(c) => c,
            None => {
                let info = self.get_video_info(bvid, cookies).await?;
                if info.cid <= 0 {
                    return Err(anyhow!("未找到视频 cid"));
                }
                info.cid
            }
        };

        let data = self.request_playurl(bvid, cid, cookies, 127, 4048).await?;
        let dash = match data.dash {
            Some(d) => d,
            None => return Ok(None),
        };

        // 收集常规 m4a 音频流（按码率降序）
        let mut qualities: Vec<AudioQuality> = Vec::new();
        for stream in dash.audio.as_deref().unwrap_or(&[]) {
            let urls = stream.collect_urls();
            if urls.is_empty() {
                continue;
            }
            let selected_url = self
                .bad_cdns
                .choose_url(&urls)
                .await
                .unwrap_or(&urls[0])
                .to_string();
            qualities.push(AudioQuality {
                id: stream.id,
                bandwidth: stream.bandwidth,
                url: selected_url,
            });
        }
        qualities.sort_by_key(|q| std::cmp::Reverse(q.bandwidth));

        // 杜比全景声：dash.dolby.audio[]，无权限/无 fnval 位时整段为 null
        let mut dolby_url: Option<String> = None;
        if let Some(dolby) = dash.dolby.as_ref() {
            for stream in dolby.audio.as_deref().unwrap_or_default() {
                let urls = stream.collect_urls();
                if urls.is_empty() {
                    continue;
                }
                dolby_url = Some(
                    self.bad_cdns
                        .choose_url(&urls)
                        .await
                        .unwrap_or(&urls[0])
                        .to_string(),
                );
                break;
            }
        }

        // Hi-Res 无损：dash.flac.audio（单个对象，可能为 null）
        let mut flac_url: Option<String> = None;
        if let Some(flac) = dash.flac.as_ref() {
            if let Some(stream) = flac.audio.as_ref() {
                let urls = stream.collect_urls();
                if !urls.is_empty() {
                    flac_url = Some(
                        self.bad_cdns
                            .choose_url(&urls)
                            .await
                            .unwrap_or(&urls[0])
                            .to_string(),
                    );
                }
            }
        }

        // 按偏好选择：命中 → 用对应轨 + 切换 ext；未命中 → 回退最高码率 m4a（保持现状）
        let (selected_url, ext) = match preference {
            "flac" if flac_url.is_some() => (flac_url.unwrap(), "flac".to_string()),
            "dolby" if dolby_url.is_some() => (dolby_url.unwrap(), "ec3".to_string()),
            "flac" if dolby_url.is_some() => (dolby_url.unwrap(), "ec3".to_string()),
            _ => {
                if qualities.is_empty() {
                    return Ok(None);
                }
                (qualities[0].url.clone(), "m4a".to_string())
            }
        };

        if qualities.is_empty() && selected_url.is_empty() {
            return Ok(None);
        }

        Ok(Some(AudioStreams {
            audio_url: selected_url,
            qualities,
            ext,
        }))
    }
}

/// 在可用视频流中执行稳定、可测试的“画质优先、编码偏好、最低画质”选择。
/// 只允许向下回退；所有候选都低于最低画质时返回 None。
pub(crate) fn choose_video_stream(
    streams: &[StreamQuality],
    desired_quality: i32,
    minimum_quality: i32,
    codec_preferences: &[String],
    allow_fallback: bool,
) -> Option<StreamQuality> {
    let mut candidates: Vec<StreamQuality> = streams
        .iter()
        .filter(|stream| {
            stream.quality >= minimum_quality
                && (allow_fallback || stream.quality == desired_quality)
                && stream.quality <= desired_quality
        })
        .cloned()
        .collect();
    candidates.sort_by(|left, right| {
        let quality = right.quality.cmp(&left.quality);
        if quality != std::cmp::Ordering::Equal {
            return quality;
        }
        let rank = |stream: &StreamQuality| {
            let actual = stream.codec.as_deref().unwrap_or_default();
            codec_preferences
                .iter()
                .position(|codec| match codec.as_str() {
                    "av1" => actual.contains("av1") || actual.contains("av01"),
                    other => actual.contains(other),
                })
                .unwrap_or(usize::MAX)
        };
        rank(left).cmp(&rank(right))
    });
    candidates.into_iter().next()
}

#[cfg(test)]
mod stream_selection_tests {
    use super::choose_video_stream;
    use super::StreamQuality;

    fn stream(quality: i32, codec: &str) -> StreamQuality {
        StreamQuality {
            quality,
            codec: Some(codec.to_string()),
            ..StreamQuality::default()
        }
    }

    #[test]
    fn prefers_requested_quality_then_preferred_codec() {
        let streams = vec![stream(80, "avc1"), stream(80, "av01"), stream(64, "av01")];
        let selected = choose_video_stream(&streams, 80, 64, &["av1".into(), "avc".into()], true)
            .expect("stream");
        assert_eq!(selected.codec.as_deref(), Some("av01"));
    }

    #[test]
    fn refuses_fallback_below_minimum() {
        let streams = vec![stream(64, "avc1")];
        assert!(choose_video_stream(&streams, 80, 80, &["avc".into()], true).is_none());
    }
}
