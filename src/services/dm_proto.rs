//! B 站弹幕 protobuf 解析（端点 /x/v2/dm/wbi/web/seg.so）。
//!
//! 字段编号参考社区文档与 Bili23-Downloader 实现。
//! 手写 prost 结构体，避免引入 protoc/build.rs 依赖。

use prost::Message;
use serde_json::{json, Value};

/// 弹幕分段响应（顶层消息）。
#[derive(Clone, PartialEq, Message)]
pub struct DanmakuSeg {
    #[prost(message, repeated, tag = "1")]
    pub elems: Vec<DanmakuElem>,
}

/// 单条弹幕。
#[derive(Clone, PartialEq, Message)]
pub struct DanmakuElem {
    /// 弹幕 dmid（数值形式，B 站已推荐使用 id_str）。
    #[prost(int64, tag = "1")]
    pub id: i64,
    /// 视频内出现时间（毫秒）。
    #[prost(int64, tag = "2")]
    pub progress: i64,
    /// 弹幕类型：1 普通 / 4 底部 / 5 顶部 / 7 高级 / 8 代码 / 9 BAS。
    #[prost(int32, tag = "3")]
    pub mode: i32,
    /// 字号。
    #[prost(int32, tag = "4")]
    pub fontsize: i32,
    /// 颜色（RGB）。
    #[prost(uint32, tag = "5")]
    pub color: u32,
    /// 用户 mid 哈希。
    #[prost(string, tag = "6")]
    pub mid_hash: String,
    /// 弹幕文本。
    #[prost(string, tag = "7")]
    pub text: String,
    /// 发送时间（Unix 秒）。
    #[prost(int64, tag = "8")]
    pub ctime: i64,
    /// 权重。
    #[prost(int32, tag = "9")]
    pub weight: i32,
    /// 动作（"subscribe" 等）。
    #[prost(string, tag = "10")]
    pub action: String,
    /// 弹幕池：0 普通 / 1 字幕 / 2 特殊。
    #[prost(int32, tag = "11")]
    pub pool: i32,
    /// dmid 字符串形式。
    #[prost(string, tag = "12")]
    pub id_str: String,
}

/// 解析 protobuf 字节流为弹幕 JSON 数组（与旧 XML 解析输出 schema 对齐）。
///
/// 字段映射：
/// - `time` = `progress / 1000.0`（秒）
/// - `type` = `mode`
/// - `size` = `fontsize`
/// - `color` = `color`
/// - `timestamp` = `ctime`
/// - `pool` = `pool`
/// - `hash` = `mid_hash`
/// - `dmid` = `id_str`（优先）或 `id`
/// - `text` = `text`
pub fn parse_danmaku_bytes(bytes: &[u8]) -> Vec<Value> {
    match DanmakuSeg::decode(bytes) {
        Ok(seg) => seg
            .elems
            .into_iter()
            .map(|e| {
                let dmid = if !e.id_str.is_empty() {
                    e.id_str.clone()
                } else {
                    e.id.to_string()
                };
                json!({
                    "time": (e.progress as f64) / 1000.0,
                    "type": e.mode,
                    "size": e.fontsize,
                    "color": e.color,
                    "timestamp": e.ctime,
                    "pool": e.pool,
                    "hash": e.mid_hash,
                    "dmid": dmid,
                    "text": e.text,
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_bytes() {
        let list = parse_danmaku_bytes(&[]);
        assert!(list.is_empty());
    }

    #[test]
    fn test_parse_single_elem() {
        // 手工构造一个 DanmakuSeg { elems: [DanmakuElem { text: "hi", progress: 1500, mode: 1, fontsize: 25, color: 0xFFFFFF, id_str: "abc", ctime: 1700000000, mid_hash: "h", pool: 0 }] }
        let elem = DanmakuElem {
            id: 0,
            progress: 1500,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            mid_hash: "h".to_string(),
            text: "hi".to_string(),
            ctime: 1700000000,
            weight: 0,
            action: String::new(),
            pool: 0,
            id_str: "abc".to_string(),
        };
        let seg = DanmakuSeg { elems: vec![elem] };
        let mut buf = Vec::new();
        seg.encode(&mut buf).expect("encode");
        let list = parse_danmaku_bytes(&buf);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["text"].as_str().unwrap(), "hi");
        assert_eq!(list[0]["time"].as_f64().unwrap(), 1.5);
        assert_eq!(list[0]["dmid"].as_str().unwrap(), "abc");
        assert_eq!(list[0]["color"].as_i64().unwrap(), 0xFFFFFF);
    }
}
