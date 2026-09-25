//! playurl 请求与视频流集合构建：WBI 签名的 `/x/player/wbi/playurl` 请求、
//! DASH 流挑选（结合坏 CDN 熔断记录选线路）、durl 直链分支
//! （充电专属/付费/多分段拒绝语义）与 qualities 收尾组装。

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tracing::debug;

use crate::services::bili_api::models::playurl::{PlayurlData, StreamQuality, VideoStreams};
use crate::services::bili_api::{BiliApi, QUALITY_NAMES};
use crate::services::wbi;

use super::error::StreamUnavailableError;
use super::selection::choose_video_stream;

impl BiliApi {
    /// 统一的 playurl 请求：WBI 签名 + GET /x/player/wbi/playurl。
    /// 内部富化 Cookie，返回强类型 data（含 dash / durl）。
    pub(super) async fn request_playurl(
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

#[cfg(test)]
mod tests {
    use super::durl_container_format;

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
}
