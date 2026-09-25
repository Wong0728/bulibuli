//! 风控/登录失效系统事件的前端推送与节流：`bili:risk-control`、
//! `bili:auth-expired` 通过 WebSocket 广播，同一事件在冷却窗口内只推一次。

use crate::error::{BiliApiError, BiliErrorKind};
use serde_json::json;
use tracing::warn;

use crate::services::bili_api::models::RiskControlData;
use crate::services::bili_api::BiliApi;

/// 同一系统事件的最小通知间隔。风控/登录失效期间每条失败请求都会触发通知，
/// 而前端 `showSystemModal` 会用同标题顶掉旧弹窗——不加节流用户既读不完也关不掉。
const SYSTEM_NOTICE_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

impl BiliApi {
    /// 供不走统一 JSON 信封的接口（protobuf / 二进制流）把 HTTP 层风控与登录态
    /// 信号送回前端：这些响应不会被 `read_envelope` 解析，否则前端只能看到 partial。
    pub async fn notify_http_status(&self, api_name: &str, status: u16) {
        let code = match status {
            401 => -101,
            403 | 412 => -403,
            429 => 429,
            _ => return,
        };
        let error = BiliApiError::classify(code, format!("{api_name} HTTP {status}"));
        if matches!(error.kind, BiliErrorKind::RiskControl) {
            self.wbi_keys.invalidate().await;
        }
        self.notify_bili_error(&error, &serde_json::Value::Null)
            .await;
    }

    /// 同一系统事件在 `SYSTEM_NOTICE_COOLDOWN` 内只允许推送一次。
    pub(super) async fn system_notice_due(&self, event: &'static str) -> bool {
        let mut notices = self.last_system_notice.write().await;
        if notices
            .get(event)
            .is_some_and(|at| at.elapsed() < SYSTEM_NOTICE_COOLDOWN)
        {
            return false;
        }
        notices.insert(event, std::time::Instant::now());
        true
    }

    pub(in crate::services::bili_api) async fn notify_bili_error(
        &self,
        error: &BiliApiError,
        data: &serde_json::Value,
    ) {
        let event = match error.kind {
            BiliErrorKind::RiskControl => Some("bili:risk-control"),
            BiliErrorKind::Unauthorized => Some("bili:auth-expired"),
            _ => None,
        };
        let Some(event) = event else {
            return;
        };
        if !self.system_notice_due(event).await {
            return;
        }
        let mut payload = json!({ "code": error.code, "message": error.message });
        if matches!(error.kind, BiliErrorKind::RiskControl) {
            if let Ok(risk) = serde_json::from_value::<RiskControlData>(data.clone()) {
                if let Some(v_voucher) = risk.v_voucher.filter(|value| !value.is_empty()) {
                    payload["v_voucher"] = json!(v_voucher);
                }
            }
        }
        if let Err(notify_error) = self
            .ws
            .broadcast_system(event, payload, crate::ws::NoticeAudience::CanAct)
            .await
        {
            warn!("推送 B站系统事件失败: {notify_error}");
        }
    }
}
