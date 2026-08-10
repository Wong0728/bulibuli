//! 历史记录读写接口：按博主查询、删除单条记录与关键字搜索。

use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::{extract::Query, extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub(super) struct ByUidQuery {
    uid: String,
}

pub(super) async fn get_by_uid(
    State(state): State<SharedState>,
    Query(q): Query<ByUidQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = q.uid.trim();
    if uid.is_empty() {
        return Err(AppError::BadRequest("请提供博主UID".to_string()));
    }
    let history = state.business.history_service.list_by_uid(uid).await?;
    Ok(Json(ApiResponse::success(json!({
        "history": history.iter().map(|h| h.to_api()).collect::<Vec<_>>(),
    }))))
}

#[derive(Deserialize)]
pub(super) struct DeleteHistoryRequest {
    /// 视频 BV 号
    bvid: String,
    /// 是否同时删除本地文件（视频/封面/弹幕/字幕）。默认 true。
    delete_files: Option<bool>,
}

/// 删除单条视频记录（抽屉"删除本地文件 + 记录"按钮用）。
///
/// 行为下沉到 `HistoryService::delete_record`：
/// 1. 删除本地视频文件（`history.file_path`）
/// 2. 删除本地封面（`history.cover_local_path`）
/// 3. 删除弹幕 / 字幕侧车文件
/// 4. 删除 `download_task` 表中该 bvid 的所有任务
/// 5. 删除 `history` 表中该 bvid 的记录
pub(super) async fn delete_history(
    State(state): State<SharedState>,
    Json(req): Json<DeleteHistoryRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = req.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }
    let delete_files = req.delete_files.unwrap_or(true);

    match state
        .business
        .history_service
        .delete_record(bvid, delete_files)
        .await?
    {
        Some((removed_files, removed_tasks)) => Ok(Json(ApiResponse::with_message(
            json!({
                "bvid": bvid,
                "removed_files": removed_files,
                "removed_tasks": removed_tasks,
            }),
            "记录已删除",
        ))),
        None => Err(AppError::NotFound("未找到该视频记录".to_string())),
    }
}

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    keyword: String,
    page: Option<u64>,
    page_size: Option<u64>,
}

pub(super) async fn search_history(
    State(state): State<SharedState>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if q.keyword.trim().is_empty() {
        return Err(AppError::BadRequest("搜索关键字不能为空".to_string()));
    }
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(50).clamp(1, 100);
    let (history, total) = state
        .business
        .history_service
        .search(&q.keyword, page, page_size)
        .await?;
    Ok(Json(ApiResponse::success(json!({
        "history": history.iter().map(|h| h.to_api()).collect::<Vec<_>>(),
        "page": page,
        "page_size": page_size,
        "total": total,
    }))))
}
