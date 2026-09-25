//! 各命令 JSON 载荷的字段解析：从 B 站推送的原始 JSON 中提取
//! 弹幕 / 礼物 / SC / 进场 / 点赞 / 统计 / 系统等结构化字段。
//! 命令类型定义、识别与分发见 `commands`。

use super::commands::LiveCommand;
use serde_json::Value;

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
pub(super) fn parse_danmaku(value: &Value) -> LiveCommand {
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

pub(super) fn parse_gift(value: &Value) -> LiveCommand {
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

pub(super) fn parse_super_chat(value: &Value) -> LiveCommand {
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

pub(super) fn parse_interact(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::Interact {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: read_string(data, &["uname", "username", "user_name"]),
        msg_type: read_i64(data, &["msg_type", "msgType"]) as i32,
    }
}

pub(super) fn parse_like(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::Like {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: read_string(data, &["uname", "username", "user_name"]),
        text: read_string(data, &["like_text", "likeText", "msg", "message"]),
    }
}

pub(super) fn parse_entry_effect(value: &Value) -> LiveCommand {
    let data = value.get("data");
    LiveCommand::EntryEffect {
        uid: read_i64(data, &["uid", "user_id"]),
        uname: read_string(data, &["uname", "username", "user_name"]),
        text: read_string(data, &["copy_writing", "copy_writing_v2", "msg", "message"]),
    }
}

pub(super) fn parse_stats(value: &Value, label: &str) -> LiveCommand {
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

pub(super) fn parse_system(value: &Value, label: &str) -> LiveCommand {
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

pub(super) fn parse_link_mic(value: &Value, cmd: &str) -> LiveCommand {
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

pub(super) fn parse_watched(value: &Value) -> LiveCommand {
    let count = read_i64(value.get("data"), &["num", "count"]);
    LiveCommand::WatchedChange { count }
}

pub(super) fn parse_live_start(value: &Value) -> LiveCommand {
    let room_id = read_i64(Some(value), &["roomid", "room_id"]);
    let live_time = read_i64(Some(value), &["live_time"]);
    LiveCommand::LiveStart { room_id, live_time }
}

pub(super) fn parse_live_end(value: &Value) -> LiveCommand {
    let room_id = value
        .get("roomid")
        .map(|v| v.to_string())
        .unwrap_or_default();
    // roomid 可能是数字或字符串，统一去引号
    let room_id = room_id.trim_matches('"').to_string();
    let round = value.get("round").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    LiveCommand::LiveEnd { room_id, round }
}

pub(super) fn parse_guard_buy(value: &Value) -> LiveCommand {
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
