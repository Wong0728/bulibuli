//! Read-only foundation-configuration summary exposed to the main Web.
//! The writable Setup API is deliberately served only by `setup_server`.

use crate::error::ApiResponse;
use crate::services::security_config::AccessMode;
use crate::state::SharedState;
use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;
use std::sync::atomic::Ordering;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/foundation/status", get(status))
}

#[derive(Serialize)]
struct FoundationStatus {
    configuration_status: &'static str,
    ai_skill_enabled: bool,
    access_mode: &'static str,
    setup_access: &'static str,
    restart_required: bool,
}

async fn status(State(state): State<SharedState>) -> Json<ApiResponse<FoundationStatus>> {
    let active = state.bili.security.current();
    let configured = crate::services::security_config::SecurityConfigService::load(
        &state.infra.paths.data_dir,
        &state.infra.paths.app_root,
    )
    .map(|service| service.current())
    .unwrap_or_else(|_| active.clone());
    let access_mode = match active.mode {
        AccessMode::Local => "local",
        AccessMode::Lan => "lan",
        AccessMode::Proxy => "proxy",
    };
    Json(ApiResponse::success(FoundationStatus {
        configuration_status: "normal",
        ai_skill_enabled: state.infra.ai_skill_enabled.load(Ordering::Relaxed),
        access_mode,
        // The Setup listener binds to loopback by design.  A future signed
        // short-lived setup URL can change this without widening the main API.
        setup_access: if state.infra.actual_setup_port.load(Ordering::Relaxed) == 0 {
            "unavailable"
        } else {
            "local_only"
        },
        restart_required: active.mode != configured.mode,
    }))
}
