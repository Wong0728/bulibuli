//! B 站 API 强类型响应模型。
//!
//! 响应应定义 `#[derive(Deserialize)]` 结构；只声明用到的字段，并用
//! `#[serde(default)]` 容忍可选字段缺失。业务代码不应逐字段解析裸
//! `serde_json::Value`。
//!
//! 子模块划分：
//! - `auth`：nav 登录态 / WBI keys / 扫码登录
//! - `playurl`：playurl 流解析（DASH/durl）与对外流结构
//! - `user`：投稿列表 / 合集系列 / 用户搜索 / 用户信息
//! - `video`：视频详情（wbi/view）
//! - `pgc`：番剧（PGC）季/集信息与 playurl 模型
//! - `cheese`：课程（Cheese / PUGV）季/集信息与 playurl 模型
//! - `live`：直播间房间信息、流地址、弹幕连接配置

pub mod auth;
pub mod cheese;
pub mod live;
pub mod pgc;
pub mod playurl;
pub mod user;
pub mod video;

// 字幕模型在 video 子模块中定义，这里 re-export 便于外部按 `models::SubtitleInfo` 引用。
pub use video::SubtitleInfo;

use serde::Deserialize;

/// B 站统一响应信封 `{code, message, data}`。
/// `data` 保持惰性，由统一解析入口（`BiliApi::parse_data`）按需反序列化为具体类型。
/// 番剧（PGC）接口使用 `result` 字段而非 `data`，这里一并声明以兼容两类响应。
#[derive(Debug, Deserialize)]
pub struct BiliEnvelope {
    pub code: Option<i64>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub data: serde_json::Value,
    /// 番剧接口的响应主体（与 `data` 互斥：PGC 用 result，普通接口用 data）。
    #[serde(default)]
    pub result: serde_json::Value,
}

/// 风控相关响应字段（命中 -352/-403 时 B 站可能在 data 中携带 v_voucher，
/// 供 gaia-vgate 申请验证码；本项目不做自动破解，仅透传给前端横幅便于排查）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RiskControlData {
    pub v_voucher: Option<String>,
}

/// 宽容整数反序列化：接受数字或字符串数字；
/// B 站隐藏数据时（如播放量返回 `"--"`）按 0 处理。
pub(crate) fn lenient_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct LenientVisitor;

    impl serde::de::Visitor<'_> for LenientVisitor {
        type Value = i64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("整数或数字字符串")
        }

        fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<i64, E> {
            Ok(value)
        }

        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<i64, E> {
            Ok(i64::try_from(value).unwrap_or(i64::MAX))
        }

        fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<i64, E> {
            Ok(value as i64)
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<i64, E> {
            Ok(value.trim().parse().unwrap_or(0))
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<i64, E> {
            Ok(0)
        }
    }

    deserializer.deserialize_any(LenientVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Wrapper {
        #[serde(default, deserialize_with = "lenient_i64")]
        value: i64,
    }

    #[test]
    fn lenient_i64_accepts_numbers_and_strings() {
        let n: Wrapper = serde_json::from_str(r#"{"value": 42}"#).expect("number");
        assert_eq!(n.value, 42);
        let s: Wrapper = serde_json::from_str(r#"{"value": "42"}"#).expect("string number");
        assert_eq!(s.value, 42);
        let hidden: Wrapper = serde_json::from_str(r#"{"value": "--"}"#).expect("hidden marker");
        assert_eq!(hidden.value, 0);
        let missing: Wrapper = serde_json::from_str(r#"{}"#).expect("missing field");
        assert_eq!(missing.value, 0);
    }

    #[test]
    fn envelope_tolerates_missing_fields() {
        let envelope: BiliEnvelope =
            serde_json::from_str(r#"{"code": 0}"#).expect("minimal envelope");
        assert_eq!(envelope.code, Some(0));
        assert!(envelope.message.is_none());
        assert!(envelope.data.is_null());
        let missing_code: BiliEnvelope = serde_json::from_str(r#"{}"#).expect("empty envelope");
        assert!(missing_code.code.is_none());
    }

    #[test]
    fn risk_data_extracts_v_voucher() {
        let risk: RiskControlData =
            serde_json::from_str(r#"{"v_voucher": "voucher_abc"}"#).expect("risk data");
        assert_eq!(risk.v_voucher.as_deref(), Some("voucher_abc"));
        let empty: RiskControlData = serde_json::from_str(r#"{}"#).expect("empty risk data");
        assert!(empty.v_voucher.is_none());
    }
}
