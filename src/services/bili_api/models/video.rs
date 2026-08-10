//! 视频详情（`/x/web-interface/wbi/view`）响应模型。

use serde::{Deserialize, Serialize};

/// 视频详情 data（只声明用到的字段）。
/// B 站原始字段为 `pubdate`，对外序列化沿用旧契约字段名 `created`。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct VideoInfo {
    pub cid: i64,
    pub title: String,
    pub duration: i64,
    pub owner: VideoOwner,
    pub stat: VideoStat,
    #[serde(rename = "created", alias = "pubdate")]
    pub created: i64,
    pub pic: String,
    pub rights: VideoRights,
    pub state: i64,
    /// 充电专属视频标识：充电视频的 rights.ugc_pay/pay 均为 0，必须用这两个字段判定
    pub is_upower_exclusive: bool,
    pub is_upower_play: bool,
    /// 分P列表：B 站 view 接口返回的 pages 数组。单P投稿仅含 1 项，多P投稿含多项。
    pub pages: Vec<Page>,
}

/// 视频分P信息（view 接口 pages 数组项，仅声明用到的字段）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Page {
    /// 该分P的 cid，取流/弹幕/评论均以此为准
    pub cid: i64,
    /// 分P序号（从 1 开始）
    pub page: i32,
    /// 分P标题
    pub part: String,
    /// 分P时长（秒）
    pub duration: i64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct VideoOwner {
    pub mid: i64,
    pub name: String,
    pub face: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct VideoStat {
    pub view: i64,
    pub danmaku: i64,
    pub reply: i64,
    pub favorite: i64,
    pub coin: i64,
    pub share: i64,
    pub like: i64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct VideoRights {
    pub ugc_pay: i64,
    pub pay: i64,
}

/// CC 字幕条目（player/wbi/v2 接口 data.subtitle.subtitles[] 项）。
/// - `lan`：语言代码（如 zh-CN、ai-zh）
/// - `lan_doc`：语言中文名（如"中文（自动生成）"）
/// - `subtitle_url`：字幕 JSON URL，可能为 `//` 开头的协议相对路径，需补 `https:`
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct SubtitleInfo {
    pub lan: String,
    pub lan_doc: String,
    pub subtitle_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_info_maps_pubdate_to_created() {
        let info: VideoInfo = serde_json::from_str(
            r#"{
                "cid": 123,
                "title": "测试视频",
                "duration": 60,
                "pubdate": 1700000000,
                "owner": {"mid": 42, "name": "up主", "face": "//i0.hdslb.com/f.jpg"},
                "stat": {"view": 999, "like": 10},
                "rights": {"ugc_pay": 1, "pay": 0},
                "state": 0
            }"#,
        )
        .expect("video info");
        assert_eq!(info.cid, 123);
        assert_eq!(info.created, 1_700_000_000);
        assert_eq!(info.owner.mid, 42);
        assert_eq!(info.stat.view, 999);
        assert_eq!(info.rights.ugc_pay, 1);
        // 缺失字段容错
        assert!(!info.is_upower_exclusive);
        assert_eq!(info.stat.coin, 0);

        // 序列化沿用旧契约字段名 created
        let value = serde_json::to_value(&info).expect("serialize video info");
        assert_eq!(value["created"], 1_700_000_000);
        assert!(value.get("pubdate").is_none());
    }

    #[test]
    fn video_info_tolerates_empty_payload() {
        let info: VideoInfo = serde_json::from_str("{}").expect("empty video info");
        assert_eq!(info.cid, 0);
        assert_eq!(info.state, 0);
        assert!(info.title.is_empty());
    }
}
