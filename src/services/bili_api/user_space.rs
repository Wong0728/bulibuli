//! UP 主空间相关接口：投稿列表、合集/系列、用户搜索与用户信息（全程强类型）。

use crate::services::wbi;
use anyhow::Result;
use std::collections::HashMap;
use tracing::{debug, warn};

use super::models::user::{
    normalize_image_url, AccInfo, ArcSearchData, RelationStat, SearchTypeData, SearchedUser,
    SeasonsSeriesData, SeriesArchivesData, SeriesEntry, SeriesMeta, SeriesVideosPage, UserProfile,
    UserSearchPage, UserSeriesList, UserVideo, UserVideosPage,
};
use super::BiliApi;
use super::{VideoListCacheKey, VIDEO_LIST_CACHE_TTL};

impl BiliApi {
    pub async fn get_user_videos(
        &self,
        uid: i64,
        cookies: &str,
        limit: i32,
    ) -> Result<UserVideosPage> {
        self.get_user_videos_page(uid, cookies, 1, limit).await
    }

    /// 获取投稿列表的指定页。监控服务用它向后扫描直到命中检查点，
    /// 避免仅查询"最新 N 条"时在高频投稿期间漏档。
    ///
    /// 内置 30 秒 TTL 内存缓存：相同 (uid, page, page_size) 的查询在缓存有效期内
    /// 直接返回，避免短时间内重复发起 HTTP 请求。
    pub async fn get_user_videos_page(
        &self,
        uid: i64,
        cookies: &str,
        page: i32,
        page_size: i32,
    ) -> Result<UserVideosPage> {
        let cache_key: VideoListCacheKey = (uid, page, page_size);

        // 快速路径：读锁检查缓存命中
        {
            let cache = self.video_list_cache.read().await;
            if let Some((cached_page, fetched_at)) = cache.get(&cache_key) {
                if fetched_at.elapsed() < VIDEO_LIST_CACHE_TTL {
                    debug!(uid, page, page_size, "视频列表缓存命中");
                    return Ok(cached_page.clone());
                }
            }
        }

        debug!(
            "视频列表缓存未命中: uid={}, page={}, page_size={}，发起 HTTP 请求",
            uid, page, page_size
        );

        let enriched = self.enrich_cookies(cookies).await?;

        // B站投稿列表 ps 上限为 50，超出会被截断或返回 -400；钳制到合法区间并显式分页
        let ps = page_size.clamp(1, 50);
        let (img_key, sub_key) = self.get_wbi_keys(&enriched).await?;
        let mut params = HashMap::new();
        params.insert("mid".to_string(), uid.to_string());
        params.insert("pn".to_string(), page.max(1).to_string());
        params.insert("ps".to_string(), ps.to_string());
        params.insert("order".to_string(), "pubdate".to_string());
        params.insert("platform".to_string(), "web".to_string());
        params.insert("web_location".to_string(), "1550101".to_string());
        self.inject_risk_params(&mut params, ""); // web_location 已设置
        wbi::enc_wbi(&mut params, &img_key, &sub_key)?;

        let url = "https://api.bilibili.com/x/space/wbi/arc/search";
        let referer = format!("https://space.bilibili.com/{uid}");
        debug!(url, uid, page, ps, "B站 API 请求: get_user_videos");

        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let data: ArcSearchData = self.parse_data(resp, "get_user_videos").await?;
        let videos: Vec<UserVideo> = data.list.vlist.into_iter().map(UserVideo::from).collect();
        let result = UserVideosPage {
            videos,
            total: data.page.count,
            page: page.max(1),
            page_size: ps,
        };

        // 写入缓存
        {
            let mut cache = self.video_list_cache.write().await;
            // 顺便清理过期条目，避免缓存无限膨胀（只清理当前 key 同 uid 的过期项）
            cache.retain(|_, (_, fetched_at)| fetched_at.elapsed() < VIDEO_LIST_CACHE_TTL);
            cache.insert(cache_key, (result.clone(), std::time::Instant::now()));
        }

        Ok(result)
    }

    /// 获取 UP 主合集/系列列表。
    /// 接口：https://api.bilibili.com/x/polymer/web-space/seasons_series_list?mid={uid}
    pub async fn get_user_series(&self, uid: i64, cookies: &str) -> Result<UserSeriesList> {
        let enriched = self.enrich_cookies(cookies).await?;
        let mut params = HashMap::new();
        params.insert("mid".to_string(), uid.to_string());
        params.insert("page_num".to_string(), "1".to_string());
        params.insert("page_size".to_string(), "20".to_string());
        params.insert("web_location".to_string(), "333.1387".to_string());

        let url = "https://api.bilibili.com/x/polymer/web-space/seasons_series_list";
        let referer = format!("https://space.bilibili.com/{uid}/lists");
        debug!(url, uid, "B站 API 请求: get_user_series");

        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let data: SeasonsSeriesData = self.parse_data(resp, "get_user_series").await?;

        let mut series: Vec<SeriesEntry> = Vec::new();
        // 解析合集 (seasons_list)：以 season_id 为主键
        for item in data.items_lists.seasons_list {
            if item.meta.season_id != 0 {
                series.push(series_entry(&item.meta, item.meta.season_id, "season"));
            }
        }
        // 解析系列 (series_list)：以 series_id 为主键
        for item in data.items_lists.series_list {
            if item.meta.series_id != 0 {
                series.push(series_entry(&item.meta, item.meta.series_id, "series"));
            }
        }

        let total = series.len() as i64;
        Ok(UserSeriesList { series, total })
    }

    /// 获取合集/系列内的视频列表。
    /// 合集(season): https://api.bilibili.com/x/polymer/web-space/seasons_archives_list
    /// 系列(series): https://api.bilibili.com/x/series/archives
    /// 前端传 offset/limit，后端换算为 pn/ps（ps 上限 30）。
    pub async fn get_series_videos(
        &self,
        uid: i64,
        series_id: i64,
        collection_type: &str,
        cookies: &str,
        offset: Option<i32>,
        limit: Option<i32>,
    ) -> Result<SeriesVideosPage> {
        let enriched = self.enrich_cookies(cookies).await?;

        let limit = limit.unwrap_or(30).clamp(1, 30);
        let offset = offset.unwrap_or(0).max(0);
        let pn = (offset / limit) + 1;

        let is_season = collection_type == "season";

        let mut params = HashMap::new();
        params.insert("mid".to_string(), uid.to_string());

        let (url, api_name) = if is_season {
            // 合集类型使用 seasons_archives_list 接口
            params.insert("season_id".to_string(), series_id.to_string());
            params.insert("page_num".to_string(), pn.to_string());
            params.insert("page_size".to_string(), limit.to_string());
            params.insert("web_location".to_string(), "333.1387".to_string());
            (
                "https://api.bilibili.com/x/polymer/web-space/seasons_archives_list",
                "get_season_videos",
            )
        } else {
            // 系列类型使用 series/archives 接口
            params.insert("series_id".to_string(), series_id.to_string());
            params.insert("pn".to_string(), pn.to_string());
            params.insert("ps".to_string(), limit.to_string());
            params.insert("sort".to_string(), "desc".to_string());
            params.insert("only_normal".to_string(), "true".to_string());
            (
                "https://api.bilibili.com/x/series/archives",
                "get_series_videos",
            )
        };

        let referer = format!("https://space.bilibili.com/{uid}/lists");
        debug!(
            url,
            uid, series_id, pn, limit, collection_type, "B站 API 请求: {api_name}"
        );

        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let data: SeriesArchivesData = self.parse_data(resp, api_name).await?;

        let videos: Vec<UserVideo> = data
            .archives
            .into_iter()
            .map(|archive| UserVideo {
                url: format!("https://www.bilibili.com/video/{}", archive.bvid),
                title: archive.title,
                bvid: archive.bvid,
                aid: archive.aid,
                pic: super::models::user::normalize_image_url(&archive.pic),
                play: archive.stat.view,
                comment: archive.stat.reply,
                created: archive.pubdate,
                length: Self::seconds_to_length(archive.duration),
                description: archive.desc,
                is_charging_arc: false,
                series_name: None,
            })
            .collect();

        // 合集和系列的分页字段名相同
        let total = data.page.total;
        let has_more = (offset + videos.len() as i32) < total as i32;
        Ok(SeriesVideosPage {
            videos,
            total,
            offset,
            limit,
            has_more,
        })
    }

    /// 将秒数转换为 mm:ss / h:mm:ss 字符串（与 get_user_videos 的 length 字段风格一致）。
    fn seconds_to_length(seconds: i64) -> String {
        if seconds <= 0 {
            return String::new();
        }
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        let secs = seconds % 60;
        if hours > 0 {
            format!("{hours}:{minutes:02}:{secs:02}")
        } else {
            format!("{minutes}:{secs:02}")
        }
    }

    /// 按用户名搜索 B 站用户（用于博主搜索功能）。
    pub async fn search_users(
        &self,
        keyword: &str,
        cookies: &str,
        page: u32,
        page_size: u32,
    ) -> Result<UserSearchPage> {
        let enriched = self.enrich_cookies(cookies).await?;
        let (img_key, sub_key) = self.get_wbi_keys(&enriched).await?;
        let mut params = HashMap::new();
        params.insert("search_type".to_string(), "bili_user".to_string());
        params.insert("keyword".to_string(), keyword.to_string());
        params.insert("page".to_string(), page.to_string());
        params.insert("page_size".to_string(), page_size.to_string());
        self.inject_risk_params(&mut params, "333.999");
        wbi::enc_wbi(&mut params, &img_key, &sub_key)?;

        let url = "https://api.bilibili.com/x/web-interface/wbi/search/type";
        let referer = format!("https://search.bilibili.com/upuser?keyword={keyword}");
        debug!(url, keyword, page, page_size, "B站 API 请求: search_users");
        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let data: SearchTypeData = self.parse_data(resp, "search_users").await?;
        let users: Vec<SearchedUser> = data.result.into_iter().map(SearchedUser::from).collect();
        Ok(UserSearchPage {
            users,
            total: data.page_info.total_results,
        })
    }

    /// 按 UID 获取用户信息（用于添加博主时的 UID 校验）。
    pub async fn get_user_info(&self, uid: i64, cookies: &str) -> Result<UserProfile> {
        let enriched = self.enrich_cookies(cookies).await?;
        let (img_key, sub_key) = self.get_wbi_keys(&enriched).await?;
        let mut params = HashMap::new();
        params.insert("mid".to_string(), uid.to_string());
        self.inject_risk_params(&mut params, "333.999");
        wbi::enc_wbi(&mut params, &img_key, &sub_key)?;

        let url = "https://api.bilibili.com/x/space/wbi/acc/info";
        let referer = format!("https://space.bilibili.com/{uid}");
        debug!(url, uid, "B站 API 请求: get_user_info");
        let request = self
            .build_get_request(url, &params, &referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        let info: AccInfo = self.parse_data(resp, "get_user_info").await?;
        let face = normalize_image_url(&info.face);

        // 获取粉丝数（使用 WBI 签名 + build_get_request，避免风控）
        let mut fans: i64 = 0;
        {
            let mut stat_params = HashMap::new();
            stat_params.insert("vmid".to_string(), uid.to_string());
            self.inject_risk_params(&mut stat_params, "333.999");
            let (img_key, sub_key) = self.get_wbi_keys(&enriched).await?;
            wbi::enc_wbi(&mut stat_params, &img_key, &sub_key)?;
            let stat_url = "https://api.bilibili.com/x/relation/stat";
            let request = self
                .build_get_request(stat_url, &stat_params, &referer, &enriched)
                .await;
            match self.send_with_retry(request).await {
                Ok(resp) => match self
                    .parse_data::<RelationStat>(resp, "get_relation_stat")
                    .await
                {
                    Ok(stat) => fans = stat.follower,
                    Err(e) => warn!("解析 relation/stat 响应失败 uid={uid}: {e}"),
                },
                Err(e) => warn!("请求 relation/stat 失败 uid={uid}: {e}"),
            }
        }

        Ok(UserProfile {
            exists: true,
            uid: info.mid,
            name: info.name,
            face,
            sign: info.sign,
            level: info.level,
            fans,
        })
    }
}

/// 把合集/系列 meta 归一化为对外条目。
fn series_entry(meta: &SeriesMeta, id: i64, kind: &str) -> SeriesEntry {
    SeriesEntry {
        id,
        series_id: id,
        kind: kind.to_string(),
        name: meta.name.clone(),
        title: meta.name.clone(),
        description: meta.description.clone(),
        cover: super::models::user::normalize_image_url(&meta.cover),
        total: meta.total,
        count: meta.total,
    }
}
