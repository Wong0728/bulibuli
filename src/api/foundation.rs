//! 主 Web 暴露的只读基础配置摘要。
//! 可写的 Setup API 只由 `setup_server` 提供。

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
    /// AI Skill 文件的绝对路径：发给 AI 前可直接复制使用。
    ai_skill_path: String,
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
        ai_skill_path: state
            .infra
            .paths
            .app_root
            .join("docs")
            .join("skill.md")
            .to_string_lossy()
            .replace('\\', "/"),
        access_mode,
        // Setup 监听器按设计只绑定回环地址；未来的短时签名 Setup URL
        // 可以在不扩大主 API 暴露面的前提下改变这一点。
        setup_access: if state.infra.actual_setup_port.load(Ordering::Relaxed) == 0 {
            "unavailable"
        } else {
            "local_only"
        },
        restart_required: active.mode != configured.mode,
    }))
}
