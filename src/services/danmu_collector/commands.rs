//! 弹幕命令解析：将 B 站 WebSocket 推送的 JSON 命令转为类型化枚举。
//!
//! 覆盖主要命令类型：弹幕、礼物、进场、点赞、SC、看过人数、房间状态、PK/连麦、流地址刷新。
//! 未识别的命令统一归入 `Other`，不丢弃；展示层只会把确实没有分类规则的命令显示为未知。

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct IncomingLiveCommand {
    pub cmd: String,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub command: LiveCommand,
    pub raw: Value,
    pub history_backfill: bool,
}

impl IncomingLiveCommand {
    pub fn from_json(raw: Value) -> Self {
        let cmd = raw
            .get("cmd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let command = LiveCommand::from_json(&raw);
        Self {
            cmd,
            received_at: chrono::Utc::now(),
            command,
            raw,
            history_backfill: false,
        }
    }
}

/// 类型化的直播命令。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveCommand {
    /// 弹幕消息（`DANMU_MSG`）。
    Danmaku {
        uid: i64,
        uname: String,
        text: String,
        timestamp: i64,
        mode: i32,
        font_size: i32,
        color: i64,
        user_level: i32,
        medal_name: String,
        medal_level: i32,
    },
    /// 送礼（`SEND_GIFT`）。
    Gift {
        uid: i64,
        uname: String,
        gift_name: String,
        num: i32,
        price: i32,
        total_coin: i64,
        coin_type: String,
        gift_id: i64,
    },
    /// 醒目留言 / SC（`SUPER_CHAT_MESSAGE`）。
    SuperChat {
        uid: i64,
        uname: String,
        message: String,
        price: i32,
        duration: i32,
        id: String,
    },
    /// 进场 / 关注 / 分享（`INTERACT_WORD`）。
    Interact {
        uid: i64,
        uname: String,
        msg_type: i32,
    },
    /// 看过人数变化（`WATCHED_CHANGE`）。
    WatchedChange { count: i64 },
    /// 点赞（`LIKE_INFO_V3_CLICK`）。
    Like {
        uid: i64,
        uname: String,
        text: String,
    },
    /// 进场特效（`ENTRY_EFFECT`）。
    EntryEffect {
        uid: i64,
        uname: String,
        text: String,
    },
    /// 在线榜、点赞数等统计更新。
    Stats {
        label: String,
        value: i64,
        text: String,
    },
    /// 房间状态、公告、抽奖等系统事件。
    System { text: String },
    /// PK、连麦等互动状态事件。
    LinkMicPk { text: String },
    /// 直播开始（`LIVE`）。
    LiveStart { room_id: i64, live_time: i64 },
    /// 直播结束 / 准备中（`PREPARING`）。
    LiveEnd { room_id: String, round: i32 },
    /// 播放链接需要刷新（`PLAYURL_RELOAD`）。
    PlayurlReload,
    /// 上舰（`GUARD_BUY`）。
    GuardBuy {
        uid: i64,
        uname: String,
        guard_level: i32,
        price: i32,
        num: i32,
        order_id: String,
    },
    /// 未识别的命令。
    Other { cmd: String },
}

impl LiveCommand {
    /// 从 JSON 命令解析为类型化枚举。
    pub fn from_json(value: &Value) -> Self {
        let cmd = value
            .get("cmd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // B 站有时会在 cmd 后面加数字后缀（如 DANMU_MSG:4:0:2:2:2:0），
        // 取冒号前的部分做匹配。
        let cmd_base = command_base(&cmd);

        match cmd_base {
            "DANMU_MSG" => parse_danmaku(value),
            "SEND_GIFT" | "SEND_GIFT_V2" | "COMBO_SEND" => parse_gift(value),
            "SUPER_CHAT_MESSAGE" | "SUPER_CHAT_MESSAGE_JPN" => parse_super_chat(value),
            "INTERACT_WORD" | "INTERACT_WORD_V2" | "INTERACT_WORD_V3" | "WELCOME"
            | "WELCOME_GUARD" => parse_interact(value),
            "WATCHED_CHANGE" => parse_watched(value),
            "LIKE_INFO_V3_CLICK" => parse_like(value),
            "ENTRY_EFFECT" => parse_entry_effect(value),
            "ONLINE_RANK_V3" | "ONLINE_RANK_V2" | "ONLINE_RANK_TOP3" | "ONLINE_RANK_COUNT" => {
                parse_stats(value, "在线榜数据更新")
            }
            "LIKE_INFO_V3_UPDATE" => parse_stats(value, "点赞数更新"),
            "LIVE" => parse_live_start(value),
            "PREPARING" => parse_live_end(value),
            "PLAYURL_RELOAD" | "PLAYURL_RELOAD_V2" => LiveCommand::PlayurlReload,
            "GUARD_BUY" | "USER_TOAST_MSG" => parse_guard_buy(value),
            _ if is_link_command(cmd_base) => parse_link_mic(value, cmd_base),
            _ if is_stats_command(cmd_base) => parse_stats(value, "统计更新"),
            _ if is_system_command(cmd_base) => parse_system(value, system_event_label(cmd_base)),
            _ => LiveCommand::Other { cmd },
        }
    }

    pub fn is_low_priority(&self) -> bool {
        matches!(
            self,
            LiveCommand::WatchedChange { .. }
                | LiveCommand::Interact { .. }
                | LiveCommand::Like { .. }
                | LiveCommand::EntryEffect { .. }
                | LiveCommand::Stats { .. }
                | LiveCommand::System { .. }
                | LiveCommand::Other { .. }
        )
    }
}

pub fn command_base(cmd: &str) -> &str {
    cmd.split(':').next().unwrap_or(cmd)
}

pub fn is_link_command(cmd: &str) -> bool {
    let base = command_base(cmd);
    ["VOICE_JOIN", "LINK_MIC", "PK_", "LIVE_MULTI_VIEW"]
        .iter()
        .any(|prefix| base.starts_with(prefix))
}

pub fn is_stats_command(cmd: &str) -> bool {
    let base = command_base(cmd);
    matches!(
        base,
        "ONLINE_RANK_V3"
            | "ONLINE_RANK_V2"
            | "ONLINE_RANK_TOP3"
            | "ONLINE_RANK_COUNT"
            | "ROOM_REAL_TIME_MESSAGE_UPDATE"
            | "HOT_RANK_CHANGED"
            | "AREA_RANK_CHANGED"
    )
}

pub fn is_system_command(cmd: &str) -> bool {
    let base = command_base(cmd);
    matches!(
        base,
        "ROOM_CHANGE"
            | "ROOM_LOCK"
            | "ROOM_BLOCK_MSG"
            | "ROOM_SILENT_ON"
            | "ROOM_SILENT_OFF"
            | "CUT_OFF"
            | "STOP_LIVE_ROOM_LIST"
            | "NOTICE_MSG"
            | "COMMON_NOTICE_DANMAKU"
            | "DANMU_AGGREGATION"
            | "DM_INTERACTION"
            | "SUPER_CHAT_MESSAGE_DELETE"
            | "SUPER_CHAT_ENTRANCE"
            | "WIDGET_BANNER"
            | "LIVE_INTERACTIVE_GAME"
            | "GIFT_STAR_PROCESS"
            | "GUARD_HONOR_THOUSAND"
            | "RING_STATUS_CHANGE"
            | "RING_STATUS_CHANGE_V2"
            | "PLAY_TOGETHER_ICON_CHANGE"
    ) || base.starts_with("ANCHOR_LOT_")
        || base.starts_with("POPULARITY_RED_POCKET_")
}

pub fn system_event_label(cmd: &str) -> &'static str {
    match command_base(cmd) {
        "ROOM_CHANGE" => "房间信息更新",
        "ROOM_LOCK" => "房间状态更新",
        "ROOM_BLOCK_MSG" => "房间封禁通知",
        "ROOM_SILENT_ON" => "全员禁言开启",
        "ROOM_SILENT_OFF" => "全员禁言关闭",
        "CUT_OFF" => "直播被切断",
        "STOP_LIVE_ROOM_LIST" => "直播结束通知",
        "NOTICE_MSG" => "直播间公告",
        "COMMON_NOTICE_DANMAKU" => "系统公告",
        "DANMU_AGGREGATION" => "弹幕聚合更新",
        "DM_INTERACTION" => "弹幕互动更新",
        "SUPER_CHAT_MESSAGE_DELETE" => "SC 删除通知",
        "SUPER_CHAT_ENTRANCE" => "SC 入口更新",
        "WIDGET_BANNER" => "直播组件更新",
        "LIVE_INTERACTIVE_GAME" => "互动游戏更新",
        "GIFT_STAR_PROCESS" => "礼物星球更新",
        "GUARD_HONOR_THOUSAND" => "千舰荣耀更新",
        "RING_STATUS_CHANGE" | "RING_STATUS_CHANGE_V2" => "响铃状态更新",
        "PLAY_TOGETHER_ICON_CHANGE" => "一起玩状态更新",
        base if base.starts_with("ANCHOR_LOT_") => "天选时刻更新",
        base if base.starts_with("POPULARITY_RED_POCKET_") => "人气红包更新",
        _ => "系统事件",
    }
}

/// 解析弹幕消息。
///
/// `DANMU_MSG` 的 `info` 数组结构：
/// - info[0][15]["user"]["base"]["name"] → 用户名（新格式）
/// - info[0][15]["user"]["uid"] → UID（新格式）
/// - info[2][0] → UID（旧格式回退）
/// - info[2][1] → 用户名（旧格式回退）
/// - info[1] → 弹幕文本
/// - info[0][9]["ts"] → 发送时间戳（新格式）
/// - info[9]["ts"] → 发送时间戳（旧格式）
fn parse_danmaku(value: &Value) -> LiveCommand {
    let info = value.get("info");
    let text = info
        .and_then(|i| i.get(1))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (uid, uname) = extract_user_from_danmu(info);

    let timestamp = info
        .and_then(|i| i.get(0))
        .and_then(|i| i.get(9))
        .and_then(|v| v.get("ts"))
        .and_then(|v| v.as_i64())
        .or_else(|| {
            info.and_then(|i| i.get(9))
                .and_then(|v| v.get("ts"))
                .and_then(|v| v.as_i64())
        })
        .unwrap_or(0);

    let mode = info
        .and_then(|i| i.get(0))
        .and_then(|i| i.get(1))
        .and_then(Value::as_i64)
        .unwrap_or(1) as i32;
    let font_size = info
        .and_then(|i| i.get(0))
        .and_then(|i| i.get(2))
        .and_then(Value::as_i64)
        .unwrap_or(25) as i32;
    let color = info
        .and_then(|i| i.get(0))
        .and_then(|i| i.get(3))
        .and_then(Value::as_i64)
        .unwrap_or(16_777_215);
    let user_level = info
        .and_then(|i| i.get(4))
        .and_then(|i| i.get(0))
        .and_then(Value::as_i64)
        .unwrap_or(0) as i32;
    let medal_name = info
        .and_then(|i| i.get(3))
        .and_then(|i| i.get(1))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let medal_level = info
        .and_then(|i| i.get(3))
        .and_then(|i| i.get(0))
        .and_then(Value::as_i64)
        .unwrap_or(0) as i32;

    LiveCommand::Danmaku {
        uid,
        uname,
        text,
        timestamp,
        mode,
        font_size,
        color,
        user_level,
        medal_name,
        medal_level,
    }
}

/// 从 DANMU_MSG info 中提取用户信息，兼容新旧格式。
fn extract_user_from_danmu(info: Option<&Value>) -> (i64, String) {
    // 新格式：info[0][15]["user"]
    if let Some(info) = info {
        if let Some(user) = info
            .get(0)
            .and_then(|a| a.get(15))
            .and_then(|m| m.get("user"))
        {
            let uid = user.get("uid").and_then(|v| v.as_i64()).unwrap_or(0);
            let uname = user
                .get("base")
                .and_then(|b| b.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if uid != 0 || !uname.is_empty() {
                return (uid, uname);
            }
        }

        // 旧格式回退：info[2]
        if let Some(sender) = info.get(2) {
            let uid = sender.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let uname = sender
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            return (uid, uname);
        }
    }

    (0, String::new())
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
}

fn read_i64(value: Option<&Value>, keys: &[&str]) -> i64 {
    keys.iter()
        .find_map(|key| {
            value
                .and_then(|value| value.get(*key))
                .and_then(value_as_i64)
        })
        .unwrap_or(0)
}

fn read_string(value: Option<&Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| {
            value
                .and_then(|value| value.get(*key))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn parse_gift(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::Gift {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: read_string(data, &["uname", "username", "user_name"]),
        gift_name: read_string(data, &["giftName", "gift_name", "name"]),
        num: read_i64(data, &["num", "quantity"]) as i32,
        price: read_i64(data, &["price"]) as i32,
        total_coin: read_i64(data, &["total_coin", "totalCoin"]),
        coin_type: read_string(data, &["coin_type", "coinType"]),
        gift_id: read_i64(data, &["giftId", "gift_id"]),
    }
}

fn parse_super_chat(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::SuperChat {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: data
            .and_then(|data| data.get("user_info"))
            .map(|user_info| read_string(Some(user_info), &["uname", "username"]))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| read_string(data, &["uname", "username"])),
        message: read_string(data, &["message", "msg", "text"]),
        price: read_i64(data, &["price"]) as i32,
        duration: read_i64(data, &["time", "duration"]) as i32,
        id: read_string(data, &["id_str", "id"]),
    }
}

fn parse_interact(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::Interact {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: read_string(data, &["uname", "username", "user_name"]),
        msg_type: read_i64(data, &["msg_type", "msgType"]) as i32,
    }
}

fn parse_like(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::Like {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: read_string(data, &["uname", "username", "user_name"]),
        text: read_string(data, &["like_text", "likeText", "msg", "message"]),
    }
}

fn parse_entry_effect(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::EntryEffect {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: read_string(data, &["uname", "username", "user_name"]),
        text: read_string(data, &["copy_writing", "copy_writing_v2", "msg", "message"]),
    }
}

fn parse_stats(value: &Value, label: &str) -> LiveCommand {
    let data = value.get("data");
    let value = read_i64(
        data,
        &["num", "count", "online", "watched_num", "online_rank_count"],
    );
    let text = read_string(data, &["text", "message", "msg", "copy_writing"]);
    LiveCommand::Stats {
        label: label.to_owned(),
        value,
        text: if text.is_empty() {
            label.to_owned()
        } else {
            text
        },
    }
}

fn parse_system(value: &Value, label: &str) -> LiveCommand {
    let data = value.get("data");
    let text = read_string(
        data,
        &["text", "message", "msg", "notice", "title", "copy_writing"],
    );
    LiveCommand::System {
        text: if text.is_empty() {
            label.to_owned()
        } else {
            text
        },
    }
}

fn parse_link_mic(value: &Value, cmd: &str) -> LiveCommand {
    let data = value.get("data");
    let text = read_string(data, &["text", "message", "msg", "copy_writing", "status"]);
    LiveCommand::LinkMicPk {
        text: if text.is_empty() {
            format!("连麦 / PK：{cmd}")
        } else {
            text
        },
    }
}

fn parse_watched(value: &Value) -> LiveCommand {
    let count = read_i64(value.get("data"), &["num", "count"]);
    LiveCommand::WatchedChange { count }
}

fn parse_live_start(value: &Value) -> LiveCommand {
    let room_id = read_i64(Some(value), &["roomid", "room_id"]);
    let live_time = read_i64(Some(value), &["live_time"]);
    LiveCommand::LiveStart { room_id, live_time }
}

fn parse_live_end(value: &Value) -> LiveCommand {
    let room_id = value
        .get("roomid")
        .map(|v| v.to_string())
        .unwrap_or_default();
    // roomid 可能是数字或字符串，统一去引号
    let room_id = room_id.trim_matches('"').to_string();
    let round = value.get("round").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    LiveCommand::LiveEnd { room_id, round }
}

fn parse_guard_buy(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::GuardBuy {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: read_string(data, &["username", "uname", "user_name"]),
        guard_level: read_i64(data, &["guard_level", "guardLevel"]) as i32,
        price: read_i64(data, &["price"]) as i32,
        num: read_i64(data, &["num", "quantity"]).max(1) as i32,
        order_id: read_string(data, &["order_id", "orderId"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_danmaku_msg() {
        let json = serde_json::json!({
            "cmd": "DANMU_MSG",
            "info": [
                [0, 1, 25, 16777215, 0, 0, 0, 0, 0, {"ts": 1723979200}, 0, 0, 0, 0, 0,
                    {"user": {"base": {"name": "测试用户"}, "uid": 12345}}
                ],
                "这是一条测试弹幕",
                [12345, "测试用户"]
            ]
        });
        match LiveCommand::from_json(&json) {
            LiveCommand::Danmaku {
                uid,
                uname,
                text,
                timestamp,
                ..
            } => {
                assert_eq!(uid, 12345);
                assert_eq!(uname, "测试用户");
                assert_eq!(text, "这是一条测试弹幕");
                assert_eq!(timestamp, 1723979200);
            }
            other => panic!("期望 Danmaku，得到: {other:?}"),
        }
    }

    #[test]
    fn parse_danmaku_cmd_with_suffix() {
        let json = serde_json::json!({
            "cmd": "DANMU_MSG:4:0:2:2:2:0",
            "info": [[], "带后缀的弹幕", [99, "用户B"]]
        });
        match LiveCommand::from_json(&json) {
            LiveCommand::Danmaku { text, uname, .. } => {
                assert_eq!(text, "带后缀的弹幕");
                assert_eq!(uname, "用户B");
            }
            other => panic!("期望 Danmaku，得到: {other:?}"),
        }
    }

    #[test]
    fn parse_gift_msg() {
        let json = serde_json::json!({
            "cmd": "SEND_GIFT",
            "data": {
                "uid": 510149209,
                "uname": "送礼用户",
                "giftName": "小花花",
                "num": 5,
                "price": 100,
                "total_coin": 500,
                "coin_type": "gold",
                "giftId": 9
            }
        });
        match LiveCommand::from_json(&json) {
            LiveCommand::Gift {
                uid,
                gift_name,
                num,
                total_coin,
                coin_type,
                ..
            } => {
                assert_eq!(uid, 510149209);
                assert_eq!(gift_name, "小花花");
                assert_eq!(num, 5);
                assert_eq!(total_coin, 500);
                assert_eq!(coin_type, "gold");
            }
            other => panic!("期望 Gift，得到: {other:?}"),
        }
    }

    #[test]
    fn parse_super_chat() {
        let json = serde_json::json!({
            "cmd": "SUPER_CHAT_MESSAGE",
            "data": {
                "uid": 294094150,
                "price": 30,
                "message": "SC内容",
                "time": 60,
                "user_info": {"uname": "SC用户"}
            }
        });
        match LiveCommand::from_json(&json) {
            LiveCommand::SuperChat {
                uid,
                message,
                price,
                duration,
                ..
            } => {
                assert_eq!(uid, 294094150);
                assert_eq!(message, "SC内容");
                assert_eq!(price, 30);
                assert_eq!(duration, 60);
            }
            other => panic!("期望 SuperChat，得到: {other:?}"),
        }
    }

    #[test]
    fn parse_live_start_end() {
        let start = serde_json::json!({"cmd": "LIVE", "roomid": 23614753, "live_time": 1651036923});
        match LiveCommand::from_json(&start) {
            LiveCommand::LiveStart { room_id, live_time } => {
                assert_eq!(room_id, 23614753);
                assert_eq!(live_time, 1651036923);
            }
            other => panic!("期望 LiveStart，得到: {other:?}"),
        }

        let end = serde_json::json!({"cmd": "PREPARING", "roomid": "1017", "round": 0});
        match LiveCommand::from_json(&end) {
            LiveCommand::LiveEnd { room_id, round } => {
                assert_eq!(room_id, "1017");
                assert_eq!(round, 0);
            }
            other => panic!("期望 LiveEnd，得到: {other:?}"),
        }
    }

    #[test]
    fn parse_playurl_reload() {
        let json = serde_json::json!({"cmd": "PLAYURL_RELOAD"});
        assert!(matches!(
            LiveCommand::from_json(&json),
            LiveCommand::PlayurlReload
        ));
    }

    #[test]
    fn parse_additional_user_and_system_commands() {
        let like = serde_json::json!({
            "cmd": "LIKE_INFO_V3_CLICK",
            "data": {"uid": 7, "uname": "点赞用户", "like_text": "点了个赞"}
        });
        assert!(matches!(
            LiveCommand::from_json(&like),
            LiveCommand::Like { uid: 7, text, .. } if text == "点了个赞"
        ));

        let entry = serde_json::json!({
            "cmd": "ENTRY_EFFECT",
            "data": {"uid": 8, "uname": "进场用户", "copy_writing": "欢迎 <%进场用户%>"}
        });
        assert!(matches!(
            LiveCommand::from_json(&entry),
            LiveCommand::EntryEffect { uid: 8, text, .. } if text.contains("进场用户")
        ));

        let stats = serde_json::json!({
            "cmd": "ONLINE_RANK_COUNT",
            "data": {"count": 1234}
        });
        assert!(matches!(
            LiveCommand::from_json(&stats),
            LiveCommand::Stats { value: 1234, .. }
        ));

        let system = serde_json::json!({"cmd": "ROOM_SILENT_ON"});
        assert!(matches!(
            LiveCommand::from_json(&system),
            LiveCommand::System { text } if text == "全员禁言开启"
        ));

        let pk = serde_json::json!({"cmd": "PK_BATTLE_START_NEW"});
        assert!(matches!(
            LiveCommand::from_json(&pk),
            LiveCommand::LinkMicPk { .. }
        ));
    }

    #[test]
    fn unknown_cmd_becomes_other() {
        let json = serde_json::json!({"cmd": "SOME_NEW_CMD"});
        match LiveCommand::from_json(&json) {
            LiveCommand::Other { cmd } => assert_eq!(cmd, "SOME_NEW_CMD"),
            other => panic!("期望 Other，得到: {other:?}"),
        }
    }
}
