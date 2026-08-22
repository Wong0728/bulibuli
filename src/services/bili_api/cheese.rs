//! 课程（Cheese / PUGV）接口客户端：季/集信息 + 取流。
//!
//! 与番剧接口的差异：
//! - 季信息走 `/pugv/view/web/season/v2`，响应在 `data` 字段（与番剧的 `result` 不同）。
//! - 取流走 `/pugv/player/web/playurl`，响应在 `data` 字段；参数用 `avid` + `cid` + `ep_id`。
//! - 课程未购买时取流返回 `code != 0` 或空 dash，调用方据此判定 pay_blocked。
//!
//! 注意：B 站课程接口路径为 `pugv`（公开课/付费课程），并非迭代计划中笔误的 `pubed`。
//! 实际可访问的端点以 Bili23-Downloader 等成熟实现为准。

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tracing::debug;

use super::models::cheese::{CheesePlayurlData, CheeseSeason};
use super::models::playurl::{StreamQuality, VideoStreams};
use super::{BiliApi, QUALITY_NAMES};

impl BiliApi {
    /// 获取课程季信息（`/pugv/view/web/season/v2`）。
    /// - `season_id`：从 fp 链接解析得到（fp 即课程 season 短链）。
    /// - `ep_id`：可选；课程链接通常以 ss/fp 形式给出，ep_id 用于精确反查。
    ///
    /// 响应主体在 `data` 字段，使用 `parse_data` 解析。
    pub async fn get_cheese_season_info(
        &self,
        season_id: Option<u64>,
        ep_id: Option<u64>,
        cookies: &str,
    ) -> Result<CheeseSeason> {
        if season_id.is_none() && ep_id.is_none() {
            return Err(anyhow!("season_id 与 ep_id 至少需要传一个"));
        }
        let enriched = self.enrich_cookies(cookies).await?;
        let mut params = HashMap::new();
        if let Some(sid) = season_id {
            params.insert("season_id".to_string(), sid.to_string());
        }
        if let Some(ep) = ep_id {
            params.insert("ep_id".to_string(), ep.to_string());
        }

        let url = "https://api.bilibili.com/pugv/view/web/season/v2";
        let referer = "https://www.bilibili.com/cheese";
        debug!(url, season_id = ?season_id, ep_id = ?ep_id, "B站 API 请求: get_cheese_season_info");

        let request = self
            .build_get_request(url, &params, referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let mut season = self
            .parse_data::<CheeseSeason>(resp, "get_cheese_season_info")
            .await?;
        season.cover = super::models::user::normalize_image_url(&season.cover);
        Ok(season)
    }

    /// 获取课程分集的播放流（`/pugv/player/web/playurl`）。
    /// 响应主体在 `data` 字段；参数用 `avid` + `cid` + `ep_id`（B 站课程接口要求 avid 而非 bvid）。
    /// `fnval=16` 仅取 DASH 基础流（课程接口不支持 HDR/杜比/Hi-Res 位）。
    pub async fn get_cheese_playurl(
        &self,
        ep_id: u64,
        aid: i64,
        cid: i64,
        qn: i32,
        cookies: &str,
    ) -> Result<CheesePlayurlData> {
        let enriched = self.enrich_cookies(cookies).await?;
        let mut params = HashMap::new();
        params.insert("avid".to_string(), aid.to_string());
        params.insert("cid".to_string(), cid.to_string());
        params.insert("qn".to_string(), qn.to_string());
        params.insert("fnval".to_string(), "16".to_string());
        params.insert("fnver".to_string(), "0".to_string());
        params.insert("fourk".to_string(), "1".to_string());
        params.insert("ep_id".to_string(), ep_id.to_string());

        let url = "https://api.bilibili.com/pugv/player/web/playurl";
        let referer = format!("https://www.bilibili.com/cheese/play/ep{ep_id}");
        debug!(url, ep_id, aid, cid, qn, "B站 API 请求: get_cheese_playurl");

        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        self.parse_data::<CheesePlayurlData>(resp, "get_cheese_playurl")
            .await
    }

    /// 解析课程分集的可下载流集合，结构与 `get_video_urls` 对齐。
    /// `dash` 缺失（未购买课程 / 区域限制）时返回空 qualities，调用方按 pay_blocked 处理。
    pub async fn get_cheese_video_urls(
        &self,
        ep_id: u64,
        aid: i64,
        cid: i64,
        cookies: &str,
        preferred_quality: Option<i32>,
    ) -> Result<VideoStreams> {
        let qn_value = preferred_quality.unwrap_or(80);
        let data = self
            .get_cheese_playurl(ep_id, aid, cid, qn_value, cookies)
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
            let available_qualities = qualities.iter().map(|q| i64::from(q.quality)).collect();
            return Ok(VideoStreams {
                cid,
                qualities,
                selected_quality: None,
                available_qualities,
                accept_quality: data.accept_quality,
            });
        }

        if data.durl.is_some() {
            // 类型化错误让真实文案直达前端，不再被归为 500 内部错误（与普通视频一致）
            return Err(
                anyhow!(super::video_stream::StreamUnavailableError::unsupported(
                "该课程分集仅提供 FLV 分段流（durl），暂不支持下载。请等待 DASH 流恢复或联系开发者",
            )),
            );
        }

        Ok(VideoStreams {
            cid,
            qualities: Vec::new(),
            selected_quality: None,
            available_qualities: Vec::new(),
            accept_quality: data.accept_quality,
        })
    }
}
