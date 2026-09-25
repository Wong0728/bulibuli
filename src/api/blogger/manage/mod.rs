//! 博主监控任务管理接口：列表、添加/更新/删除自动任务与资料变更批量确认。
//!
//! 按职责拆分的子模块：
//! - `helpers`：校验、规范化与序列化工具；
//! - `saved`：搜索页「已添加博主」（收藏身份）的增删查。

mod helpers;
mod saved;

use crate::error::{ApiResponse, AppError};
use crate::models::operation_log::OperationTarget;
use crate::services::blogger::{BloggerUpdate, NewBlogger};
use crate::state::SharedState;
use axum::{extract::State, Json};
use chrono::Local;
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) use saved::{add_saved_blogger, delete_saved_blogger, list_saved_bloggers};

#[derive(Deserialize)]
pub(super) struct AcknowledgeBatchRequest {
    uids: Vec<String>,
}

pub(crate) async fn acknowledge_profile_changes(
    State(state): State<SharedState>,
    Json(request): Json<AcknowledgeBatchRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let affected = state
        .business
        .blogger_service
        .acknowledge_profile_changes(&request.uids)
        .await?;
    Ok(Json(ApiResponse::success(json!({ "affected": affected }))))
}

#[derive(Deserialize)]
pub(super) struct AddBloggerRequest {
    uid: String,
    name: Option<String>,
    min_interval: Option<i32>,
    max_interval: Option<i32>,
    download_video: Option<bool>,
    download_danmaku: Option<bool>,
    download_comments: Option<bool>,
    download_cover: Option<bool>,
    burn_danmaku: Option<bool>,
    burn_subtitle: Option<bool>,
    series_filter_regex: Option<String>,
    active_windows: Option<Vec<String>>,
    start_monitoring: Option<bool>,
    #[serde(default)]
    expected_version: Option<i32>,
}

/// 删除自动任务与删除已添加博主共用的请求体。
#[derive(Deserialize)]
pub(super) struct DeleteBloggerRequest {
    id: i32,
    #[serde(default)]
    expected_version: Option<i32>,
}

pub(crate) async fn list_bloggers(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bloggers = state.business.blogger_service.list_auto_tasks().await?;
    let now = Local::now();
    Ok(Json(ApiResponse::success(json!({
        "bloggers": bloggers.iter().map(|b| helpers::blogger_api_value(b, now)).collect::<Vec<_>>(),
        "server_timestamp": now.timestamp(),
        "server_utc_offset": now.format("%:z").to_string(),
    }))))
}

pub(crate) async fn add_blogger(
    State(state): State<SharedState>,
    Json(req): Json<AddBloggerRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = crate::services::file_safety::validate_uid(&req.uid)?
        .as_str()
        .to_string();
    let name = req.name.unwrap_or_default().trim().to_string();
    let min_interval = req.min_interval.unwrap_or(60);
    let max_interval = req.max_interval.unwrap_or(300);

    helpers::validate_intervals(min_interval, max_interval)?;
    let series_filter_regex = helpers::validate_series_filter(req.series_filter_regex)?;
    let active_windows = helpers::normalize_active_windows(req.active_windows)?;

    let existing = state.business.blogger_service.find_by_uid(&uid).await?;
    if existing
        .as_ref()
        .is_some_and(|blogger| blogger.has_auto_task)
    {
        return Err(AppError::Conflict("该博主已有自动任务".to_string()));
    }

    // 创建记录前获取当前资料，避免首屏展示空字段。
    // UID 由所选博主决定：拉取不到资料（不存在、风控或网络错误）直接拒绝创建，
    // 防止乱填 UID 也能添加成功。
    let cookies = state.infra.settings_service.cookie_header().await?;
    let fallback_name = if name.is_empty() {
        None
    } else {
        Some(name.clone())
    };
    let uid_i64 = uid
        .parse::<i64>()
        .map_err(|_| AppError::BadRequest("UID 格式无效".to_string()))?;
    let profile = state
        .bili
        .bili_api
        .get_user_info(uid_i64, &cookies, false)
        .await
        .map_err(|error| {
            tracing::warn!(uid, %error, "添加博主时拉取资料失败，拒绝创建");
            AppError::BadRequest(format!(
                "无法获取 UID {uid} 的用户资料（可能不存在或网络失败），请确认后重试"
            ))
        })?;
    let api_name = Some(profile.name).filter(|s| !s.is_empty());
    let api_face = Some(profile.face).filter(|s| !s.is_empty());
    let api_sign = Some(profile.sign).filter(|s| !s.is_empty());
    // 用户填写的是监控备注名，应优先保留；未填写时使用 API 昵称。
    let (final_name, face, sign, level, fans) = (
        fallback_name.or(api_name),
        api_face,
        api_sign,
        Some(profile.level as i32),
        Some(profile.fans),
    );

    let monitor_enabled = req.start_monitoring.unwrap_or(false);
    let blogger = if let Some(existing) = existing {
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
                    name: final_name,
                    min_interval: Some(min_interval),
                    max_interval: Some(max_interval),
                    download_video: Some(req.download_video.unwrap_or(true)),
                    download_danmaku: Some(req.download_danmaku.unwrap_or(true)),
                    download_comments: Some(req.download_comments.unwrap_or(true)),
                    download_cover: Some(req.download_cover.unwrap_or(true)),
                    burn_danmaku: Some(req.burn_danmaku.unwrap_or(false)),
                    burn_subtitle: Some(req.burn_subtitle.unwrap_or(false)),
                    series_filter_regex: Some(series_filter_regex.unwrap_or_default()),
                    active_windows: Some(active_windows),
                    monitor_enabled: Some(monitor_enabled),
                    has_auto_task: Some(true),
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
        state
            .business
            .blogger_service
            .find_by_id(id)
            .await?
            .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?
    } else {
        state
            .business
            .blogger_service
            .add_blogger(NewBlogger {
                uid: uid.clone(),
                name: final_name,
                min_interval,
                max_interval,
                face,
                sign,
                level,
                fans,
                download_video: req.download_video.unwrap_or(true),
                download_danmaku: req.download_danmaku.unwrap_or(true),
                download_comments: req.download_comments.unwrap_or(true),
                download_cover: req.download_cover.unwrap_or(true),
                burn_danmaku: req.burn_danmaku.unwrap_or(false),
                burn_subtitle: req.burn_subtitle.unwrap_or(false),
                series_filter_regex,
                active_windows,
                monitor_enabled,
                is_saved: false,
                has_auto_task: true,
            })
            .await
            .map_err(helpers::map_blogger_insert_error)?
    };
    tracing::info!("添加博主成功: {uid}");

    let now = Local::now();
    Ok(Json(ApiResponse::with_message(
        json!({
            "blogger_id": blogger.id,
            "blogger": helpers::blogger_api_value(&blogger, now),
            "server_timestamp": now.timestamp(),
            "server_utc_offset": now.format("%:z").to_string(),
        }),
        "博主已添加",
    )))
}

#[derive(Deserialize)]
pub(super) struct UpdateBloggerRequest {
    id: i32,
    uid: Option<String>,
    name: Option<String>,
    min_interval: Option<i32>,
    max_interval: Option<i32>,
    download_video: Option<bool>,
    download_danmaku: Option<bool>,
    download_comments: Option<bool>,
    download_cover: Option<bool>,
    burn_danmaku: Option<bool>,
    burn_subtitle: Option<bool>,
    series_filter_regex: Option<String>,
    /// 活跃检查时段（["HH:MM-HH:MM", ...]；空数组 = 清空恢复全天检查）
    active_windows: Option<Vec<String>>,
    /// 监控开关；与时段外的计划性等待是两个不同概念。
    monitor_enabled: Option<bool>,
    #[serde(default)]
    expected_version: Option<i32>,
}

pub(crate) async fn update_blogger(
    State(state): State<SharedState>,
    Json(req): Json<UpdateBloggerRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let b = state.business.blogger_service.find_by_id(req.id).await?;
    let Some(b) = b else {
        return Err(AppError::NotFound("未找到该博主".to_string()));
    };
    if !b.has_auto_task {
        return Err(AppError::NotFound("未找到该自动任务".to_string()));
    }

    let mut update = BloggerUpdate::default();
    if let Some(uid) = req.uid {
        let uid = uid.trim().to_string();
        if !uid.is_empty() {
            crate::services::file_safety::validate_uid(&uid)?;
            if state
                .business
                .blogger_service
                .uid_taken_by_other(&uid, req.id)
                .await?
            {
                return Err(AppError::Conflict("该UID已被其他博主使用".to_string()));
            }
            update.uid = Some(uid);
        }
    }
    if let Some(name) = req.name {
        update.name = Some(name.trim().to_string());
    }
    if let Some(min) = req.min_interval {
        if !(30..=3600).contains(&min) {
            return Err(AppError::BadRequest(
                "最小间隔必须在30-3600秒之间".to_string(),
            ));
        }
        update.min_interval = Some(min);
    }
    if let Some(max) = req.max_interval {
        if !(30..=7200).contains(&max) {
            return Err(AppError::BadRequest(
                "最大间隔必须在30-7200秒之间".to_string(),
            ));
        }
        update.max_interval = Some(max);
    }

    // 使用即将写入的值校验 min <= max。
    let min = update.min_interval.unwrap_or(b.min_interval);
    let max = update.max_interval.unwrap_or(b.max_interval);
    helpers::validate_intervals(min, max)?;

    // 下载/烧录策略字段
    update.download_video = req.download_video;
    update.download_danmaku = req.download_danmaku;
    update.download_comments = req.download_comments;
    update.download_cover = req.download_cover;
    update.burn_danmaku = req.burn_danmaku;
    update.burn_subtitle = req.burn_subtitle;
    if req.series_filter_regex.is_some() {
        update.series_filter_regex =
            Some(helpers::validate_series_filter(req.series_filter_regex)?.unwrap_or_default());
    }
    if let Some(windows) = req.active_windows {
        update.active_windows = Some(helpers::normalize_active_windows(Some(windows))?);
    }
    update.monitor_enabled = req.monitor_enabled;

    let guard = state
        .infra
        .conflict_guard
        .check_and_bump(
            OperationTarget::Blogger,
            &req.id.to_string(),
            req.expected_version,
        )
        .await?;
    if let Err(error) = state.business.blogger_service.apply_update(b, update).await {
        if let Err(rollback_error) = guard.rollback().await {
            tracing::warn!(%rollback_error, "博主版本守卫回滚失败");
        }
        return Err(error.into());
    }
    guard.commit();
    let updated = state
        .business
        .blogger_service
        .find_by_id(req.id)
        .await?
        .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?;
    let now = Local::now();
    Ok(Json(ApiResponse::with_message(
        json!({
            "blogger": helpers::blogger_api_value(&updated, now),
            "server_timestamp": now.timestamp(),
            "server_utc_offset": now.format("%:z").to_string(),
        }),
        "博主配置已更新",
    )))
}

pub(crate) async fn delete_blogger(
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
    let deleted = match state
        .business
        .blogger_service
        .remove_auto_task(req.id)
        .await
    {
        Ok(deleted) => deleted,
        Err(error) => {
            if let Err(rollback_error) = guard.rollback().await {
                tracing::warn!(%rollback_error, "博主版本守卫回滚失败");
            }
            return Err(error.into());
        }
    };
    if deleted.is_none() {
        if let Err(rollback_error) = guard.rollback().await {
            tracing::warn!(%rollback_error, "博主版本守卫回滚失败");
        }
        return Err(AppError::NotFound("未找到该自动任务".to_string()));
    }
    guard.commit();
    Ok(Json(ApiResponse::with_message(json!({}), "自动任务已删除")))
}
