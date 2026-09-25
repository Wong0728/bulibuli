//! UP 主投稿列表分页接口（`POST /api/video/get-videos`）。

use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
pub(crate) struct GetVideosRequest {
    uid: String,
    limit: Option<i32>,
    offset: Option<i64>,
}

/// B 站 `space/wbi/arc/search` 的单页上限。`bili_api::user_space::get_user_videos_page`
/// 内部把 `ps` 钳制到 `1..=50` 并原样回写 `page_size`/`offset`，API 层换算页码时
/// 必须用同一个有效页大小：否则 limit 落在 51..=100 时会"按 limit 算页码、按 50 取
/// 窗口"，相邻两次请求的窗口既重叠又漏项，且客户端回显服务端 `offset` 后再叠加自己的
/// limit 会撞上 `offset % limit != 0` 的 400，"加载更多"就此卡死。
const MAX_PAGE_SIZE: i32 = 50;

/// 把客户端的 `(limit, offset)` 换算成 B 站的 `(pn, ps)`。
///
/// 返回 `(pn, ps)`，其中 `ps = min(limit, MAX_PAGE_SIZE)` 就是真正发出去的页大小，
/// 与响应体里的 `page_size` 一致，调用方按 `ps` 递增 `offset` 即可无重叠无漏项地翻页。
fn resolve_page_window(limit: i32, offset: i64) -> Result<(i32, i32), String> {
    // 与 settings.rs 的 manual_query_limit 校验（1..=100）保持一致：
    // 此前 1..=50 会让把设置调到 51-100 的用户所有默认请求恒 400。
    if !(1..=100).contains(&limit) {
        return Err("视频列表 limit 必须在 1 到 100 之间".to_string());
    }
    let page_size = limit.min(MAX_PAGE_SIZE);
    if offset < 0 || offset % i64::from(page_size) != 0 {
        return Err("视频列表 offset 必须是非负 page size 整数倍".to_string());
    }
    let page = i32::try_from(offset / i64::from(page_size) + 1)
        .map_err(|_| "视频列表页码超出范围".to_string())?;
    Ok((page, page_size))
}

pub(crate) async fn get_videos(
    State(state): State<SharedState>,
    Json(req): Json<GetVideosRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    let uid = req.uid.trim();
    if uid.is_empty() {
        info!("[API] /api/video/get-videos 请求失败: UID为空");
        return Err(AppError::BadRequest("请输入用户UID".to_string()));
    }
    let uid_int: i64 = uid.parse().map_err(|_| {
        info!(
            "[API] /api/video/get-videos 请求失败: UID格式错误 uid={}",
            uid
        );
        AppError::BadRequest("UID必须是数字".to_string())
    })?;
    crate::api::validate_bili_id("UID", uid_int)?;

    info!(
        "[API] /api/video/get-videos 请求: uid={}, limit={:?}",
        uid_int, req.limit
    );

    let default_limit = state
        .infra
        .settings_service
        .current()
        .query
        .manual_query_limit;
    let limit = req.limit.unwrap_or(default_limit);
    let offset = req.offset.unwrap_or(0);
    let (page, page_size) = resolve_page_window(limit, offset).map_err(AppError::BadRequest)?;

    let cookies = state.infra.settings_service.cookie_header().await?;
    let result = state
        .bili
        .bili_api
        .get_user_videos_page(uid_int, &cookies, page, page_size, false)
        .await?;
    info!(
        "[API] /api/video/get-videos 成功: uid={}, 返回视频数={}",
        uid_int,
        result.videos.len()
    );
    Ok(Json(ApiResponse::success(serde_json::to_value(result)?)))
}

#[cfg(test)]
mod tests {
    use super::resolve_page_window;

    /// limit 未超 B 站单页上限时，页大小原样透传，页码按 offset/limit 递增。
    #[test]
    fn resolve_page_window_keeps_limit_at_or_below_bilibili_cap() {
        assert_eq!(resolve_page_window(20, 0).unwrap(), (1, 20));
        assert_eq!(resolve_page_window(20, 20).unwrap(), (2, 20));
        assert_eq!(resolve_page_window(20, 40).unwrap(), (3, 20));
        assert_eq!(resolve_page_window(50, 0).unwrap(), (1, 50));
    }

    /// 回归：limit 落在 51..=100 时，页码必须按钳制后的 ps=50 换算。
    /// 此前页码按客户端 limit 计算、窗口按 50 抓取，两者错位导致"加载更多"
    /// 反复取回同一段数据；回显服务端 offset 后还会撞上 offset 校验的 400。
    #[test]
    fn resolve_page_window_clamps_effective_size_above_bilibili_cap() {
        assert_eq!(resolve_page_window(100, 0).unwrap(), (1, 50));
        assert_eq!(resolve_page_window(100, 50).unwrap(), (2, 50));
        assert_eq!(resolve_page_window(100, 100).unwrap(), (3, 50));
        assert_eq!(resolve_page_window(60, 300).unwrap(), (7, 50));
    }

    /// 按返回的 ps 递增 offset 连续翻页：窗口既不重叠也不留空洞。
    #[test]
    fn resolve_page_window_walks_contiguous_windows() {
        let (limit, mut offset, mut previous_end) = (100i32, 0i64, 0i64);
        for expected_page in 1..=6i32 {
            let (page, ps) = resolve_page_window(limit, offset).unwrap();
            assert_eq!(page, expected_page);
            let start = i64::from(page - 1) * i64::from(ps);
            assert_eq!(start, previous_end, "第 {page} 页窗口起点应衔接上一页");
            previous_end = start + i64::from(ps);
            offset += i64::from(ps);
        }
    }

    #[test]
    fn resolve_page_window_rejects_out_of_range_limit() {
        for limit in [0, -1, 101, i32::MAX] {
            let error = resolve_page_window(limit, 0).unwrap_err();
            assert!(error.contains("limit"), "错误文案应指向 limit: {error}");
        }
    }

    #[test]
    fn resolve_page_window_rejects_misaligned_or_negative_offset() {
        // 20 在钳制区间内：offset 必须是 20 的整数倍。
        assert!(resolve_page_window(20, 10).is_err());
        assert!(resolve_page_window(20, -20).is_err());
        // 100 的有效页大小是 50：50 的整数倍可接受（旧实现会误判成 400）。
        assert!(resolve_page_window(100, 50).is_ok());
        assert!(resolve_page_window(100, 30).is_err());
    }
}
