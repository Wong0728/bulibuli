//! 课程（Cheese / PUGV）季/集信息与 playurl 响应模型。
//!
//! 课程接口响应主体在 `data` 字段（与番剧的 `result` 不同），由 `BiliApi::parse_data`
//! 统一解析。课程分集通过 `sections[].episodes[]` 嵌套返回，这里展平为统一列表。

use serde::{Deserialize, Serialize};

use super::playurl::PlayurlData;

/// `/pugv/view/web/season/v2` 的 data（只声明用到的字段）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CheeseSeason {
    pub season_id: i64,
    pub title: String,
    pub cover: String,
    /// 课程章节（每章含若干分集），调用方按章节顺序展平
    pub sections: Vec<CheeseSection>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CheeseSection {
    pub title: String,
    pub episodes: Vec<CheeseEpisode>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CheeseEpisode {
    /// 课程分集 ep_id（B 站字段名为 `id`，这里 rename 为 ep_id 便于与番剧统一）
    #[serde(rename = "id")]
    pub ep_id: i64,
    pub aid: i64,
    pub cid: i64,
    /// 课程分集对应的 bvid（部分历史响应缺失，调用方需做空值兜底）
    pub bvid: String,
    pub title: String,
    #[serde(rename = "release_date")]
    pub release_date: i64,
    pub duration: i64,
    /// 0/1/2/3：状态/付费标识，调用方据此判定是否可试看
    pub status: i64,
}

impl CheeseSeason {
    /// 按章节顺序展平所有分集，同时携带所属章节标题，便于前端分组展示与批量下载。
    /// 返回 `(section_title, episode)` 引用元组，调用方据此构造分集列表。
    pub fn flatten_episodes(&self) -> Vec<(&str, &CheeseEpisode)> {
        self.sections
            .iter()
            .flat_map(|s| s.episodes.iter().map(|e| (s.title.as_str(), e)))
            .collect()
    }
}

/// `/pugv/player/web/playurl` 的 data：复用普通视频的 PlayurlData 结构。
pub type CheesePlayurlData = PlayurlData;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheese_season_parses_and_flattens_sections() {
        let season: CheeseSeason = serde_json::from_str(
            r#"{
                "season_id": 422,
                "title": "Rust 全栈课程",
                "cover": "//i0.hdslb.com/abc.jpg",
                "sections": [
                    {
                        "title": "第1章 入门",
                        "episodes": [
                            {"id": 6677, "aid": 123, "cid": 456, "bvid": "BV1xx411c7mD", "title": "环境搭建", "release_date": 1700000000, "duration": 600, "status": 0}
                        ]
                    },
                    {
                        "title": "第2章 进阶",
                        "episodes": [
                            {"id": 6678, "aid": 124, "cid": 457, "bvid": "BV1xx411c7mE", "title": "异步编程", "release_date": 1700100000, "duration": 720, "status": 2}
                        ]
                    }
                ]
            }"#,
        )
        .expect("cheese season");
        assert_eq!(season.season_id, 422);
        let eps = season.flatten_episodes();
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0].1.ep_id, 6677);
        assert_eq!(eps[0].0, "第1章 入门");
        assert_eq!(eps[1].1.bvid, "BV1xx411c7mE");
        assert_eq!(eps[1].0, "第2章 进阶");
    }

    #[test]
    fn cheese_season_tolerates_empty_sections() {
        let season: CheeseSeason = serde_json::from_str(r#"{"season_id": 1}"#).expect("minimal");
        assert_eq!(season.season_id, 1);
        assert!(season.flatten_episodes().is_empty());
    }
}
