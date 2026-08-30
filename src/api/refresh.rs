use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::{extract::Query, extract::State, routing::post, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/refresh", post(refresh))
}

#[derive(Deserialize)]
struct RefreshQuery {
    kind: String,
    bvid: Option<String>,
}

async fn refresh(
    State(state): State<SharedState>,
    Query(query): Query<RefreshQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    info!(kind = %query.kind, "[API] manual refresh");
    let (data, message) = match query.kind.as_str() {
        "board" => {
            let outcome = state.business.refresh_service.trigger_l1().await?;
            let message = match outcome.skipped {
                Some(reason) => reason.to_string(),
                None => format!("已刷新 {} 条视频", outcome.refreshed),
            };
            (
                json!({ "refreshed": outcome.refreshed, "skipped": outcome.skipped }),
                message,
            )
        }
        "blogger" => {
            let outcome = state.business.refresh_service.trigger_l2().await?;
            let message = match outcome.skipped {
                Some(reason) => reason.to_string(),
                None => format!("已刷新 {} 个博主", outcome.refreshed),
            };
            (
                json!({ "refreshed": outcome.refreshed, "skipped": outcome.skipped }),
                message,
            )
        }
        "video" => {
            let bvid = query
                .bvid
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| AppError::BadRequest("请提供 bvid".to_string()))?;
            state.business.refresh_service.trigger_video(bvid).await?;
            (json!({ "bvid": bvid }), "已刷新视频数据".to_string())
        }
        "verify" => {
            let verified = state.bili.verify_service.trigger_once().await?;
            (
                json!({ "verified": verified }),
                format!("已校验 {verified} 条记录"),
            )
        }
        other => return Err(AppError::BadRequest(format!("未知刷新类型: {other}"))),
    };
    Ok(Json(ApiResponse::with_message(data, message)))
}
