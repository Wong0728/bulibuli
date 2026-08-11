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
    let mut logs: Vec<_> = logs.into_iter().map(|l| l.to_api()).collect();
    logs.reverse();
    Ok(Json(ApiResponse::success(json!({ "logs": logs }))))
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
    let mut logs: Vec<_> = logs.into_iter().map(|l| l.to_api()).collect();
    logs.reverse();
    Ok(Json(ApiResponse::success(json!({ "logs": logs }))))
}

#[derive(Deserialize)]
struct BvidLogQuery {
    bvid: String,
    limit: Option<u64>,
}

/// 按 bvid 查询日志（抽屉"日志"区用）。按时间倒序，只读。
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
    // 保持倒序（最新在最上面），与抽屉"滚动列表，按时间倒序"一致
    let logs: Vec<_> = logs.into_iter().map(|l| l.to_api()).collect();
    Ok(Json(ApiResponse::success(json!({ "logs": logs }))))
}
