//! 直播 API 路由装配。
//!
//! 按职责拆分的子模块：
//! - `recording`：录制启动/停止与状态查询；
//! - `source`：直播源的增删改；
//! - `dashboard`：直播页看板聚合轮询；
//! - `burn`：录制互动弹幕烧录；
//! - `merge`：录制合并任务查询与取消；
//! - `history`：录制历史、实时事件与恢复列表。

mod burn;
mod dashboard;
mod history;
mod merge;
mod recording;
mod source;

use crate::state::SharedState;
use axum::{
    routing::{get, post},
    Router,
};
use serde::Deserialize;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/live/room-info", get(recording::room_info))
        .route("/api/live/start", post(recording::start_recording))
        .route("/api/live/stop", post(recording::stop_recording))
        .route("/api/live/status", get(recording::recording_status))
        .route("/api/live/dashboard", get(dashboard::dashboard))
        .route("/api/live/source/add", post(source::add_source))
        .route("/api/live/source/update", post(source::update_source))
        .route("/api/live/source/delete", post(source::delete_source))
        .route("/api/live/events", get(history::events))
        .route("/api/live/history", get(history::history))
        .route(
            "/api/live/history/{recording_id}",
            get(history::history_item),
        )
        .route(
            "/api/live/history/{recording_id}/merge",
            post(merge::start_merge),
        )
        .route(
            "/api/live/history/{recording_id}/burn-danmaku",
            post(burn::burn_recording_danmaku),
        )
        .route(
            "/api/live/history/{recording_id}/open-directory",
            post(history::open_history_directory),
        )
        .route("/api/live/recovery", get(history::recovery))
        .route("/api/live/merge/{job_id}", get(merge::merge_job))
        .route("/api/live/merge/{job_id}/cancel", post(merge::cancel_merge))
}

/// 录制启动/停止与直播源删除共用的请求体。
#[derive(Deserialize)]
pub(super) struct RoomBody {
    room_id: i64,
    #[serde(default)]
    expected_version: Option<i32>,
}

/// 实时事件流查询参数（`history` 子模块使用）。
#[derive(Deserialize)]
pub(super) struct EventsQuery {
    room_id: i64,
    recording_id: Option<i32>,
    #[serde(default)]
    after_seq: u64,
    limit: Option<usize>,
}

/// 录制历史列表查询参数（`history` 子模块使用）。
#[derive(Deserialize)]
pub(super) struct HistoryQuery {
    limit: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_error_strips_signed_url_tokens() {
        let message = "获取流失败: https://cdn.example.com/live.flv?sign=abc&token=secret";
        let result = history::public_error(message);
        assert!(result.contains("https://cdn.example.com/live.flv"));
        assert!(!result.contains("token=secret"));
        assert!(!result.contains("sign=abc"));
    }

    #[test]
    fn public_error_redacts_absolute_paths() {
        assert_eq!(
            history::public_error("D:\\downloads\\live\\out.flv"),
            "diagnostic redacted"
        );
        assert_eq!(
            history::public_error("/var/data/out.flv"),
            "diagnostic redacted"
        );
    }

    #[test]
    fn public_error_keeps_plain_messages() {
        assert_eq!(history::public_error(" 认证被拒绝 "), "认证被拒绝");
    }
}
