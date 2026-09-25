//! 搜索页「已添加博主」接口：收藏身份的增删查，与监控自动任务相互独立。

use super::helpers::map_blogger_insert_error;
use super::DeleteBloggerRequest;
use crate::error::{ApiResponse, AppError};
use crate::models::operation_log::OperationTarget;
use crate::services::blogger::{BloggerUpdate, NewBlogger};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub(crate) struct AddSavedBloggerRequest {
    uid: String,
    name: Option<String>,
    face: Option<String>,
    sign: Option<String>,
    level: Option<i32>,
    fans: Option<i64>,
    #[serde(default)]
    expected_version: Option<i32>,
}

/// 搜索页"已添加博主"列表；不包含仅存在于自动任务中的博主。
pub(crate) async fn list_saved_bloggers(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bloggers = state.business.blogger_service.list_saved().await?;
    Ok(Json(ApiResponse::success(json!({
        "bloggers": bloggers.iter().map(crate::models::blogger::Model::to_api).collect::<Vec<_>>(),
    }))))
}

/// 收藏一个搜索结果。若同 UID 已有自动任务，只增加收藏身份，不改动任务配置。
pub(crate) async fn add_saved_blogger(
    State(state): State<SharedState>,
    Json(req): Json<AddSavedBloggerRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = crate::services::file_safety::validate_uid(&req.uid)?
        .as_str()
        .to_string();
    if let Some(existing) = state.business.blogger_service.find_by_uid(&uid).await? {
        if existing.is_saved {
            return Ok(Json(ApiResponse::with_message(
                json!({ "blogger": existing.to_api() }),
                "博主已在添加列表中",
            )));
        }
        let id = existing.id;
        let guard = state
            .infra
            .conflict_guard
            .check_and_bump(
                OperationTarget::Blogger,
                &id.to_string(),
                req.expected_version,
            )
            .await?;
        if let Err(error) = state
            .business
            .blogger_service
            .apply_update(
                existing,
                BloggerUpdate {
                    is_saved: Some(true),
                    ..Default::default()
                },
            )
            .await
        {
            if let Err(rollback_error) = guard.rollback().await {
                tracing::warn!(%rollback_error, "博主版本守卫回滚失败");
            }
            return Err(error.into());
        }
        guard.commit();
        let blogger = state
            .business
            .blogger_service
            .find_by_id(id)
            .await?
            .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?;
        return Ok(Json(ApiResponse::with_message(
            json!({ "blogger": blogger.to_api() }),
            "博主已添加",
        )));
    }

    let name = req
        .name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let blogger = state
        .business
        .blogger_service
        .add_blogger(NewBlogger {
            uid,
            name,
            min_interval: 60,
            max_interval: 300,
            face: req.face.filter(|value| !value.is_empty()),
            sign: req.sign.filter(|value| !value.is_empty()),
            level: req.level,
            fans: req.fans,
            download_video: true,
            download_danmaku: true,
            download_comments: true,
            download_cover: true,
            burn_danmaku: false,
            burn_subtitle: false,
            series_filter_regex: None,
            active_windows: None,
            monitor_enabled: false,
            is_saved: true,
            has_auto_task: false,
        })
        .await
        .map_err(map_blogger_insert_error)?;
    Ok(Json(ApiResponse::with_message(
        json!({ "blogger": blogger.to_api() }),
        "博主已添加",
    )))
}

pub(crate) async fn delete_saved_blogger(
    State(state): State<SharedState>,
    Json(req): Json<DeleteBloggerRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let guard = state
        .infra
        .conflict_guard
        .check_and_bump(
            OperationTarget::Blogger,
            &req.id.to_string(),
            req.expected_version,
        )
        .await?;
    if state
        .business
        .blogger_service
        .remove_saved(req.id)
        .await?
        .is_none()
    {
        if let Err(rollback_error) = guard.rollback().await {
            tracing::warn!(%rollback_error, "博主版本守卫回滚失败");
        }
        return Err(AppError::NotFound("未找到已添加博主".to_string()));
    }
    guard.commit();
    Ok(Json(ApiResponse::with_message(json!({}), "博主已移除")))
}
