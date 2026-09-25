//! 弹幕命令解析：将 B 站 WebSocket 推送的 JSON 命令转为类型化枚举。
//!
//! 覆盖主要命令类型：弹幕、礼物、进场、点赞、SC、看过人数、房间状态、PK/连麦、流地址刷新。
//! 未识别的命令统一归入 `Other`，不丢弃；展示层只会把确实没有分类规则的命令显示为未知。
//!
//! 命令识别与分发在本文件；各命令 JSON 载荷的字段提取见 `parsers`。

use super::parsers::{
    parse_danmaku, parse_entry_effect, parse_gift, parse_guard_buy, parse_interact, parse_like,
    parse_link_mic, parse_live_end, parse_live_start, parse_stats, parse_super_chat, parse_system,
    parse_watched,
};
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
