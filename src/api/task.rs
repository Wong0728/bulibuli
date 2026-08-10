use crate::error::{ApiResponse, AppError};
use crate::services::blogger::MonitorToggle;
use crate::state::SharedState;
use axum::{extract::State, routing::post, Json, Router};
use chrono::Local;
use serde::Deserialize;
use serde_json::{json, Value};

fn schedule_value(blogger: &crate::models::blogger::Model, now: chrono::DateTime<Local>) -> Value {
    let snapshot = crate::services::monitor::schedule_snapshot(
        blogger.is_running,
        blogger.next_check,
        blogger.active_windows.as_deref(),
        now,
    );
    json!({
        "uid": blogger.uid,
        "next_check": blogger.next_check.map(|time| time.timestamp()).unwrap_or(0),
        "is_running": blogger.is_running,
        "monitor_enabled": snapshot.monitor_enabled,
        "runtime_state": snapshot.runtime_state,
        "pause_reason": snapshot.pause_reason,
        "within_active_window": snapshot.within_active_window,
        "next_action_at": snapshot.next_action_at,
        "next_action_kind": snapshot.next_action_kind,
    })
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/task/start", post(start_task))
        .route("/api/task/stop", post(stop_task))
        .route("/api/task/status", axum::routing::get(get_status))
        .route("/api/task/next-check", axum::routing::get(get_next_check))
}

#[derive(Deserialize)]
struct StartTaskRequest {
    uid: String,
}

async fn start_task(
    State(state): State<SharedState>,
    Json(req): Json<StartTaskRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = req.uid.trim();
    if uid.is_empty() {
        return Err(AppError::BadRequest("请提供博主UID".to_string()));
    }
    match state
        .business
        .blogger_service
        .set_monitor_running(uid, true)
        .await?
    {
        MonitorToggle::NotFound => {
            return Err(AppError::NotFound("未找到该博主，请先添加".to_string()));
        }
        MonitorToggle::AlreadyInState => {
            return Err(AppError::Conflict("该博主监控已在运行中".to_string()));
        }
        MonitorToggle::Updated => {}
    }
    let blogger = state
        .business
        .blogger_service
        .find_by_uid(uid)
        .await?
        .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?;
    let now = Local::now();
    let schedule = crate::services::monitor::schedule_snapshot(
        blogger.is_running,
        blogger.next_check,
        blogger.active_windows.as_deref(),
        now,
    );
    let log_message = if schedule.runtime_state == "waiting_window" {
        format!(
            "监控已启用，当前处于时段外，将于 {} 自动恢复",
            blogger
                .next_check
                .map(|time| time.format("%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "下一监测窗口".to_string())
        )
    } else {
        "监控已启动，准备执行第一次检查...".to_string()
    };
    state
        .business
        .monitor_service
        .add_log(Some(uid), None, &log_message, "success")
        .await;
    Ok(Json(ApiResponse::with_message(
        json!({
            "next_check": blogger.next_check.map(|time| time.timestamp()).unwrap_or(0),
            "schedule": schedule_value(&blogger, now),
            "server_timestamp": now.timestamp(),
            "server_utc_offset": now.format("%:z").to_string(),
        }),
        "监控已启动",
    )))
}

async fn stop_task(
    State(state): State<SharedState>,
    Json(req): Json<StartTaskRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let uid = req.uid.trim();
    if uid.is_empty() {
        return Err(AppError::BadRequest("请提供博主UID".to_string()));
    }
    match state
        .business
        .blogger_service
        .set_monitor_running(uid, false)
        .await?
    {
        MonitorToggle::NotFound => {
            return Err(AppError::NotFound("未找到该博主".to_string()));
        }
        MonitorToggle::AlreadyInState => {
            return Err(AppError::Conflict("该博主监控未在运行".to_string()));
        }
        MonitorToggle::Updated => {}
    }
    state
        .business
        .monitor_service
        .add_log(Some(uid), None, "监控已停止", "info")
        .await;
    Ok(Json(ApiResponse::with_message(json!({}), "监控已停止")))
}

async fn get_status(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bloggers = state.business.blogger_service.list_auto_tasks().await?;
    let now = Local::now();
    let snapshots = bloggers
        .iter()
        .map(|blogger| {
            crate::services::monitor::schedule_snapshot(
                blogger.is_running,
                blogger.next_check,
                blogger.active_windows.as_deref(),
                now,
            )
        })
        .collect::<Vec<_>>();
    let enabled_count = snapshots
        .iter()
        .filter(|snapshot| snapshot.monitor_enabled)
        .count();
    let active_count = snapshots
        .iter()
        .filter(|snapshot| matches!(snapshot.runtime_state, "scheduled" | "checking"))
        .count();
    let waiting_count = snapshots
        .iter()
        .filter(|snapshot| snapshot.runtime_state == "waiting_window")
        .count();
    let next_ts = snapshots
        .iter()
        .filter_map(|snapshot| (snapshot.next_action_at > 0).then_some(snapshot.next_action_at))
        .min()
        .unwrap_or(0);
    Ok(Json(ApiResponse::success(json!({
        "running": enabled_count > 0,
        "server_timestamp": now.timestamp(),
        "server_utc_offset": now.format("%:z").to_string(),
        "next_check_timestamp": next_ts,
        "enabled_tasks": enabled_count,
        "active_tasks": active_count,
        "waiting_window_tasks": waiting_count,
    }))))
}

async fn get_next_check(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bloggers = state.business.blogger_service.list_auto_tasks().await?;
    let now = Local::now();
    let mut result = serde_json::Map::new();
    for b in bloggers {
        result.insert(b.uid.clone(), schedule_value(&b, now));
    }
    Ok(Json(ApiResponse::success(json!({
        "bloggers": result,
        "server_timestamp": now.timestamp(),
        "server_utc_offset": now.format("%:z").to_string(),
    }))))
}
