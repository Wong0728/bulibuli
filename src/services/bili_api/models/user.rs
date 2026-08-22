//! UP 主空间接口响应模型：投稿列表 / 合集系列 / 用户搜索 / 用户信息。

use super::lenient_i64;
use serde::{Deserialize, Serialize};

// --- 投稿列表：/x/space/wbi/arc/search ---

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ArcSearchData {
    pub list: ArcSearchList,
    pub page: PageCount,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ArcSearchList {
    pub vlist: Vec<Vlist>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Vlist {
    pub title: String,
    pub bvid: String,
    pub aid: i64,
    pub pic: String,
    #[serde(deserialize_with = "lenient_i64")]
    pub play: i64,
    #[serde(deserialize_with = "lenient_i64")]
    pub comment: i64,
    pub created: i64,
    pub length: String,
    pub description: String,
    /// 充电专属标识：投稿列表自带 is_charging_arc（bool），elec_arc_type==1 兜底
    pub is_charging_arc: bool,
    pub elec_arc_type: i64,
}

impl Vlist {
    pub fn is_charging(&self) -> bool {
        self.is_charging_arc || self.elec_arc_type == 1
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PageCount {
    pub count: i64,
}

// --- 合集/系列：/x/polymer/web-space/seasons_series_list ---

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeasonsSeriesData {
    pub items_lists: ItemsLists,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ItemsLists {
    pub seasons_list: Vec<SeriesItem>,
    pub series_list: Vec<SeriesItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeriesItem {
    pub meta: SeriesMeta,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeriesMeta {
    pub season_id: i64,
    pub series_id: i64,
    pub name: String,
    pub description: String,
    pub cover: String,
    pub total: i64,
}

// --- 合集/系列视频：seasons_archives_list / series/archives ---

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SeriesArchivesData {
    pub archives: Vec<Archive>,
    pub page: PageTotal,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Archive {
    pub title: String,
    pub bvid: String,
    pub aid: i64,
    pub pic: String,
    pub stat: ArchiveStat,
    pub pubdate: i64,
    pub duration: i64,
    pub desc: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ArchiveStat {
    pub view: i64,
    pub reply: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PageTotal {
    pub total: i64,
}

// --- 用户搜索：/x/web-interface/wbi/search/type ---

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchTypeData {
    pub result: Vec<SearchUser>,
    #[serde(rename = "pageInfo")]
    pub page_info: SearchPageInfo,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchUser {
    pub mid: i64,
    pub uname: String,
    pub upic: String,
    #[serde(deserialize_with = "lenient_i64")]
    pub fans: i64,
    pub level: i64,
    pub usign: String,
    #[serde(deserialize_with = "lenient_i64")]
    pub videos: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchPageInfo {
    #[serde(rename = "totalResults")]
    pub total_results: i64,
}

// --- 用户信息：/x/space/wbi/acc/info + /x/relation/stat ---

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AccInfo {
    pub mid: i64,
    pub name: String,
    pub face: String,
    pub sign: String,
    pub level: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RelationStat {
    pub follower: i64,
}

/// 协议相对地址补全（`//i0.hdslb.com/...` → `https://...`）。
pub(crate) fn normalize_image_url(url: &str) -> String {
    crate::services::bili_url_policy::normalize_syntax(url)
        .map(|url| url.to_string())
        .unwrap_or_default()
}

/// 图床 URL 的"同图"判断：B 站同一张头像/封面会在 i0/i1/i2.hdslb.com
/// 之间轮换，整串字符串比较会把同一张图误判为已更换（改名/改头像误报）。
/// 这里剥掉协议与主机，只比较路径+查询串（忽略大小写）。
pub(crate) fn same_image_url(a: Option<&str>, b: Option<&str>) -> bool {
    fn path_part(url: &str) -> &str {
        let no_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
        no_scheme
            .split_once('/')
            .map_or(no_scheme, |(_, path)| path)
    }
    match (a, b) {
        (Some(a), Some(b)) => path_part(a).eq_ignore_ascii_case(path_part(b)),
        (None, None) => true,
        _ => false,
    }
}

// --- 对外域模型（序列化字段名与前端契约一致） ---

/// 投稿/合集视频条目。
#[derive(Debug, Clone, Default, Serialize)]
pub struct UserVideo {
    pub title: String,
    pub bvid: String,
    pub aid: i64,
    pub url: String,
    pub pic: String,
    pub play: i64,
    pub comment: i64,
    pub created: i64,
    pub length: String,
    pub description: String,
    pub is_charging_arc: bool,
    /// 合集名称；投稿列表未提供时为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_name: Option<String>,
}

impl From<Vlist> for UserVideo {
    fn from(v: Vlist) -> Self {
        let url = format!("https://www.bilibili.com/video/{}", v.bvid);
        let is_charging = v.is_charging();
        Self {
            title: v.title,
            bvid: v.bvid,
            aid: v.aid,
            url,
            pic: normalize_image_url(&v.pic),
            play: v.play,
            comment: v.comment,
            created: v.created,
            length: v.length,
            description: v.description,
            is_charging_arc: is_charging,
            series_name: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UserVideosPage {
    pub videos: Vec<UserVideo>,
    pub total: i64,
    pub page: i32,
    pub page_size: i32,
    pub offset: i32,
    pub has_more: bool,
}

/// 合集/系列条目（season/series 归一化）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct SeriesEntry {
    pub id: i64,
    pub series_id: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub cover: String,
    pub total: i64,
    pub count: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UserSeriesList {
    pub series: Vec<SeriesEntry>,
    pub total: i64,
    pub has_more: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SeriesVideosPage {
    pub videos: Vec<UserVideo>,
    pub total: i64,
    pub offset: i32,
    pub limit: i32,
    pub has_more: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchedUser {
    pub mid: i64,
    pub uname: String,
    pub upic: String,
    pub fans: i64,
    pub level: i64,
    pub sign: String,
    pub videos: i64,
}

impl From<SearchUser> for SearchedUser {
    fn from(u: SearchUser) -> Self {
        Self {
            mid: u.mid,
            uname: u.uname,
            upic: normalize_image_url(&u.upic),
            fans: u.fans,
            level: u.level,
            sign: u.usign,
            videos: u.videos,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UserSearchPage {
    pub users: Vec<SearchedUser>,
    pub total: i64,
}

/// 用户信息（acc/info + relation/stat 合并后的域模型）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct UserProfile {
    pub exists: bool,
    pub uid: i64,
    pub name: String,
    pub face: String,
    pub sign: String,
    pub level: i64,
    pub fans: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlist_charging_detection_uses_both_fields() {
        let charging: Vlist =
            serde_json::from_str(r#"{"bvid": "BV1", "is_charging_arc": true}"#).expect("vlist");
        assert!(charging.is_charging());
        let elec: Vlist =
            serde_json::from_str(r#"{"bvid": "BV2", "elec_arc_type": 1}"#).expect("vlist");
        assert!(elec.is_charging());
        let normal: Vlist = serde_json::from_str(r#"{"bvid": "BV3"}"#).expect("vlist");
        assert!(!normal.is_charging());
    }

    #[test]
    fn arc_search_parses_play_and_comment_as_lenient_numbers() {
        let data: ArcSearchData = serde_json::from_str(
            r#"{
                "list": {"vlist": [{"bvid": "BV1", "play": "1234", "comment": 5}]},
                "page": {"count": 100}
            }"#,
        )
        .expect("arc search");
        assert_eq!(data.list.vlist[0].play, 1234);
        assert_eq!(data.list.vlist[0].comment, 5);
        assert_eq!(data.page.count, 100);
    }

    #[test]
    fn search_user_maps_page_info_alias() {
        let data: SearchTypeData = serde_json::from_str(
            r#"{"result": [{"mid": 7, "uname": "u", "fans": "999"}], "pageInfo": {"totalResults": 3}}"#,
        )
        .expect("search type");
        assert_eq!(data.result[0].fans, 999);
        assert_eq!(data.page_info.total_results, 3);
    }

    #[test]
    fn normalize_image_url_adds_scheme() {
        assert_eq!(
            normalize_image_url("//i0.hdslb.com/a.jpg"),
            "https://i0.hdslb.com/a.jpg"
        );
        assert_eq!(
            normalize_image_url("https://i0.hdslb.com/a.jpg"),
            "https://i0.hdslb.com/a.jpg"
        );
        assert_eq!(normalize_image_url(""), "");
    }

    #[test]
    fn user_video_from_vlist_builds_url_and_charging_flag() {
        let vlist: Vlist =
            serde_json::from_str(r#"{"bvid": "BV1xx411c7mD", "title": "t", "elec_arc_type": 1}"#)
                .expect("vlist");
        let video = UserVideo::from(vlist);
        assert_eq!(video.url, "https://www.bilibili.com/video/BV1xx411c7mD");
        assert!(video.is_charging_arc);
        // series_name 未设置时不序列化（与旧契约一致）
        let value = serde_json::to_value(&video).expect("serialize");
        assert!(value.get("series_name").is_none());
    }
}
