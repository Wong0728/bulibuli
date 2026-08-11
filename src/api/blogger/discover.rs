//! B 站侧博主检索：搜索用户、UID 校验与合集/系列查询。

use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::{extract::Query, extract::State, Json};
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

#[derive(Deserialize)]
pub(super) struct SearchBloggersQuery {
    q: String,
    page: Option<u32>,
    page_size: Option<u32>,
}

pub(super) async fn search_bloggers(
    State(state): State<SharedState>,
    Query(q): Query<SearchBloggersQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let keyword = q.q.trim();
    if keyword.is_empty() {
        return Err(AppError::BadRequest("搜索关键字不能为空".to_string()));
    }
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(20).clamp(1, 50);
    let cookies = state
        .infra
        .settings_service
        .cookie_header()
        .await
        .unwrap_or_default();
    match state
        .bili
        .bili_api
        .search_users(keyword, &cookies, page, page_size)
        .await
    {
        Ok(result) => Ok(Json(ApiResponse::success(serde_json::to_value(result)?))),
        Err(error) => {
            warn!(keyword, page, page_size, %error, "搜索博主失败");
            Err(error.into())
        }
    }
}

#[derive(Deserialize)]
pub(super) struct ValidateUidQuery {
    uid: i64,
}

pub(super) async fn validate_uid(
    State(state): State<SharedState>,
    Query(q): Query<ValidateUidQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    crate::api::validate_bili_id("UID", q.uid)?;
    info!("开始校验博主 UID: {}", q.uid);
    let cookies = state
        .infra
        .settings_service
        .cookie_header()
        .await
        .unwrap_or_default();
    // `get_user_info` 成功即表示 UID 存在（exists=true）；不存在或风控错误直接上抛。
    let profile = state.bili.bili_api.get_user_info(q.uid, &cookies).await?;
    info!("博主 UID {} 校验成功: name={}", q.uid, profile.name);
    Ok(Json(ApiResponse::success(serde_json::to_value(profile)?)))
}

#[derive(Deserialize)]
pub(super) struct GetSeriesQuery {
    uid: i64,
}

/// 获取 UP 主合集/系列列表。
pub(super) async fn get_series(
    State(state): State<SharedState>,
    Query(q): Query<GetSeriesQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    crate::api::validate_bili_id("UID", q.uid)?;
    let cookies = state.infra.settings_service.cookie_header().await?;
    match state.bili.bili_api.get_user_series(q.uid, &cookies).await {
        Ok(result) => Ok(Json(ApiResponse::success(serde_json::to_value(result)?))),
        Err(error) => {
            warn!(uid = q.uid, %error, "[API] /api/blogger/series 失败");
            Err(error.into())
        }
    }
}

#[derive(Deserialize)]
pub(super) struct GetSeriesVideosQuery {
    uid: i64,
    series_id: i64,
    #[serde(default)]
    collection_type: Option<String>,
    offset: Option<i32>,
    limit: Option<i32>,
}

/// 获取合集/系列内的视频列表。
pub(super) async fn get_series_videos(
    State(state): State<SharedState>,
    Query(q): Query<GetSeriesVideosQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let ctype = q.collection_type.as_deref().unwrap_or("series");
    crate::api::validate_bili_id("UID", q.uid)?;
    crate::api::validate_bili_id("合集 ID", q.series_id)?;
    if !matches!(ctype, "season" | "series") {
        return Err(AppError::BadRequest(
            "collection_type 只能是 season 或 series".to_string(),
        ));
    }
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(30);
    if offset < 0 || !(1..=30).contains(&limit) || offset % limit != 0 {
        return Err(AppError::BadRequest(
            "合集分页 offset 必须非负且按 1-30 的 limit 对齐".to_string(),
        ));
    }
    let cookies = state.infra.settings_service.cookie_header().await?;
    match state
        .bili
        .bili_api
        .get_series_videos(
            q.uid,
            q.series_id,
            ctype,
            &cookies,
            Some(offset),
            Some(limit),
        )
        .await
    {
        Ok(result) => Ok(Json(ApiResponse::success(serde_json::to_value(result)?))),
        Err(error) => {
            warn!(
                uid = q.uid,
                series_id = q.series_id,
                %error,
                "[API] /api/blogger/series-videos 失败"
            );
            Err(error.into())
        }
    }
}
