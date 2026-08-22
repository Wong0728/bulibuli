//! 历史记录读写接口：按博主查询、删除单条记录与关键字搜索。

use crate::error::{ApiResponse, AppError};
use crate::models::operation_log::OperationTarget;
use crate::services::auth::ClientInfo;
use crate::services::file_safety::ensure_existing_within_root;
use crate::services::security_config::can_open_directory;
use crate::state::SharedState;
use axum::{extract::Query, extract::State, Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Deserialize)]
pub(super) struct ByUidQuery {
    uid: String,
}

pub(super) async fn get_by_uid(
    State(state): State<SharedState>,
    Query(q): Query<ByUidQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = crate::services::file_safety::validate_uid(&q.uid)?
        .as_str()
        .to_owned();
    let history = state.business.history_service.list_by_uid(&uid).await?;
    Ok(Json(ApiResponse::success(json!({
        "history": history.iter().map(|h| h.to_api()).collect::<Vec<_>>(),
    }))))
}

#[derive(Deserialize)]
pub(super) struct DeleteHistoryRequest {
    /// 视频 BV 号
    bvid: String,
    /// 可选的精确 history 记录；缺省时删除该 BV 的全部分 P 记录。
    history_id: Option<i32>,
    /// 是否同时删除本地文件（视频/封面/弹幕/字幕）。默认 true。
    delete_files: Option<bool>,
    /// 精确删除时使用的历史记录版本；缺省保持旧客户端的最后写入胜出语义。
    #[serde(default)]
    expected_version: Option<i32>,
}

#[derive(Deserialize)]
pub(super) struct OpenDirectoryRequest {
    bvid: String,
    history_id: Option<i32>,
    /// 由 scan_files 返回的相对路径；不接受客户端绝对路径。
    path: Option<String>,
}

pub(super) async fn open_directory(
    State(state): State<SharedState>,
    Extension(client): Extension<ClientInfo>,
    Json(req): Json<OpenDirectoryRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if !can_open_directory(&state.bili.security.current().mode, client.ip) {
        return Err(AppError::BadRequest(
            "仅本机访问支持打开所在目录".to_string(),
        ));
    }
    let bvid = req.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }
    let history = match req.history_id {
        Some(id) => state
            .business
            .history_service
            .find_by_id(id)
            .await?
            .filter(|history| history.bvid == bvid),
        None => state.business.history_service.find_by_bvid(bvid).await?,
    }
    .ok_or_else(|| AppError::NotFound("未找到该视频记录".to_string()))?;
    let target = if let Some(relative) = req.path.as_deref().filter(|path| !path.trim().is_empty())
    {
        let files = state
            .business
            .history_service
            .scan_files(bvid, history.uid.as_deref(), history.file_path.as_deref())
            .await;
        if !files.iter().any(|file| file.path == relative) {
            return Err(AppError::BadRequest("文件不属于该视频记录".to_string()));
        }
        state
            .business
            .history_service
            .resolve_download_relative_path(relative)
            .ok_or_else(|| AppError::BadRequest("文件路径无效".to_string()))?
    } else {
        history
            .file_path
            .as_deref()
            .map(|path| Path::new(path).to_path_buf())
            .ok_or_else(|| AppError::BadRequest("该记录没有可打开的文件".to_string()))?
    };
    ensure_existing_within_root(&state.infra.paths.download_dir, &target).await?;
    if !tokio::fs::try_exists(&target).await.unwrap_or(false) {
        return Err(AppError::NotFound("文件不存在".to_string()));
    }
    let directory = target.parent().map(|path| path.to_path_buf());
    let directory = match directory {
        Some(path)
            if tokio::fs::metadata(&path)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false) =>
        {
            path
        }
        _ => return Err(AppError::NotFound("文件所在目录不存在".to_string())),
    };
    open::that(directory)
        .map_err(|_| AppError::Internal("open download directory failed".to_string()))?;
    Ok(Json(ApiResponse::with_message(
        json!({"bvid": bvid}),
        "已打开文件所在目录",
    )))
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
    // 有意的产品契约：省略 delete_files 时默认连带删除本地文件（与前端"删除"按钮
    // 的预期一致）。API 文档已声明；需要仅删记录的调用方必须显式传 false。
    let delete_files = req.delete_files.unwrap_or(true);

    let guard = if let Some(expected_version) = req.expected_version {
        let history_id = req
            .history_id
            .ok_or_else(|| AppError::BadRequest("带版本删除时必须提供 history_id".to_string()))?;
        let history = state
            .business
            .history_service
            .find_by_id(history_id)
            .await?
            .filter(|history| history.bvid == bvid)
            .ok_or_else(|| AppError::NotFound("未找到该视频记录".to_string()))?;
        Some(
            state
                .infra
                .conflict_guard
                .check_and_bump(
                    OperationTarget::History,
                    &history.id.to_string(),
                    Some(expected_version),
                )
                .await?,
        )
    } else {
        None
    };

    match state
        .business
        .history_service
        .delete_record(bvid, delete_files, req.history_id)
        .await
    {
        Ok(Some((removed_files, removed_tasks))) => {
            if let Some(guard) = guard {
                guard.commit();
            }
            Ok(Json(ApiResponse::with_message(
                json!({
                    "bvid": bvid,
                    "removed_files": removed_files,
                    "removed_tasks": removed_tasks,
                }),
                "记录已删除",
            )))
        }
        Ok(None) => {
            if let Some(guard) = guard {
                let _ = guard.rollback().await;
            }
            Err(AppError::NotFound("未找到该视频记录".to_string()))
        }
        Err(error) => {
            if let Some(guard) = guard {
                let _ = guard.rollback().await;
            }
            Err(error)
        }
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
