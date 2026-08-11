//! 直播间 API 响应模型：新版 getRoomPlayInfo / getDanmuInfo 与旧版兼容接口。
//!
//! `getDanmuInfo` 自 2025 年起必须使用有效登录态和 WBI 签名；播放接口是否
//! 是否需要登录由 B 站当前策略决定。
//! 只声明用到的字段，配合 `#[serde(default)]` 容忍缺失。

use serde::Deserialize;
use std::collections::HashMap;

/// `get_status_info_by_uids` 的批量直播状态条目。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LiveBatchStatus {
    pub room_id: i64,
    pub uid: i64,
    pub live_status: i32,
    pub live_time: i64,
}

pub type LiveBatchStatusMap = HashMap<String, LiveBatchStatus>;

/// `room_init` 响应：短号换长号 + 直播状态。
///
/// `GET https://api.live.bilibili.com/room/v1/Room/room_init?id={id}`
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LiveRoomInit {
    pub room_id: i64,
    pub short_id: i64,
    pub uid: i64,
    pub live_status: i32,
    pub encrypted: bool,
    pub live_time: i64,
}

impl Default for LiveRoomInit {
    fn default() -> Self {
        Self {
            room_id: 0,
            short_id: 0,
            uid: 0,
            live_status: 0,
            encrypted: false,
            live_time: -62_170_012_800,
        }
    }
}

impl LiveRoomInit {
    /// 是否正在直播（live_status == 1）。
    pub fn is_live(&self) -> bool {
        self.live_status == 1
    }

    /// 是否轮播中（live_status == 2）。
    #[allow(dead_code)]
    pub fn is_replay(&self) -> bool {
        self.live_status == 2
    }
}

/// `get_info` 响应：直播间详细信息（标题/主播/封面/分区）。
///
/// `GET https://api.live.bilibili.com/room/v1/Room/get_info?room_id={room_id}`
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LiveRoomInfo {
    pub uid: i64,
    pub room_id: i64,
    pub short_id: i64,
    pub title: String,
    pub live_status: i32,
    pub online: i64,
    pub user_cover: String,
    pub keyframe: String,
    pub area_name: String,
    pub parent_area_name: String,
    pub live_time: String,
    pub description: String,
    pub tags: String,
    pub is_portrait: bool,
}

/// `playUrl` 响应：直播流地址（旧版，无需签名）。
///
/// `GET https://api.live.bilibili.com/room/v1/Room/playUrl?cid={room_id}&qn=10000&platform=web`
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LivePlayUrl {
    pub current_quality: i32,
    pub accept_quality: Vec<String>,
    pub quality_description: Vec<LiveQualityDesc>,
    pub durl: Vec<LiveStreamUrl>,
}

/// 新版 `getRoomPlayInfo` 的播放地址树。
///
/// B 站会按协议、封装、编码和 CDN 线路分层返回 URL；这里保留完整的
/// 中间层，业务层再统一展平为 `LivePlayUrl.durl`，便于旧版 fallback 与
/// 录制器复用同一套线路选择逻辑。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LiveRoomPlayInfo {
    pub room_id: i64,
    pub live_status: i32,
    pub playurl_info: Option<LivePlayurlInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LivePlayurlInfo {
    pub playurl: LivePlayurl,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LivePlayurl {
    pub stream: Vec<LivePlayStream>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LivePlayStream {
    pub protocol_name: String,
    pub format: Vec<LivePlayFormat>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LivePlayFormat {
    pub format_name: String,
    pub codec: Vec<LivePlayCodec>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LivePlayCodec {
    pub codec_name: String,
    pub current_qn: i32,
    pub accept_qn: Vec<i32>,
    pub base_url: String,
    pub url_info: Vec<LiveUrlInfo>,
    pub durl: Vec<LiveStreamUrl>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LiveUrlInfo {
    pub host: String,
    pub extra: String,
    pub stream_ttl: i64,
}

/// 清晰度描述。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LiveQualityDesc {
    pub qn: i32,
    pub desc: String,
}

/// 流地址条目（多线路 CDN）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LiveStreamUrl {
    pub url: String,
    pub order: i32,
    pub stream_type: i32,
    pub protocol_name: String,
    pub format_name: String,
    pub codec_name: String,
    pub current_qn: i32,
}

/// `getDanmuInfo` 响应：弹幕 WebSocket 连接信息。
/// 新版接口要求有效登录态和 WBI 签名；旧版 `getConf` 不再作为生产回退。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LiveDanmuConf {
    pub token: String,
    #[serde(alias = "host_list")]
    pub host_server_list: Vec<LiveDanmuHost>,
}

/// 弹幕 WebSocket 服务器地址。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LiveDanmuHost {
    pub host: String,
    pub port: i32,
    pub wss_port: i32,
    pub ws_port: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_init_tolerates_missing_fields() {
        let init: LiveRoomInit =
            serde_json::from_str(r#"{"room_id": 14073662, "short_id": 76, "uid": 50333369}"#)
                .expect("room_init");
        assert_eq!(init.room_id, 14073662);
        assert_eq!(init.short_id, 76);
        assert!(!init.is_live());
    }

    #[test]
    fn room_init_detects_live_status() {
        let live: LiveRoomInit =
            serde_json::from_str(r#"{"room_id": 100, "live_status": 1}"#).expect("live");
        assert!(live.is_live());
        assert!(!live.is_replay());

        let replay: LiveRoomInit =
            serde_json::from_str(r#"{"room_id": 100, "live_status": 2}"#).expect("replay");
        assert!(replay.is_replay());
    }

    #[test]
    fn playurl_parses_durl_array() {
        let json = r#"{
            "current_quality": 4,
            "accept_quality": ["4", "3", "2"],
            "quality_description": [{"qn": 10000, "desc": "原画"}],
            "durl": [
                {"url": "https://cdn-a/live.flv?expires=123&sign=abc", "order": 1, "stream_type": 0},
                {"url": "https://cdn-b/live.flv", "order": 2, "stream_type": 0}
            ]
        }"#;
        let playurl: LivePlayUrl = serde_json::from_str(json).expect("playurl");
        assert_eq!(playurl.durl.len(), 2);
        assert_eq!(playurl.durl[0].order, 1);
        assert!(playurl.durl[0].url.contains("expires=123"));
    }

    #[test]
    fn danmu_conf_parses_hosts() {
        let json = r#"{
            "token": "test_token",
            "host_server_list": [
                {"host": "broadcastlv.chat.bilibili.com", "port": 2243, "wss_port": 443, "ws_port": 2244}
            ]
        }"#;
        let conf: LiveDanmuConf = serde_json::from_str(json).expect("danmu conf");
        assert_eq!(conf.token, "test_token");
        assert_eq!(conf.host_server_list.len(), 1);
        assert_eq!(conf.host_server_list[0].wss_port, 443);
    }

    #[test]
    fn room_play_info_parses_nested_stream_tree() {
        let json = r#"{
            "room_id": 100,
            "live_status": 1,
            "playurl_info": {"playurl": {"stream": [{
                "protocol_name": "http_hls",
                "format": [{"format_name": "flv", "codec": [{
                    "codec_name": "avc",
                    "current_qn": 10000,
                    "base_url": "/live-bvc/stream.flv",
                    "url_info": [{"host": "https://cdn.example.com", "extra": "?deadline=123"}],
                    "durl": [{"url": "https://cdn.example.com/live.flv", "order": 1}]
                }]}]
            }]}}
        }"#;
        let info: LiveRoomPlayInfo = serde_json::from_str(json).expect("getRoomPlayInfo");
        assert_eq!(info.room_id, 100);
        let playurl = info.playurl_info.expect("playurl info");
        assert_eq!(playurl.playurl.stream.len(), 1);
        assert_eq!(playurl.playurl.stream[0].format[0].codec[0].durl.len(), 1);
    }

    #[test]
    fn room_play_info_accepts_offline_null_playurl() {
        let info: LiveRoomPlayInfo =
            serde_json::from_str(r#"{"room_id":100,"live_status":0,"playurl_info":null}"#)
                .expect("offline getRoomPlayInfo");
        assert!(info.playurl_info.is_none());
    }

    #[test]
    fn danmu_info_accepts_new_host_list_name() {
        let conf: LiveDanmuConf = serde_json::from_str(
            r#"{"token":"token","host_list":[{"host":"chat.example.com","wss_port":443}]}"#,
        )
        .expect("getDanmuInfo");
        assert_eq!(conf.token, "token");
        assert_eq!(conf.host_server_list[0].host, "chat.example.com");
    }
}
