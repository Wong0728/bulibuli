//! 博主管理接口共享的校验、规范化与序列化工具。

use crate::error::AppError;
use chrono::{DateTime, Local};

pub(super) fn validate_intervals(min_interval: i32, max_interval: i32) -> Result<(), AppError> {
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

pub(super) fn validate_series_filter(value: Option<String>) -> Result<Option<String>, AppError> {
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

pub(super) fn map_blogger_insert_error(error: anyhow::Error) -> AppError {
    let message = error.to_string();
    if message.to_ascii_lowercase().contains("unique")
        || message.to_ascii_lowercase().contains("constraint")
    {
        AppError::Conflict("该 UID 已存在，请刷新列表后重试".to_string())
    } else {
        error.into()
    }
}

pub(super) fn normalize_active_windows(
    windows: Option<Vec<String>>,
) -> Result<Option<String>, AppError> {
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

/// 把博主模型序列化为 API 响应，并附带监控调度快照字段。
pub(super) fn blogger_api_value(
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
            serde_json::json!(snapshot.monitor_enabled),
        );
        object.insert(
            "runtime_state".to_string(),
            serde_json::json!(snapshot.runtime_state),
        );
        object.insert(
            "pause_reason".to_string(),
            serde_json::json!(snapshot.pause_reason),
        );
        object.insert(
            "within_active_window".to_string(),
            serde_json::json!(snapshot.within_active_window),
        );
        object.insert(
            "next_action_at".to_string(),
            serde_json::json!(snapshot.next_action_at),
        );
        object.insert(
            "next_action_kind".to_string(),
            serde_json::json!(snapshot.next_action_kind),
        );
    }
    value
}
