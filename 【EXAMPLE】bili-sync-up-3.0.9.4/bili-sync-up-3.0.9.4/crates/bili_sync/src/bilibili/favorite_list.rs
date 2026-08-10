use anyhow::{anyhow, Context, Result};
use async_stream::try_stream;
use futures::Stream;
use serde_json::Value;
use tracing::warn;

use crate::bilibili::{BiliClient, Validate, VideoInfo};
pub struct FavoriteList<'a> {
    client: &'a BiliClient,
    fid: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct FavoriteListInfo {
    pub id: i64,
    pub title: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct Upper<T> {
    pub mid: T,
    pub name: String,
    pub face: String,
}
impl<'a> FavoriteList<'a> {
    pub fn new(client: &'a BiliClient, fid: String) -> Self {
        Self { client, fid }
    }

    pub async fn get_info(&self) -> Result<FavoriteListInfo> {
        let mut res = self
            .client
            .request(reqwest::Method::GET, "https://api.bilibili.com/x/v3/fav/folder/info")
            .await
            .query(&[("media_id", &self.fid)])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()?;
        Ok(serde_json::from_value(res["data"].take())?)
    }

    async fn get_videos(&self, page: u32) -> Result<Value> {
        self.client
            .request(reqwest::Method::GET, "https://api.bilibili.com/x/v3/fav/resource/list")
            .await
            .query(&[
                ("media_id", self.fid.as_str()),
                ("pn", page.to_string().as_str()),
                ("ps", "20"),
                ("order", "mtime"),
                ("type", "0"),
                ("tid", "0"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?
            .validate()
    }

    // 拿到收藏夹的所有权，返回一个收藏夹下的视频流
    pub fn into_video_stream(self) -> impl Stream<Item = Result<VideoInfo>> + 'a {
        try_stream! {
            let mut page = 1;
            loop {
                let mut videos = self
                    .get_videos(page)
                    .await
                    .with_context(|| format!("failed to get videos of favorite {} page {}", self.fid, page))?;

                let media_count = videos["data"]["info"]["media_count"].as_u64().unwrap_or(0);
                let medias = &mut videos["data"]["medias"];

                if medias.as_array().is_none_or(|v| v.is_empty()) {
                    if media_count > 0 {
                        // 统计显示有视频但medias为空，说明内容被B站API过滤
                        // 只记录警告，不抛出错误，正常结束扫描
                        warn!("收藏夹 {} 中的 {} 个视频被B站API过滤，无法通过API获取（可能是番剧、纪录片等特殊内容类型）", self.fid, media_count);
                        break;
                    } else {
                        // 正常的空页面情况
                        break;
                    }
                }
                let videos_info: Vec<VideoInfo> = serde_json::from_value(medias.take())
                    .with_context(|| format!("failed to parse videos of favorite {} page {}", self.fid, page))?;
                for video_info in videos_info {
                    yield video_info;
                }
                let has_more = &videos["data"]["has_more"];
                if let Some(v) = has_more.as_bool() {
                    if v {
                        page += 1;
                        continue;
                    }
                } else {
                    Err(anyhow!("has_more is not a bool"))?;
                }
                break;
            }
        }
    }
}
