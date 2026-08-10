use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Credential {
    pub sessdata: Option<String>,
    pub bili_jct: Option<String>,
    pub buvid3: Option<String>,
    pub buvid4: Option<String>,
    pub bili_ticket: Option<String>,
    pub dede_user_id: Option<String>,
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

impl Credential {
    pub fn from_cookie_header(header: &str) -> Self {
        let mut credential = Self::default();
        for part in header.split(';') {
            let Some((name, value)) = part.trim().split_once('=') else {
                continue;
            };
            let value = value.trim().to_string();
            match name.trim() {
                "SESSDATA" => credential.sessdata = Some(value),
                "bili_jct" => credential.bili_jct = Some(value),
                "buvid3" => credential.buvid3 = Some(value),
                "buvid4" => credential.buvid4 = Some(value),
                "bili_ticket" => credential.bili_ticket = Some(value),
                "DedeUserID" => credential.dede_user_id = Some(value),
                other => {
                    credential.extra.insert(other.to_string(), value);
                }
            }
        }
        credential
    }

    pub fn to_cookie_header(&self) -> String {
        let mut pairs = Vec::new();
        for (name, value) in [
            ("SESSDATA", self.sessdata.as_deref()),
            ("bili_jct", self.bili_jct.as_deref()),
            ("buvid3", self.buvid3.as_deref()),
            ("buvid4", self.buvid4.as_deref()),
            ("bili_ticket", self.bili_ticket.as_deref()),
            ("DedeUserID", self.dede_user_id.as_deref()),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                // Cookie 值不应含 `;` 或换行符，否则会破坏 Cookie 头结构。
                let safe = sanitize_cookie_value(value);
                pairs.push(format!("{name}={safe}"));
            }
        }
        let mut extra: Vec<_> = self.extra.iter().collect();
        extra.sort_by(|left, right| left.0.cmp(right.0));
        pairs.extend(extra.into_iter().map(|(name, value)| {
            let safe = sanitize_cookie_value(value);
            format!("{name}={safe}")
        }));
        pairs.join("; ")
    }

    pub fn is_logged_in(&self) -> bool {
        self.sessdata
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    }
}

/// 清理 Cookie 值：移除 `;`、换行符与控制字符，防止注入额外的 Cookie 对。
/// 正常的 B 站凭据值不会包含这些字符；出现时视为异常输入，直接丢弃。
fn sanitize_cookie_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c != ';' && *c != '\r' && *c != '\n' && !c.is_control())
        .collect()
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credential")
            .field("has_sessdata", &self.sessdata.is_some())
            .field("has_bili_jct", &self.bili_jct.is_some())
            .field("has_buvid3", &self.buvid3.is_some())
            .field("has_buvid4", &self.buvid4.is_some())
            .field("has_bili_ticket", &self.bili_ticket.is_some())
            .field("has_dede_user_id", &self.dede_user_id.is_some())
            .field("extra_count", &self.extra.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_contains_secret_values() {
        let credential =
            Credential::from_cookie_header("SESSDATA=secret; bili_jct=csrf; buvid3=device");
        let debug = format!("{credential:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("csrf"));
        assert!(!debug.contains("device"));
        assert!(credential.is_logged_in());
    }

    #[test]
    fn cookie_header_round_trips_known_fields() {
        let credential = Credential::from_cookie_header("SESSDATA=abc; bili_jct=def; custom=value");
        let header = credential.to_cookie_header();
        assert!(header.contains("SESSDATA=abc"));
        assert!(header.contains("custom=value"));
    }

    #[test]
    fn cookie_value_with_semicolon_is_stripped() {
        let credential = Credential {
            sessdata: Some("abc;evil=injected".to_string()),
            ..Default::default()
        };
        let header = credential.to_cookie_header();
        assert!(header.contains("SESSDATA=abcevil=injected"));
        assert!(!header.contains("; evil=injected"));
    }
}
