//! 直播间 API 客户端：房间状态探测、房间信息与播放/弹幕配置。
//!
//! 播放地址保留旧版接口作为播放协议兼容路径；弹幕配置只允许
//! WBI 签名的 `getDanmuInfo`，不会回退到旧版 `getConf`。
//! 接口域名：`api.live.bilibili.com`，走 `api_client`（严格 TLS）。
//!
//! 按职责拆分为兄弟模块：
//! - 本文件：UID 批量状态探测与房间信息查询（status / recent / init / info）
//! - [`playurl`]：播放地址解析（新版 getRoomPlayInfo + 旧版 playUrl 回退）
//! - [`danmu`]：弹幕 WebSocket 连接配置（WBI 签名 getDanmuInfo）
//! - [`stream_url`]：直播流地址契约校验、URL 展平与解析

use anyhow::Result;
use std::collections::HashMap;
use tracing::{debug, warn};

use super::models::live::{LiveBatchStatus, LiveBatchStatusMap, LiveRoomInfo, LiveRoomInit};
use super::BiliApi;

#[path = "live/danmu.rs"]
mod danmu;
#[path = "live/playurl.rs"]
mod playurl;
#[path = "live/stream_url.rs"]
mod stream_url;

/// uid 无效（<=0）警告的进程级闸门：只告警一次，避免每个监控周期刷屏。
static WARNED_INVALID_LIVE_UID: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn warn_invalid_live_uid_once() {
    use std::sync::atomic::Ordering;
    if !WARNED_INVALID_LIVE_UID.swap(true, Ordering::Relaxed) {
        warn!(
            "存在 uid<=0 的直播源：UID 批量探测永远无法覆盖（已跳过），\
             请重新保存直播源以解析真实 uid（本警告仅记录一次）"
        );
    }
}

impl BiliApi {
    /// 监控服务使用的批量状态探测。
    /// 一次请求覆盖所有已保存的主播，避免按房间同步轮询造成请求尖峰。
    pub async fn live_status_by_uids(
        &self,
        uids: &[i64],
        cookies: &str,
    ) -> Result<HashMap<i64, LiveBatchStatus>> {
        let uids_len = uids.len();
        // uid<=0 的直播源（历史脏数据/解析失败遗留）永远无法通过 UID 批量探测：
        // 跳过且只警告一次，而不是每个监控周期（约 30s）重复报错。
        let uids = uids
            .iter()
            .copied()
            .filter(|uid| *uid > 0)
            .collect::<Vec<_>>();
        if uids.len() != uids_len {
            warn_invalid_live_uid_once();
        }
        if uids.is_empty() {
            return Ok(HashMap::new());
        }
        let url = "https://api.live.bilibili.com/room/v1/Room/get_status_info_by_uids";
        let enriched = self.enrich_cookies(cookies).await?;
        let credential = crate::services::credential::Credential::from_cookie_header(&enriched);
        let request = self
            .client_for(url)
            .post(url)
            .json(&serde_json::json!({"uids": uids}))
            .header("User-Agent", &self.config.user_agent)
            .header("Referer", "https://live.bilibili.com/")
            .header("Origin", "https://www.bilibili.com")
            .header("Accept", "application/json, text/plain, */*")
            .header("Cookie", credential.to_cookie_header());
        // 后台批量探测走专用限流器：仅在发送处扣减后台配额，
        // 不再先过 background limiter 又在发送时扣前台配额（双扣减）。
        let response = self
            .send_with_retry_limited(request, &self.background_rate_limiter)
            .await?;
        // 这是该端点的唯一一次请求（无回退接口），必须用普通解析器：silent 解析会吞掉
        // 风控/未登录信号，直播侧只留下日志，调用方只能靠错误字符串匹配做退避。
        let data: LiveBatchStatusMap = self
            .parse_data(response, "live_status_info_by_uids")
            .await?;
        Ok(data
            .into_values()
            .filter(|item| item.uid > 0)
            .map(|item| (item.uid, item))
            .collect())
    }

    /// 获取用于补齐重连间隔的小窗口最新消息。
    pub async fn live_recent_danmaku(
        &self,
        room_id: i64,
        cookies: &str,
    ) -> Result<Vec<serde_json::Value>> {
        let mut params = HashMap::new();
        params.insert("roomid".to_owned(), room_id.to_string());
        let enriched = self.enrich_cookies(cookies).await?;
        let request = self
            .build_get_request(
                "https://api.live.bilibili.com/xlive/web-room/v1/dM/gethistory",
                &params,
                "https://live.bilibili.com/",
                &enriched,
            )
            .await;
        let response = self.send_with_retry(request).await?;
        let data = self
            .parse_data_silent::<serde_json::Value>(response, "live_recent_danmaku")
            .await?;
        Ok(data
            .get("room")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(10)
            .collect())
    }

    /// 短号换长号 + 直播状态（`room_init`）。
    ///
    /// 用户输入的直播间号可能是短号，必须先调此接口拿到真实 `room_id`。
    /// 后续所有直播 API 均使用 `room_id`（长号）。
    pub async fn live_room_init(&self, room_id: i64, cookies: &str) -> Result<LiveRoomInit> {
        let mut params = HashMap::new();
        params.insert("id".to_string(), room_id.to_string());

        let url = "https://api.live.bilibili.com/room/v1/Room/room_init";
        let referer = "https://live.bilibili.com/";
        debug!(url, room_id, "B站直播 API 请求: room_init");

        let enriched = self.enrich_cookies(cookies).await?;
        let request = self
            .build_get_request(url, &params, referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        self.parse_data::<LiveRoomInit>(resp, "live_room_init")
            .await
    }

    /// 获取直播间详细信息（`get_info`）。
    ///
    /// 返回标题、主播 UID、封面、在线人数、分区等。
    pub async fn live_get_info(&self, room_id: i64, cookies: &str) -> Result<LiveRoomInfo> {
        let mut params = HashMap::new();
        params.insert("room_id".to_string(), room_id.to_string());

        let url = "https://api.live.bilibili.com/room/v1/Room/get_info";
        let referer = "https://live.bilibili.com/";
        debug!(url, room_id, "B站直播 API 请求: get_info");

        let enriched = self.enrich_cookies(cookies).await?;
        let request = self
            .build_get_request(url, &params, referer, &enriched)
            .await;
        let resp = self.send_with_retry(request).await?;
        self.parse_data::<LiveRoomInfo>(resp, "live_get_info").await
    }
}
