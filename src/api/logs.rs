use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::{extract::Query, extract::State, routing::get, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/logs/get", get(get_logs))
        .route("/api/logs/blogger", get(get_blogger_logs))
        .route("/api/logs/bvid", get(get_bvid_logs))
}

#[derive(Deserialize)]
struct LogQuery {
    limit: Option<u64>,
}

/// 三个日志端点统一的排序口径：底层 query_logs 固定 `created_at DESC + limit`
/// 取"最新 N 条"窗口，再统一 reverse 成时间升序（旧→新，最新在末尾）返回。
/// 前端（设置页全局日志、博主日志面板）均按"最新在最后 + 滚到底部"渲染；
/// 抽屉 bvid 日志此前未 reverse（新→旧），口径与其余两个不一致，现统一。
fn sorted_oldest_first(logs: Vec<crate::models::log::Model>) -> Vec<serde_json::Value> {
    let mut api_logs: Vec<_> = logs.into_iter().map(|l| l.to_api()).collect();
    api_logs.reverse();
    api_logs
}

async fn get_logs(
    State(state): State<SharedState>,
    Query(q): Query<LogQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 100);
    let logs = state
        .business
        .monitor_service
        .query_logs(None, None, limit)
        .await?;
    Ok(Json(ApiResponse::success(json!({
        "logs": sorted_oldest_first(logs)
    }))))
}

#[derive(Deserialize)]
struct BloggerLogQuery {
    uid: String,
    limit: Option<u64>,
}

async fn get_blogger_logs(
    State(state): State<SharedState>,
    Query(q): Query<BloggerLogQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = q.uid.trim();
    if uid.is_empty() {
        return Err(AppError::BadRequest("请提供博主UID".to_string()));
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 100);
    let logs = state
        .business
        .monitor_service
        .query_logs(Some(uid), None, limit)
        .await?;
    Ok(Json(ApiResponse::success(json!({
        "logs": sorted_oldest_first(logs)
    }))))
}

#[derive(Deserialize)]
struct BvidLogQuery {
    bvid: String,
    limit: Option<u64>,
}

/// 按 bvid 查询日志（抽屉"日志"区用）。只读。
async fn get_bvid_logs(
    State(state): State<SharedState>,
    Query(q): Query<BvidLogQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = q.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 100);
    let logs = state
        .business
        .monitor_service
        .query_logs(None, Some(bvid), limit)
        .await?;
    // 与其余两个日志端点统一：时间升序返回（最新在最后）。
    Ok(Json(ApiResponse::success(json!({
        "logs": sorted_oldest_first(logs)
    }))))
}
