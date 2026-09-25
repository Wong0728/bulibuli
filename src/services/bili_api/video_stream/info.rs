//! 视频详情与 CC 字幕：`/x/web-interface/wbi/view`（带会话级缓存）
//! 与 `/x/player/wbi/v2` 字幕列表，全程 WBI 签名。

use anyhow::Result;
use std::collections::HashMap;
use tracing::{debug, warn};

use crate::services::bili_api::models::user::normalize_image_url;
use crate::services::bili_api::models::video::{SubtitleInfo, VideoInfo};
use crate::services::bili_api::{session_fingerprint, BiliApi};
use crate::services::wbi;

/// 视频详情缓存有效期（5 分钟）：同一会话对同一 bv 的重复查询直接命中缓存。
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
        info.pic = normalize_image_url(&info.pic);
        info.owner.face = normalize_image_url(&info.owner.face);
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
        let list: Vec<SubtitleInfo> = match serde_json::from_value(subtitles_value) {
            Ok(list) => list,
            Err(error) => {
                // 结构漂移、或风控页伪装成正常 200 时，静默返回空列表会让前端显示
                // "暂无字幕"，用户与排查者都无从分辨"确实没有"和"解析失败"。
                warn!("字幕列表解析失败，按空列表处理: {error}");
                Vec::new()
            }
        };
        Ok(list)
    }
}
