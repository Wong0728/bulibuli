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

/// B 站流不可下载的业务性错误（区别于网络/风控等瞬时错误）：
/// - `permission = true`：充电专属/付费内容无观看权限，playurl 只回试看片段（durl）。
///   下载试看片段会产出时长正确但后半段为空的坏文件，必须拒绝。
/// - `permission = false`：有权限但只有暂不支持下载语义的流（如多分段 durl）。
///
/// 由 `AppError::From<anyhow>` 映射为 HTTP 402/400 并透传真实文案；
/// 此前用裸 anyhow 报错会被归为 Internal(500)，前端只能看到"服务器内部错误"。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct StreamUnavailableError {
    pub message: String,
    pub permission: bool,
}

impl StreamUnavailableError {
    pub fn permission(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permission: true,
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            permission: false,
        }
    }
}

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
    /// 内部富化 Cookie，返回强类型 data（含 dash / durl）。
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
        let mut data = self
            .request_playurl(bvid, cid, cookies, qn_value, fnval_value)
            .await?;

        if let Some(dash) = data.dash.take() {
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
            return self.finish_video_streams(data, cid, qualities, preferred_quality);
        }

        // 无 DASH 时的 durl 分支。durl 是音视频已封装的直链（现代 B 站几乎总是单段），
        // 出现场景与处置：
        // - 充电专属/付费内容且无观看权限 → 只回"试看"片段，下载会产出坏文件，明确拒绝；
        // - 有权限但仅有多分段流 → 需要分段下载+拼接语义，暂不支持，明确拒绝；
        // - 有权限的单段直链（多为老投稿）→ 直接构建可下载流，不再一刀切报错。
        if let Some(durl) = data.durl.take().filter(|segments| !segments.is_empty()) {
            let info = self.get_video_info(bvid, cookies).await?;
            if info.is_upower_exclusive && !info.is_upower_play {
                return Err(anyhow!(StreamUnavailableError::permission(
                    "该视频为充电专属内容，当前账号没有观看权限，仅能获取试看片段",
                )));
            }
            if info.rights.ugc_pay == 1 || info.rights.pay == 1 {
                return Err(anyhow!(StreamUnavailableError::permission(
                    "该视频为付费内容，当前账号未购买，仅能获取试看片段",
                )));
            }
            if durl.len() > 1 {
                return Err(anyhow!(StreamUnavailableError::unsupported(format!(
                    "该视频仅提供 {} 段分段流（durl），暂不支持分段下载拼接",
                    durl.len()
                ))));
            }
            let segment = &durl[0];
            let mut urls = Vec::new();
            if let Some(main) = segment.url.as_deref().filter(|u| !u.is_empty()) {
                urls.push(main.to_string());
            }
            for backup in segment.backup_url.iter().flatten() {
                if !backup.is_empty() && !urls.contains(backup) {
                    urls.push(backup.clone());
                }
            }
            if urls.is_empty() {
                // durl 存在但全部 URL 为空：按“未找到视频流”处理
                return Ok(VideoStreams {
                    cid,
                    qualities: Vec::new(),
                    selected_quality: None,
                    available_qualities: Vec::new(),
                    accept_quality: data.accept_quality,
                });
            }
            let url = self
                .bad_cdns
                .choose_url(&urls)
                .await
                .unwrap_or(&urls[0])
                .to_string();
            let quality = data.quality.unwrap_or(16).clamp(16, 127) as i32;
            let quality_name = QUALITY_NAMES
                .iter()
                .find(|(q, _)| *q == quality)
                .map(|(_, n)| n.to_string())
                .unwrap_or_else(|| "直链".to_string())
                .to_string();
            let format = durl_container_format(&urls[0]);
            tracing::info!(
                bvid,
                cid,
                quality,
                format,
                size = segment.size,
                "playurl 仅提供 durl 直链（有观看权限），按单段封装流构建下载"
            );
            let qualities = vec![StreamQuality {
                quality,
                quality_name,
                width: 0,
                height: 0,
                url,
                urls,
                size: segment.size,
                format: format.to_string(),
                codec: None,
            }];
            return self.finish_video_streams(data, cid, qualities, preferred_quality);
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

    /// 流集合的公共收尾：排序、按偏好选流、组装 VideoStreams。
    /// DASH 与 durl 单段直链两条构建路径共用。
    fn finish_video_streams(
        &self,
        data: PlayurlData,
        cid: i64,
        mut qualities: Vec<StreamQuality>,
        preferred_quality: Option<i32>,
    ) -> Result<VideoStreams> {
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
        Ok(VideoStreams {
            cid,
            qualities,
            selected_quality: selected,
            available_qualities,
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

/// 从 durl 直链推断封装容器：B 站 durl 只有 flv/mp4 两种，
/// 扩展名在 URL 路径里（查询串是签名参数）。无法识别时按 mp4 兜底。
fn durl_container_format(url: &str) -> &'static str {
    let path = url.split('?').next().unwrap_or_default();
    if path.to_ascii_lowercase().ends_with(".flv") {
        "flv"
    } else {
        "mp4"
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
    use super::durl_container_format;
    use super::StreamQuality;
    use super::StreamUnavailableError;

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

    #[test]
    fn durl_format_is_derived_from_url_path_only() {
        assert_eq!(
            durl_container_format("https://cdn/x/main.flv?e=sig&uipk=5"),
            "flv"
        );
        assert_eq!(
            durl_container_format("https://cdn/x/main.MP4?deadline=1"),
            "mp4"
        );
        assert_eq!(durl_container_format("https://cdn/x/no-ext"), "mp4");
    }

    #[test]
    fn stream_unavailable_error_flags_permission() {
        let permission = StreamUnavailableError::permission("无权限");
        assert!(permission.permission);
        let unsupported = StreamUnavailableError::unsupported("多分段");
        assert!(!unsupported.permission);
    }
}
