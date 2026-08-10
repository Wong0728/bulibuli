use crate::state::SharedState;
use axum::{extract::State, routing::get, Json, Router};
use serde_json::json;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/health", get(health_check))
        .route("/api/ready", get(readiness_check))
}

async fn health_check() -> Json<serde_json::Value> {
    Json(json!({"status": "ok"}))
}

async fn readiness_check(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let db_healthy = state
        .infra
        .db
        .ping()
        .await
        .inspect_err(|e| tracing::warn!("ready check db ping failed: {e}"))
        .is_ok();
    let aria2_ready = state.media.aria2.is_available().await;
    Json(json!({
        "status": if db_healthy && aria2_ready { "ok" } else { "degraded" },
        "db": db_healthy,
        "aria2": aria2_ready,
    }))
}
