//! nav 登录态 / WBI keys / 扫码登录响应模型。

use serde::{Deserialize, Serialize};

/// `/x/web-interface/nav` 的 data（只声明用到的字段）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct NavData {
    #[serde(rename = "isLogin")]
    pub is_login: bool,
    pub mid: i64,
    pub uname: String,
    pub face: String,
    pub level_info: LevelInfo,
    #[serde(rename = "vipStatus")]
    pub vip_status: i64,
    #[serde(rename = "vipType")]
    pub vip_type: i64,
    pub vip_label: VipLabel,
    pub vip: VipInfo,
    pub wbi_img: WbiImg,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LevelInfo {
    pub current_level: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VipLabel {
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VipInfo {
    pub label: VipLabel,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WbiImg {
    pub img_url: String,
    pub sub_url: String,
}

/// nav 原始信封（WbiKeysCache 独立请求 nav 时用，与统一入口共用同一结构约定）。
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct NavResponse {
    pub code: i64,
    pub message: String,
    pub data: Option<NavData>,
}

impl Default for NavResponse {
    fn default() -> Self {
        Self {
            code: -1,
            message: String::new(),
            data: None,
        }
    }
}

/// 登录态摘要（对外域模型，序列化字段名与前端契约一致）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct NavStatus {
    pub is_login: bool,
    pub mid: i64,
    pub uname: String,
    pub face: String,
    pub level: i64,
    pub vip_status: i64,
    pub vip_type: i64,
    pub vip_label: String,
}

impl From<NavData> for NavStatus {
    fn from(data: NavData) -> Self {
        // 协议相对地址补全（//i0.hdslb.com/... → https://...）
        let face = if !data.face.is_empty() && !data.face.starts_with("http") {
            format!("https:{}", data.face)
        } else {
            data.face
        };
        let vip_label = if data.vip_label.text.is_empty() {
            data.vip.label.text
        } else {
            data.vip_label.text
        };
        Self {
            is_login: data.is_login,
            mid: data.mid,
            uname: data.uname,
            face,
            level: data.level_info.current_level,
            vip_status: data.vip_status,
            vip_type: data.vip_type,
            vip_label,
        }
    }
}

/// `/x/passport-login/web/qrcode/generate` 的 data。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct QrcodeGenerate {
    pub url: String,
    pub qrcode_key: String,
}

/// `/x/passport-login/web/qrcode/poll` 的 data（poll.code 才是扫码状态）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct QrcodePollData {
    pub code: i64,
    pub message: String,
}

impl Default for QrcodePollData {
    fn default() -> Self {
        Self {
            code: -1,
            message: String::new(),
        }
    }
}

/// 扫码轮询结果（域模型：附带从响应头收集的登录 Cookie）。
#[derive(Debug, Clone, Default)]
pub struct QrcodePoll {
    pub code: i64,
    pub message: String,
    pub cookies: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_data_parses_and_tolerates_missing_fields() {
        let data: NavData = serde_json::from_str(
            r#"{
                "isLogin": true,
                "mid": 100,
                "uname": "tester",
                "face": "//i0.hdslb.com/a.jpg",
                "level_info": {"current_level": 6},
                "vipStatus": 1,
                "vipType": 2,
                "vip": {"label": {"text": "年度大会员"}},
                "wbi_img": {"img_url": "https://x/abc.png", "sub_url": "https://x/def.png"}
            }"#,
        )
        .expect("nav data");
        let status = NavStatus::from(data);
        assert!(status.is_login);
        assert_eq!(status.level, 6);
        assert_eq!(status.face, "https://i0.hdslb.com/a.jpg");
        // vip_label.text 缺失时回退到 vip.label.text
        assert_eq!(status.vip_label, "年度大会员");

        let empty: NavData = serde_json::from_str("{}").expect("empty nav data");
        assert!(!empty.is_login);
        assert!(empty.wbi_img.img_url.is_empty());
    }

    #[test]
    fn qrcode_poll_defaults_to_error_code() {
        let poll: QrcodePollData = serde_json::from_str("{}").expect("empty poll");
        assert_eq!(poll.code, -1);
    }
}
