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
///
/// 测试兼容入口：解码失败按空段返回。生产路径请改用 try_parse_danmaku_bytes。
#[cfg(test)]
pub fn parse_danmaku_bytes(bytes: &[u8]) -> Vec<Value> {
    try_parse_danmaku_bytes(bytes).unwrap_or_default()
}

/// 解码成功返回弹幕列表（空分段为 `Some(vec![])`，即「无弹幕」）；
/// 解码失败返回 None——调用方应计入失败分段，而不是误当成成功空段。
pub fn try_parse_danmaku_bytes(bytes: &[u8]) -> Option<Vec<Value>> {
    match DanmakuSeg::decode(bytes) {
        Ok(seg) => Some(
            seg.elems
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
        ),
        Err(error) => {
            // 解码失败此前静默返回空数组，调用方会把损坏分段误当成
            // 「成功空段」计入成功数，导致问题被完全吞掉。
            // 留 warn 日志（含首字节，便于区分风控 HTML/网关错误页），按未解析处理。
            tracing::warn!(
                len = bytes.len(),
                head = ?bytes.first().map(|b| format!("{b:02x}")),
                error = %error,
                "弹幕 protobuf 分段解码失败"
            );
            None
        }
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

    mod proptest_suite {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// 任意字节流（随机包头/压缩正文/截断包）解析不得 panic，
            /// 失败时返回空数组而不是错误。
            #[test]
            fn parse_danmaku_bytes_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..=2048)) {
                let list = parse_danmaku_bytes(&bytes);
                for item in &list {
                    prop_assert!(item.is_object());
                    prop_assert!(item.get("time").and_then(Value::as_f64).is_some());
                }
            }

            /// 合法编码的消息必须无损往返：条数、文本、时间戳逐条一致。
            #[test]
            fn encode_decode_roundtrip_preserves_elems(
                texts in proptest::collection::vec(".{0,32}", 0..=20),
                progresses in proptest::collection::vec(any::<i64>(), 0..=20),
                modes in proptest::collection::vec(any::<i32>(), 0..=20),
            ) {
                let n = texts.len().min(progresses.len()).min(modes.len());
                let elems = (0..n)
                    .map(|i| DanmakuElem {
                        id: 0,
                        progress: progresses[i],
                        mode: modes[i],
                        fontsize: 25,
                        color: 0xFFFFFF,
                        mid_hash: String::new(),
                        text: texts[i].clone(),
                        ctime: 1_700_000_000,
                        weight: 0,
                        action: String::new(),
                        pool: 0,
                        id_str: String::new(),
                    })
                    .collect();
                let mut buf = Vec::new();
                DanmakuSeg { elems }.encode(&mut buf).expect("encode");
                let list = parse_danmaku_bytes(&buf);
                prop_assert_eq!(list.len(), n);
                for (i, item) in list.iter().enumerate() {
                    prop_assert_eq!(item["text"].as_str().unwrap_or_default(), &texts[i]);
                    prop_assert_eq!(
                        item["time"].as_f64().unwrap_or_default(),
                        progresses[i] as f64 / 1000.0
                    );
                }
            }
        }
    }
}
