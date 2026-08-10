//! 弹幕命令解析：将 B 站 WebSocket 推送的 JSON 命令转为类型化枚举。
//!
//! 覆盖主要命令类型：弹幕、礼物、进场、SC、看过人数、开播/下播、流地址刷新。
//! 未识别的命令统一归入 `Other`，不丢弃。

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
        let cmd_base = cmd.split(':').next().unwrap_or(&cmd);

        match cmd_base {
            "DANMU_MSG" => parse_danmaku(value),
            "SEND_GIFT" => parse_gift(value),
            "SUPER_CHAT_MESSAGE" => parse_super_chat(value),
            "INTERACT_WORD" => parse_interact(value),
            "WATCHED_CHANGE" => parse_watched(value),
            "LIVE" => parse_live_start(value),
            "PREPARING" => parse_live_end(value),
            "PLAYURL_RELOAD" | "PLAYURL_RELOAD_V2" => LiveCommand::PlayurlReload,
            "GUARD_BUY" => parse_guard_buy(value),
            _ => LiveCommand::Other { cmd },
        }
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

fn parse_gift(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::Gift {
        uid: data
            .and_then(|d| d.get("uid"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        uname: data
            .and_then(|d| d.get("uname"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        gift_name: data
            .and_then(|d| d.get("giftName"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        num: data
            .and_then(|d| d.get("num"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        price: data
            .and_then(|d| d.get("price"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        total_coin: data
            .and_then(|d| d.get("total_coin"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        coin_type: data
            .and_then(|d| d.get("coin_type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        gift_id: data
            .and_then(|d| d.get("giftId"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
    }
}

fn parse_super_chat(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::SuperChat {
        uid: data
            .and_then(|d| d.get("uid"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        uname: data
            .and_then(|d| d.get("user_info"))
            .and_then(|u| u.get("uname"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        message: data
            .and_then(|d| d.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        price: data
            .and_then(|d| d.get("price"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        duration: data
            .and_then(|d| d.get("time"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        id: data
            .and_then(|d| d.get("id_str"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    }
}

fn parse_interact(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::Interact {
        uid: data
            .and_then(|d| d.get("uid"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        uname: data
            .and_then(|d| d.get("uname"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        msg_type: data
            .and_then(|d| d.get("msg_type"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
    }
}

fn parse_watched(value: &Value) -> LiveCommand {
    let count = value
        .get("data")
        .and_then(|d| d.get("num"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    LiveCommand::WatchedChange { count }
}

fn parse_live_start(value: &Value) -> LiveCommand {
    let room_id = value.get("roomid").and_then(|v| v.as_i64()).unwrap_or(0);
    let live_time = value.get("live_time").and_then(|v| v.as_i64()).unwrap_or(0);
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
        uid: data
            .and_then(|d| d.get("uid"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        uname: data
            .and_then(|d| d.get("username"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        guard_level: data
            .and_then(|d| d.get("guard_level"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        price: data
            .and_then(|d| d.get("price"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        num: data
            .and_then(|d| d.get("num"))
            .and_then(Value::as_i64)
            .unwrap_or(1) as i32,
        order_id: data
            .and_then(|d| d.get("order_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
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
    fn unknown_cmd_becomes_other() {
        let json = serde_json::json!({"cmd": "SOME_NEW_CMD"});
        match LiveCommand::from_json(&json) {
            LiveCommand::Other { cmd } => assert_eq!(cmd, "SOME_NEW_CMD"),
            other => panic!("期望 Other，得到: {other:?}"),
        }
    }
}
