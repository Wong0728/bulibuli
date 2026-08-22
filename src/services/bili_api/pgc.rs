//! 番剧（PGC）接口客户端：季/集信息 + 取流。
//!
//! 与普通视频接口的差异：
//! - 季信息走 `/pgc/view/web/season`，响应在 `result` 字段。
//! - 取流走 `/pgc/player/web/playurl`，响应在 `result` 字段；参数用 `ep_id` + `cid`
//!   （普通视频用 `bvid` + `cid`），不需要 WBI 签名。
//! - 大会员专享内容在取流阶段返回 `code != 0` 或空 dash，调用方据此判定 pay_blocked。

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use tracing::debug;

use super::models::pgc::{PgcPlayurlData, PgcSeason};
use super::models::playurl::{StreamQuality, VideoStreams};
use super::{BiliApi, QUALITY_NAMES};

impl BiliApi {
    /// 获取番剧季信息（`/pgc/view/web/season`）。
    /// - `ep_id`：从 ep 链接解析得到，传入时 B 站会反查所属 season 并返回全部 episodes。
    /// - `season_id`：从 ss 链接解析得到，直接按季查询。
    ///
    /// 二者至少传一个；同时传时 B 站以 ep_id 优先。
    pub async fn get_pgc_season_info(
        &self,
        ep_id: Option<u64>,
        season_id: Option<u64>,
        cookies: &str,
    ) -> Result<PgcSeason> {
        if ep_id.is_none() && season_id.is_none() {
            return Err(anyhow!("ep_id 与 season_id 至少需要传一个"));
        }
        let enriched = self.enrich_cookies(cookies).await?;
        let mut params = HashMap::new();
        if let Some(ep) = ep_id {
            params.insert("ep_id".to_string(), ep.to_string());
        }
        if let Some(sid) = season_id {
            params.insert("season_id".to_string(), sid.to_string());
        }

        let url = "https://api.bilibili.com/pgc/view/web/season";
        let referer = "https://www.bilibili.com/bangumi";
        debug!(url, ep_id = ?ep_id, season_id = ?season_id, "B站 API 请求: get_pgc_season_info");

        let request = self
            .build_get_request(url, &params, referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let mut season = self
            .parse_result::<PgcSeason>(resp, "get_pgc_season_info")
            .await?;
        season.cover = super::models::user::normalize_image_url(&season.cover);
        for episode in &mut season.episodes {
            episode.cover = super::models::user::normalize_image_url(&episode.cover);
        }
        Ok(season)
    }

    /// 获取番剧分集的播放流（`/pgc/player/web/playurl`）。
    /// 响应主体在 `result` 字段，dash/durl 结构与普通视频一致，复用 PlayurlData。
    /// `qn` 为期望画质（如 127=8K，80=1080P）；`fnval=4048` 与普通视频一致覆盖全量 DASH。
    pub async fn get_pgc_playurl(
        &self,
        ep_id: u64,
        cid: i64,
        qn: i32,
        fnval: i32,
        cookies: &str,
    ) -> Result<PgcPlayurlData> {
        let enriched = self.enrich_cookies(cookies).await?;
        let mut params = HashMap::new();
        params.insert("ep_id".to_string(), ep_id.to_string());
        params.insert("cid".to_string(), cid.to_string());
        params.insert("qn".to_string(), qn.to_string());
        params.insert("fnval".to_string(), fnval.to_string());
        params.insert("fnver".to_string(), "0".to_string());
        params.insert("fourk".to_string(), "1".to_string());
        params.insert("platform".to_string(), "web".to_string());

        let url = "https://api.bilibili.com/pgc/player/web/playurl";
        let referer = format!("https://www.bilibili.com/bangumi/play/ep{ep_id}");
        debug!(url, ep_id, cid, qn, fnval, "B站 API 请求: get_pgc_playurl");

        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        self.parse_result::<PgcPlayurlData>(resp, "get_pgc_playurl")
            .await
    }

    /// 解析番剧分集的可下载流集合，结构与 `get_video_urls` 对齐，便于下游复用。
    /// `dash` 缺失（大会员专享 / 未购买 / 区域限制）时返回空 qualities，调用方按 pay_blocked 处理。
    pub async fn get_pgc_video_urls(
        &self,
        ep_id: u64,
        cid: i64,
        cookies: &str,
        fnval: i32,
        preferred_quality: Option<i32>,
    ) -> Result<VideoStreams> {
        let qn_value = preferred_quality.unwrap_or(127);
        let fnval_value = if fnval <= 0 { 4048 } else { fnval };
        let data = self
            .get_pgc_playurl(ep_id, cid, qn_value, fnval_value, cookies)
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

        // durl 分支同样拒绝（与普通视频一致），dash/durl 均缺失视为无权限取流。
        // 类型化错误让真实文案直达前端，不再被归为 500 内部错误。
        if data.durl.is_some() {
            return Err(
                anyhow!(super::video_stream::StreamUnavailableError::unsupported(
                "该番剧分集仅提供 FLV 分段流（durl），暂不支持下载。请等待 DASH 流恢复或联系开发者",
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
