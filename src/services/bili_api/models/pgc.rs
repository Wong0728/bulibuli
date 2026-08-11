//! 番剧（PGC）季/集信息与 playurl 响应模型。
//!
//! 与普通视频不同，番剧接口响应主体在 `result` 字段而非 `data` 字段，
//! 由 `BiliApi::parse_result` 统一解析。playurl 的 dash/durl 结构与普通视频一致，
//! 直接复用 `PlayurlData`。

use serde::{Deserialize, Serialize};

use super::playurl::PlayurlData;

/// `/pgc/view/web/season` 的 result（只声明用到的字段）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PgcSeason {
    pub season_id: i64,
    /// 番剧标题（`title` 字段；`season_title` 同义，B 站两个字段都可能存在）
    pub title: String,
    #[serde(rename = "season_title")]
    pub season_title: String,
    pub cover: String,
    /// 正片分集列表（`result.episodes`）
    pub episodes: Vec<PgcEpisode>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PgcEpisode {
    /// 分集 ep_id（番剧分集主键）
    pub ep_id: i64,
    pub cid: i64,
    /// 番剧分集对应的 bvid（任务主键沿用 bvid 语义）
    pub bvid: String,
    pub aid: i64,
    /// 短标题（如 "第1话"）
    pub title: String,
    /// 长标题（如 "开端"）
    #[serde(rename = "long_title")]
    pub long_title: String,
    /// 分集时长（秒）
    pub duration: i64,
    /// 角标（如 "预告"/"会员"）
    pub badge: String,
    pub cover: String,
    pub link: String,
}

impl PgcEpisode {
    /// 对外展示用标题：优先 long_title，其次 title，最终回退 ep_id。
    pub fn display_title(&self) -> String {
        if !self.long_title.is_empty() {
            self.long_title.clone()
        } else if !self.title.is_empty() {
            self.title.clone()
        } else {
            format!("ep{}", self.ep_id)
        }
    }
}

impl PgcSeason {
    /// 季标题：B 站两个字段都可能存在，优先 title，回退 season_title。
    pub fn title(&self) -> String {
        if !self.title.is_empty() {
            self.title.clone()
        } else {
            self.season_title.clone()
        }
    }
}

/// `/pgc/player/web/playurl` 的 result：复用普通视频的 PlayurlData 结构。
pub type PgcPlayurlData = PlayurlData;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgc_season_parses_minimal_payload() {
        let season: PgcSeason = serde_json::from_str(
            r#"{
                "season_id": 28769,
                "title": "鬼灭之刃",
                "season_title": "鬼灭之刃",
                "cover": "//i0.hdslb.com/bfs/bangumi/abc.jpg",
                "episodes": [
                    {
                        "ep_id": 262060,
                        "cid": 74087791,
                        "bvid": "BV1Hx411r7tY",
                        "aid": 20200289,
                        "title": "第1话",
                        "long_title": "残酷",
                        "duration": 1427,
                        "badge": "",
                        "cover": "",
                        "link": "https://www.bilibili.com/bangumi/play/ep262060"
                    }
                ]
            }"#,
        )
        .expect("pgc season");
        assert_eq!(season.season_id, 28769);
        assert_eq!(season.episodes.len(), 1);
        let ep = &season.episodes[0];
        assert_eq!(ep.ep_id, 262060);
        assert_eq!(ep.cid, 74087791);
        assert_eq!(ep.bvid, "BV1Hx411r7tY");
        assert_eq!(ep.display_title(), "残酷");
    }

    #[test]
    fn pgc_episode_falls_back_to_title_when_long_title_empty() {
        let ep: PgcEpisode =
            serde_json::from_str(r#"{"ep_id": 1, "title": "第1话"}"#).expect("pgc episode");
        assert_eq!(ep.display_title(), "第1话");
    }

    #[test]
    fn pgc_episode_falls_back_to_ep_id_when_titles_empty() {
        let ep: PgcEpisode = serde_json::from_str(r#"{"ep_id": 7}"#).expect("pgc episode");
        assert_eq!(ep.display_title(), "ep7");
    }

    #[test]
    fn pgc_season_title_falls_back_to_season_title_when_title_empty() {
        let season: PgcSeason =
            serde_json::from_str(r#"{"season_id": 1, "season_title": "备用标题"}"#)
                .expect("pgc season");
        assert_eq!(season.title(), "备用标题");
    }
}
