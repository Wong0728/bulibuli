use crate::error::{ApiResponse, AppError};
use crate::services::blogger::{BloggerUpdate, NewBlogger};
use crate::state::SharedState;
use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Local};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

#[derive(Deserialize)]
pub(super) struct AcknowledgeBatchRequest {
    uids: Vec<String>,
}

pub(super) async fn acknowledge_profile_changes(
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
}

fn validate_intervals(min_interval: i32, max_interval: i32) -> Result<(), AppError> {
    if !(30..=3600).contains(&min_interval) {
        return Err(AppError::BadRequest(
            "最小间隔必须在30-3600秒之间".to_string(),
        ));
    }
    if !(30..=7200).contains(&max_interval) {
        return Err(AppError::BadRequest(
            "最大间隔必须在30-7200秒之间".to_string(),
        ));
    }
    if min_interval > max_interval {
        return Err(AppError::BadRequest("最小间隔不能大于最大间隔".to_string()));
    }
    Ok(())
}

fn validate_series_filter(value: Option<String>) -> Result<Option<String>, AppError> {
    let value = value.map(|item| item.trim().to_string());
    if let Some(trimmed) = value.as_deref().filter(|item| !item.is_empty()) {
        regex::RegexBuilder::new(trimmed)
            .size_limit(10_240)
            .dfa_size_limit(10_240)
            .build()
            .map_err(|error| AppError::BadRequest(format!("正则表达式无效: {error}")))?;
    }
    Ok(value.filter(|item| !item.is_empty()))
}

fn normalize_active_windows(windows: Option<Vec<String>>) -> Result<Option<String>, AppError> {
    let Some(windows) = windows else {
        return Ok(None);
    };
    if windows.len() > 6 {
        return Err(AppError::BadRequest("活跃时段最多设置6条".to_string()));
    }
    let normalized =
        crate::services::monitor::normalize_windows(&windows).map_err(AppError::BadRequest)?;
    if normalized.len() > 6 {
        return Err(AppError::BadRequest(
            "规范化后的活跃时段最多设置6条".to_string(),
        ));
    }
    Ok((!normalized.is_empty()).then(|| serde_json::to_string(&normalized).unwrap_or_default()))
}

fn blogger_api_value(
    blogger: &crate::models::blogger::Model,
    now: DateTime<Local>,
) -> serde_json::Value {
    let mut value = blogger.to_api();
    let snapshot = crate::services::monitor::schedule_snapshot(
        blogger.is_running,
        blogger.next_check,
        blogger.active_windows.as_deref(),
        now,
    );
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "monitor_enabled".to_string(),
            json!(snapshot.monitor_enabled),
        );
        object.insert("runtime_state".to_string(), json!(snapshot.runtime_state));
        object.insert("pause_reason".to_string(), json!(snapshot.pause_reason));
        object.insert(
            "within_active_window".to_string(),
            json!(snapshot.within_active_window),
        );
        object.insert("next_action_at".to_string(), json!(snapshot.next_action_at));
        object.insert(
            "next_action_kind".to_string(),
            json!(snapshot.next_action_kind),
        );
    }
    value
}

pub(super) async fn list_bloggers(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bloggers = state.business.blogger_service.list_auto_tasks().await?;
    let now = Local::now();
    Ok(Json(ApiResponse::success(json!({
        "bloggers": bloggers.iter().map(|b| blogger_api_value(b, now)).collect::<Vec<_>>(),
        "server_timestamp": now.timestamp(),
        "server_utc_offset": now.format("%:z").to_string(),
    }))))
}

pub(super) async fn add_blogger(
    State(state): State<SharedState>,
    Json(req): Json<AddBloggerRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = req.uid.trim().to_string();
    let name = req.name.unwrap_or_default().trim().to_string();
    let min_interval = req.min_interval.unwrap_or(60);
    let max_interval = req.max_interval.unwrap_or(300);

    validate_intervals(min_interval, max_interval)?;
    if uid.is_empty() {
        return Err(AppError::BadRequest("请输入博主UID".to_string()));
    }
    let series_filter_regex = validate_series_filter(req.series_filter_regex)?;
    let active_windows = normalize_active_windows(req.active_windows)?;

    let existing = state.business.blogger_service.find_by_uid(&uid).await?;
    if existing
        .as_ref()
        .is_some_and(|blogger| blogger.has_auto_task)
    {
        return Err(AppError::Conflict("该博主已有自动任务".to_string()));
    }

    // 创建记录前获取当前资料，避免首屏展示空字段。
    // `get_user_info` 成功即表示资料可用；不存在、风控或网络错误时回退到用户传入信息。
    let cookies = state.infra.settings_service.cookie_header().await?;
    let fallback_name = if name.is_empty() {
        None
    } else {
        Some(name.clone())
    };
    let (final_name, face, sign, level, fans) = match uid.parse::<i64>() {
        Ok(uid_i64) => match state.bili.bili_api.get_user_info(uid_i64, &cookies).await {
            Ok(profile) => {
                // 用户填写的是监控备注名，应优先保留；未填写时使用 API 昵称。
                let api_name = Some(profile.name).filter(|s| !s.is_empty());
                let api_face = Some(profile.face).filter(|s| !s.is_empty());
                let api_sign = Some(profile.sign).filter(|s| !s.is_empty());
                (
                    fallback_name.or(api_name),
                    api_face,
                    api_sign,
                    Some(profile.level as i32),
                    Some(profile.fans),
                )
            }
            Err(error) => {
                warn!(uid, %error, "添加博主时拉取资料失败，使用兜底信息");
                (fallback_name, None, None, None, None)
            }
        },
        Err(_) => (fallback_name, None, None, None, None),
    };

    let monitor_enabled = req.start_monitoring.unwrap_or(false);
    let blogger = if let Some(existing) = existing {
        let id = existing.id;
        state
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
            .await?;
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
            .await?
    };
    info!("添加博主成功: {uid}");

    let now = Local::now();
    Ok(Json(ApiResponse::with_message(
        json!({
            "blogger_id": blogger.id,
            "blogger": blogger_api_value(&blogger, now),
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
}

pub(super) async fn update_blogger(
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
    validate_intervals(min, max)?;

    // 下载/烧录策略字段
    update.download_video = req.download_video;
    update.download_danmaku = req.download_danmaku;
    update.download_comments = req.download_comments;
    update.download_cover = req.download_cover;
    update.burn_danmaku = req.burn_danmaku;
    update.burn_subtitle = req.burn_subtitle;
    if req.series_filter_regex.is_some() {
        update.series_filter_regex =
            Some(validate_series_filter(req.series_filter_regex)?.unwrap_or_default());
    }
    if let Some(windows) = req.active_windows {
        update.active_windows = Some(normalize_active_windows(Some(windows))?);
    }
    update.monitor_enabled = req.monitor_enabled;

    state
        .business
        .blogger_service
        .apply_update(b, update)
        .await?;
    let updated = state
        .business
        .blogger_service
        .find_by_id(req.id)
        .await?
        .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?;
    let now = Local::now();
    Ok(Json(ApiResponse::with_message(
        json!({
            "blogger": blogger_api_value(&updated, now),
            "server_timestamp": now.timestamp(),
            "server_utc_offset": now.format("%:z").to_string(),
        }),
        "博主配置已更新",
    )))
}

#[derive(Deserialize)]
pub(super) struct DeleteBloggerRequest {
    id: i32,
}

pub(super) async fn delete_blogger(
    State(state): State<SharedState>,
    Json(req): Json<DeleteBloggerRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let deleted = state
        .business
        .blogger_service
        .remove_auto_task(req.id)
        .await?;
    if deleted.is_none() {
        return Err(AppError::NotFound("未找到该自动任务".to_string()));
    }
    Ok(Json(ApiResponse::with_message(json!({}), "自动任务已删除")))
}

#[derive(Deserialize)]
pub(super) struct AddSavedBloggerRequest {
    uid: String,
    name: Option<String>,
    face: Option<String>,
    sign: Option<String>,
    level: Option<i32>,
    fans: Option<i64>,
}

/// 搜索页"已添加博主"列表；不包含仅存在于自动任务中的博主。
pub(super) async fn list_saved_bloggers(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bloggers = state.business.blogger_service.list_saved().await?;
    Ok(Json(ApiResponse::success(json!({
        "bloggers": bloggers.iter().map(crate::models::blogger::Model::to_api).collect::<Vec<_>>(),
    }))))
}

/// 收藏一个搜索结果。若同 UID 已有自动任务，只增加收藏身份，不改动任务配置。
pub(super) async fn add_saved_blogger(
    State(state): State<SharedState>,
    Json(req): Json<AddSavedBloggerRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = req.uid.trim().to_string();
    if uid.is_empty() || !uid.chars().all(|character| character.is_ascii_digit()) {
        return Err(AppError::BadRequest("请输入有效的数字 UID".to_string()));
    }
    if let Some(existing) = state.business.blogger_service.find_by_uid(&uid).await? {
        if existing.is_saved {
            return Err(AppError::Conflict("该博主已在添加列表中".to_string()));
        }
        let id = existing.id;
        state
            .business
            .blogger_service
            .apply_update(
                existing,
                BloggerUpdate {
                    is_saved: Some(true),
                    ..Default::default()
                },
            )
            .await?;
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
        .await?;
    Ok(Json(ApiResponse::with_message(
        json!({ "blogger": blogger.to_api() }),
        "博主已添加",
    )))
}

pub(super) async fn delete_saved_blogger(
    State(state): State<SharedState>,
    Json(req): Json<DeleteBloggerRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if state
        .business
        .blogger_service
        .remove_saved(req.id)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound("未找到已添加博主".to_string()));
    }
    Ok(Json(ApiResponse::with_message(json!({}), "博主已移除")))
}
