//! B 站直播弹幕 WebSocket 协议：数据包打包 / 解包。
//!
//! 所有数据包格式：16 字节固定头部 + 正文。
//!
//! 头部结构（大端序）：
//! - offset 0, 4 bytes, uint32: 封包总大小（头部 + 正文）
//! - offset 4, 2 bytes, uint16: 头部大小（固定 16）
//! - offset 6, 2 bytes, uint16: 协议版本（proto）
//! - offset 8, 4 bytes, uint32: 操作码（operation）
//! - offset 12, 4 bytes, uint32: sequence（递增序号）
//!
//! 协议版本（proto）：
//! - 0: 普通包，正文不压缩
//! - 1: 心跳及认证包，正文不压缩
//! - 2: 普通包，正文 zlib 压缩
//! - 3: 普通包，正文 brotli 压缩（可能包含多个子包）
//!
//! 操作码（operation）：
//! - 2: 心跳包（上行）
//! - 3: 心跳包回复 / 人气值（下行）
//! - 5: 普通包 / 命令（下行）
//! - 7: 认证包（上行）
//! - 8: 认证包回复（下行）

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// 头部固定 16 字节。
pub const HEADER_SIZE: usize = 16;

const MAX_WIRE_PACKET_SIZE: usize = 16 * 1024 * 1024;
const MAX_DECOMPRESSED_SIZE: usize = 8 * 1024 * 1024;
const MAX_SUBCOMMANDS: usize = 4096;
const MAX_NESTING_DEPTH: usize = 4;

/// 操作码常量。
pub mod op {
    pub const HEARTBEAT: u32 = 2;
    pub const HEARTBEAT_REPLY: u32 = 3;
    pub const COMMAND: u32 = 5;
    pub const AUTH: u32 = 7;
    pub const AUTH_REPLY: u32 = 8;
}

/// 协议版本常量。
pub mod proto {
    pub const PLAIN: u16 = 0;
    pub const HEARTBEAT_AUTH: u16 = 1;
    pub const ZLIB: u16 = 2;
    pub const BROTLI: u16 = 3;
}

/// 打包一条 JSON 消息为可发送的二进制数据包。
///
/// 用于构造认证包（op=7）和心跳包（op=2）。
/// 头部 proto 固定为 1（心跳/认证包不压缩）。
pub fn make_packet(data: &Value, operation: u32) -> Vec<u8> {
    let body = serde_json::to_vec(data).expect("JSON 序列化不应失败");
    let total_len = (HEADER_SIZE + body.len()) as u32;

    let mut packet = Vec::with_capacity(total_len as usize);
    // 大端序: total_len(4) + header_len(2) + proto(2) + operation(4) + sequence(4)
    packet.extend_from_slice(&total_len.to_be_bytes());
    packet.extend_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
    packet.extend_from_slice(&proto::HEARTBEAT_AUTH.to_be_bytes());
    packet.extend_from_slice(&operation.to_be_bytes());
    packet.extend_from_slice(&1u32.to_be_bytes()); // sequence = 1
    packet.extend_from_slice(&body);
    packet
}

/// 构造认证包（op=7）。
///
/// 认证必须使用登录 Cookie 中的账号 UID 与 `getDanmuInfo` 返回的 token。
pub fn make_auth_packet(room_id: i64, uid: i64, token: &str) -> Vec<u8> {
    let data = serde_json::json!({
        "uid": uid,
        "roomid": room_id,
        "protover": 3,
        "platform": "web",
        "type": 2,
        "key": token
    });
    make_packet(&data, op::AUTH)
}

/// 构造心跳包（op=2），正文为空对象。
pub fn make_heartbeat_packet() -> Vec<u8> {
    make_packet(&serde_json::json!({}), op::HEARTBEAT)
}

/// 一帧中每个外层协议包的解析结果。保留 operation 供认证流程严格校验。
#[derive(Debug, Clone)]
pub struct ParsedFrame {
    pub operation: u32,
    pub values: Vec<Value>,
}

/// 从 WebSocket 二进制消息中解析出所有命令 JSON。
///
/// 一条 WebSocket 消息可能包含：
/// - 单个包（proto=0/1，直接解析）
/// - zlib 压缩的包（proto=2，先解压再按子包拆分）
/// - brotli 压缩的包（proto=3，先解压再按子包拆分，可能含多个子命令）
pub fn parse_commands(raw: &[u8]) -> Result<Vec<Value>> {
    if raw.len() < HEADER_SIZE {
        return Err(anyhow!("弹幕数据包过短: {} 字节", raw.len()));
    }
    if raw.len() > MAX_WIRE_PACKET_SIZE {
        return Err(anyhow!(
            "弹幕数据包超过 {} MiB 限制",
            MAX_WIRE_PACKET_SIZE / 1024 / 1024
        ));
    }

    Ok(parse_frames(raw)?
        .into_iter()
        .flat_map(|frame| frame.values)
        .collect())
}

/// 解析 WebSocket frame 中所有外层协议包，禁止静默丢弃尾包。
pub fn parse_frames(raw: &[u8]) -> Result<Vec<ParsedFrame>> {
    if raw.len() < HEADER_SIZE {
        return Err(anyhow!("弹幕数据包过短: {} 字节", raw.len()));
    }
    if raw.len() > MAX_WIRE_PACKET_SIZE {
        return Err(anyhow!(
            "弹幕数据包超过 {} MiB 限制",
            MAX_WIRE_PACKET_SIZE / 1024 / 1024
        ));
    }

    let mut offset = 0;
    let mut frames = Vec::new();
    while offset < raw.len() {
        let remaining = raw.len() - offset;
        if remaining < HEADER_SIZE {
            return Err(anyhow!("弹幕外层包头不完整: remaining={remaining}"));
        }
        let packet = &raw[offset..];
        let total_len = u32::from_be_bytes([packet[0], packet[1], packet[2], packet[3]]) as usize;
        let header_len = u16::from_be_bytes([packet[4], packet[5]]) as usize;
        let proto_ver = u16::from_be_bytes([packet[6], packet[7]]);
        let operation = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
        validate_packet_bounds(remaining, total_len, header_len)?;
        let body = &packet[header_len..total_len];
        let values = match operation {
            op::HEARTBEAT_REPLY => Vec::new(),
            op::AUTH_REPLY => vec![serde_json::from_slice(body).context("解析认证回复 JSON 失败")?],
            op::COMMAND if matches!(proto_ver, proto::PLAIN | proto::HEARTBEAT_AUTH) => {
                vec![serde_json::from_slice(body).context("解析明文弹幕 JSON 失败")?]
            }
            op::COMMAND => extract_sub_commands(&decompress_body(body, proto_ver)?, 0)?,
            _ => Vec::new(),
        };
        frames.push(ParsedFrame { operation, values });
        offset += total_len;
    }
    Ok(frames)
}

/// 认证被服务器拒绝（`code != 0`）。
///
/// 结构化错误：携带 B 站业务错误码，供重连策略优先按错误码精确分类，
/// 中文错误文案匹配仅作兜底（文案变更不应影响分类行为）。
#[derive(Debug, thiserror::Error)]
#[error("弹幕认证失败: code={code}")]
pub struct AuthRejected {
    /// B 站业务错误码（-101 登录失效 / -352 风控 / -412 请求被拒 等）。
    pub code: i64,
}

/// 严格验证认证首包，只有 `op=8` 和 `code=0` 才可视为连接成功。
pub fn validate_auth_reply(raw: &[u8]) -> Result<()> {
    let frames = parse_frames(raw)?;
    let Some(frame) = frames.first() else {
        return Err(anyhow!("认证回复为空"));
    };
    if frame.operation != op::AUTH_REPLY {
        return Err(anyhow!(
            "认证回复操作码非法: expected=8, actual={}",
            frame.operation
        ));
    }
    let Some(value) = frame.values.first() else {
        return Err(anyhow!("认证回复缺少 JSON 正文"));
    };
    match value.get("code").and_then(Value::as_i64) {
        Some(0) => Ok(()),
        Some(code) => Err(anyhow::Error::new(AuthRejected { code })),
        None => Err(anyhow!("认证回复缺少 code")),
    }
}

fn validate_packet_bounds(raw_len: usize, total_len: usize, header_len: usize) -> Result<()> {
    if header_len < HEADER_SIZE {
        return Err(anyhow!("弹幕包头长度非法: {header_len}"));
    }
    if total_len < header_len {
        return Err(anyhow!(
            "弹幕包总长度小于包头长度: total={total_len}, header={header_len}"
        ));
    }
    if total_len > raw_len {
        return Err(anyhow!(
            "弹幕包声明长度超过实际数据: total={total_len}, actual={raw_len}"
        ));
    }
    if total_len > MAX_WIRE_PACKET_SIZE {
        return Err(anyhow!("弹幕包声明长度超过安全上限"));
    }
    Ok(())
}

/// 按协议版本解压正文。
fn decompress_body(body: &[u8], proto_ver: u16) -> Result<Vec<u8>> {
    match proto_ver {
        proto::PLAIN | proto::HEARTBEAT_AUTH => Ok(body.to_vec()),
        proto::ZLIB => {
            let mut decoder = flate2::read::ZlibDecoder::new(body);
            read_limited(&mut decoder, "zlib")
        }
        proto::BROTLI => {
            let mut reader = brotli::Decompressor::new(body, 4096);
            read_limited(&mut reader, "brotli")
        }
        other => Err(anyhow!("未知弹幕协议版本: {other}")),
    }
}

fn read_limited(reader: &mut impl std::io::Read, label: &str) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut output = Vec::new();
    reader
        .take((MAX_DECOMPRESSED_SIZE + 1) as u64)
        .read_to_end(&mut output)
        .with_context(|| format!("{label} 解压弹幕数据失败"))?;
    if output.len() > MAX_DECOMPRESSED_SIZE {
        return Err(anyhow!(
            "{label} 解压结果超过 {} MiB 限制",
            MAX_DECOMPRESSED_SIZE / 1024 / 1024
        ));
    }
    Ok(output)
}

/// 从解压后的 body 中提取所有子命令。
///
/// 解压后的数据可能包含多个子包，每个子包也有自己的 16 字节头部。
fn extract_sub_commands(data: &[u8], depth: usize) -> Result<Vec<Value>> {
    if depth > MAX_NESTING_DEPTH {
        return Err(anyhow!("弹幕压缩嵌套层级超过安全上限"));
    }
    let mut commands = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        let remaining = data.len() - offset;
        if remaining < HEADER_SIZE {
            return Err(anyhow!("弹幕子包头不完整: remaining={remaining}"));
        }
        let sub_len = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        let sub_header_len = u16::from_be_bytes([data[offset + 4], data[offset + 5]]) as usize;
        let sub_proto = u16::from_be_bytes([data[offset + 6], data[offset + 7]]);
        let sub_operation = u32::from_be_bytes([
            data[offset + 8],
            data[offset + 9],
            data[offset + 10],
            data[offset + 11],
        ]);

        validate_packet_bounds(remaining, sub_len, sub_header_len)?;
        let end = offset
            .checked_add(sub_len)
            .ok_or_else(|| anyhow!("弹幕子包长度溢出"))?;
        let sub_body = &data[offset + sub_header_len..end];

        if sub_operation == op::HEARTBEAT_REPLY {
            offset = end;
            continue;
        }
        if sub_operation == op::COMMAND && matches!(sub_proto, proto::ZLIB | proto::BROTLI) {
            let nested = decompress_body(sub_body, sub_proto)?;
            commands.extend(extract_sub_commands(&nested, depth + 1)?);
        } else if sub_operation == op::COMMAND || sub_operation == op::AUTH_REPLY {
            match serde_json::from_slice::<Value>(sub_body) {
                Ok(value) => commands.push(value),
                Err(error) => tracing::debug!(%error, "跳过非 JSON 弹幕子命令"),
            }
        }

        if commands.len() > MAX_SUBCOMMANDS {
            return Err(anyhow!("弹幕子命令数量超过安全上限"));
        }
        offset = end;
    }

    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn make_auth_packet_has_correct_header() {
        let packet = make_auth_packet(32352630, 114514, "test_token");
        assert!(packet.len() > HEADER_SIZE);

        let total_len = u32::from_be_bytes([packet[0], packet[1], packet[2], packet[3]]) as usize;
        assert_eq!(total_len, packet.len());

        let header_len = u16::from_be_bytes([packet[4], packet[5]]);
        assert_eq!(header_len, HEADER_SIZE as u16);

        let proto_ver = u16::from_be_bytes([packet[6], packet[7]]);
        assert_eq!(proto_ver, proto::HEARTBEAT_AUTH);

        let operation = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
        assert_eq!(operation, op::AUTH);

        // 验证 body 是合法 JSON 且包含关键字段
        let body = &packet[HEADER_SIZE..];
        let json: Value = serde_json::from_slice(body).expect("auth body is JSON");
        assert_eq!(json["roomid"], 32352630);
        assert_eq!(json["key"], "test_token");
        assert_eq!(json["uid"], 114514);
        assert_eq!(json["protover"], 3);
    }

    #[test]
    fn make_heartbeat_packet_is_op2() {
        let packet = make_heartbeat_packet();
        let operation = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
        assert_eq!(operation, op::HEARTBEAT);
    }

    #[test]
    fn auth_reply_requires_op8_and_zero_code() {
        let ok = make_packet(&serde_json::json!({"code": 0}), op::AUTH_REPLY);
        assert!(validate_auth_reply(&ok).is_ok());
        let rejected = make_packet(&serde_json::json!({"code": -101}), op::AUTH_REPLY);
        assert!(validate_auth_reply(&rejected).is_err());
        let wrong_operation = make_packet(&serde_json::json!({"code": 0}), op::COMMAND);
        assert!(validate_auth_reply(&wrong_operation).is_err());
    }

    #[test]
    fn parse_frames_preserves_all_top_level_packets() {
        let one = make_packet(&serde_json::json!({"cmd": "ONE"}), op::COMMAND);
        let two = make_packet(&serde_json::json!({"cmd": "TWO"}), op::COMMAND);
        let mut frame = one;
        frame.extend(two);
        let frames = parse_frames(&frame).expect("multiple outer packets");
        assert_eq!(frames.len(), 2);
        assert_eq!(parse_commands(&frame).unwrap().len(), 2);
    }

    #[test]
    fn parse_commands_handles_plain_json() {
        // 构造一个 proto=0, op=5 的明文命令包
        let cmd = serde_json::json!({"cmd": "DANMU_MSG", "info": [[], "测试弹幕"]});
        let body = serde_json::to_vec(&cmd).unwrap();
        let total_len = (HEADER_SIZE + body.len()) as u32;

        let mut packet = Vec::new();
        packet.extend_from_slice(&total_len.to_be_bytes());
        packet.extend_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
        packet.extend_from_slice(&proto::PLAIN.to_be_bytes());
        packet.extend_from_slice(&op::COMMAND.to_be_bytes());
        packet.extend_from_slice(&1u32.to_be_bytes());
        packet.extend_from_slice(&body);

        let commands = parse_commands(&packet).expect("parse plain command");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["cmd"], "DANMU_MSG");
    }

    #[test]
    fn parse_commands_ignores_heartbeat_reply() {
        // 构造 op=3 心跳回复（前 4 字节是人气值）
        let mut packet = Vec::new();
        let body = 12345u32.to_be_bytes();
        let total_len = (HEADER_SIZE + body.len()) as u32;
        packet.extend_from_slice(&total_len.to_be_bytes());
        packet.extend_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
        packet.extend_from_slice(&proto::PLAIN.to_be_bytes());
        packet.extend_from_slice(&op::HEARTBEAT_REPLY.to_be_bytes());
        packet.extend_from_slice(&1u32.to_be_bytes());
        packet.extend_from_slice(&body);

        let commands = parse_commands(&packet).expect("parse heartbeat reply");
        assert!(commands.is_empty());
    }

    #[test]
    fn parse_commands_too_short_returns_error() {
        let result = parse_commands(&[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_commands_rejects_invalid_header_ranges_without_panicking() {
        let mut packet = vec![0u8; HEADER_SIZE];
        packet[..4].copy_from_slice(&(HEADER_SIZE as u32).to_be_bytes());
        packet[4..6].copy_from_slice(&((HEADER_SIZE + 1) as u16).to_be_bytes());
        packet[8..12].copy_from_slice(&op::COMMAND.to_be_bytes());
        assert!(parse_commands(&packet).is_err());

        packet[4..6].copy_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
        packet[..4].copy_from_slice(&((HEADER_SIZE + 1) as u32).to_be_bytes());
        assert!(parse_commands(&packet).is_err());
    }

    #[test]
    fn parse_commands_handles_zlib_sub_packet() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        let command = serde_json::json!({"cmd": "DANMU_MSG", "info": [[], "压缩"]});
        let body = serde_json::to_vec(&command).unwrap();
        let sub_len = (HEADER_SIZE + body.len()) as u32;
        let mut sub_packet = Vec::new();
        sub_packet.extend_from_slice(&sub_len.to_be_bytes());
        sub_packet.extend_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
        sub_packet.extend_from_slice(&proto::PLAIN.to_be_bytes());
        sub_packet.extend_from_slice(&op::COMMAND.to_be_bytes());
        sub_packet.extend_from_slice(&1u32.to_be_bytes());
        sub_packet.extend_from_slice(&body);

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&sub_packet).unwrap();
        let compressed = encoder.finish().unwrap();
        let total_len = (HEADER_SIZE + compressed.len()) as u32;
        let mut packet = Vec::new();
        packet.extend_from_slice(&total_len.to_be_bytes());
        packet.extend_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
        packet.extend_from_slice(&proto::ZLIB.to_be_bytes());
        packet.extend_from_slice(&op::COMMAND.to_be_bytes());
        packet.extend_from_slice(&1u32.to_be_bytes());
        packet.extend_from_slice(&compressed);

        let commands = parse_commands(&packet).expect("parse zlib command");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["cmd"], "DANMU_MSG");
    }

    #[test]
    fn parse_commands_rejects_oversized_decompression() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        let oversized = vec![b'x'; MAX_DECOMPRESSED_SIZE + 1];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&oversized).unwrap();
        let compressed = encoder.finish().unwrap();
        let total_len = (HEADER_SIZE + compressed.len()) as u32;
        let mut packet = Vec::new();
        packet.extend_from_slice(&total_len.to_be_bytes());
        packet.extend_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
        packet.extend_from_slice(&proto::ZLIB.to_be_bytes());
        packet.extend_from_slice(&op::COMMAND.to_be_bytes());
        packet.extend_from_slice(&1u32.to_be_bytes());
        packet.extend_from_slice(&compressed);

        assert!(parse_commands(&packet).is_err());
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_wire_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..=4096)) {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = parse_commands(&bytes);
            }));
            prop_assert!(outcome.is_ok());
        }
    }
}
