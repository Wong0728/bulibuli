//! Setup 向导 API：供 Web Setup 页面调用，完成首次配置。
//!
//! - `GET /api/setup/status` → 返回 onboarding 完成状态、当前模式、检测到的 IP
//! - `POST /api/setup/apply` → 应用配置（模式、访问规则、AI Skill 等）
//! - `GET /api/setup/detect` → 检测本机所有网络接口地址

use crate::error::{ApiResponse, AppError};
use crate::services::security_config::{AccessAction, AccessMode, SecurityConfigService};
use crate::state::SharedState;
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::Ordering;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/setup/status", get(get_status))
        .route("/api/setup/apply", post(apply_config))
        .route("/api/setup/finish", post(finish_setup))
        .route("/api/setup/ai-skill", post(update_ai_skill))
        .route("/api/setup/detect", get(detect_network))
        .route("/api/setup/ports", get(get_ports))
}

#[derive(Serialize)]
struct SetupStatus {
    completed: bool,
    /// 当前进程实际监听所使用的模式。
    mode: String,
    /// security.toml 中已保存、下次启动会使用的模式。
    configured_mode: String,
    restart_required: bool,
    ai_skill_enabled: bool,
    /// AI Skill 文件绝对路径（复制给 AI 直接可用）。
    ai_skill_path: String,
    detected_ips: Vec<String>,
    main_port: u16,
    setup_port: u16,
    main_url: Option<String>,
    accessible_urls: Vec<String>,
}

async fn get_status(state: axum::extract::State<SharedState>) -> Json<ApiResponse<SetupStatus>> {
    let startup = crate::app::onboarding::StartupState::load(&state.infra.paths.data_dir);
    let active_security = state.bili.security.current();
    let configured_security =
        SecurityConfigService::load(&state.infra.paths.data_dir, &state.infra.paths.app_root)
            .map(|service| service.current())
            .unwrap_or_else(|_| active_security.clone());
    let ips = detect_local_ips();
    let endpoints = endpoint_info(&state, &ips);
    Json(ApiResponse {
        code: 0,
        message: "ok".to_string(),
        data: SetupStatus {
            completed: startup.onboarding_completed,
            mode: mode_name(&active_security.mode).to_string(),
            configured_mode: mode_name(&configured_security.mode).to_string(),
            restart_required: active_security.mode != configured_security.mode,
            ai_skill_enabled: startup.ai_skill_enabled,
            ai_skill_path: state
                .infra
                .paths
                .app_root
                .join("docs")
                .join("skill.md")
                .to_string_lossy()
                .replace('\\', "/"),
            detected_ips: ips,
            main_port: endpoints.main_port,
            setup_port: endpoints.setup_port,
            main_url: endpoints.main_url,
            accessible_urls: endpoints.accessible_urls,
        },
    })
}

fn mode_name(mode: &AccessMode) -> &'static str {
    match mode {
        AccessMode::Local => "local",
        AccessMode::Lan => "lan",
        AccessMode::Proxy => "proxy",
    }
}

#[derive(Deserialize)]
struct ApplyRequest {
    mode: String,
    #[serde(default)]
    access_default: Option<String>,
    #[serde(default)]
    proxy_domain: Option<String>,
    #[serde(default)]
    mark_completed: Option<bool>,
}

async fn apply_config(
    state: axum::extract::State<SharedState>,
    Json(req): Json<ApplyRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    // 解析模式
    let mode = match req.mode.as_str() {
        "local" => AccessMode::Local,
        "lan" => AccessMode::Lan,
        "proxy" => AccessMode::Proxy,
        _ => {
            return Err(AppError::BadRequest(
                "无效的模式，可选: local, lan, proxy".to_string(),
            ));
        }
    };

    // 写入 security.toml
    let service =
        match SecurityConfigService::load(&state.infra.paths.data_dir, &state.infra.paths.app_root)
        {
            Ok(s) => s,
            Err(error) => return Err(error),
        };

    let is_lan = mode == AccessMode::Lan;
    let active_mode = state.bili.security.current().mode;
    let restart_required = active_mode != mode;

    service
        .update(|config| {
            config.mode = mode;
            config.proxy_domain = req.proxy_domain.clone();
            if let Some(ref default_action) = req.access_default {
                config.access_default = match default_action.as_str() {
                    "allow" => AccessAction::Allow,
                    _ => AccessAction::Deny,
                };
            } else if is_lan {
                // LAN 模式未显式指定策略时默认允许，否则选了局域网也无法访问
                config.access_default = AccessAction::Allow;
            }
            Ok(())
        })
        .await?;
    if !restart_required {
        state.bili.security.replace_current(service.current());
    }

    // 标记 onboarding 完成；Setup 端口交接由 /api/setup/finish 明确确认，
    // 不能在 apply 响应返回前关闭当前页面所在的服务。
    if req.mark_completed.unwrap_or(false) {
        if let Err(error) =
            crate::app::onboarding::StartupState::mark_completed(&state.infra.paths.data_dir)
        {
            tracing::warn!(%error, "标记 onboarding 完成失败");
        }
    }

    let endpoints = endpoint_info(&state, &detect_local_ips());
    Ok(Json(ApiResponse {
        code: 0,
        message: if restart_required {
            "配置已保存，重启应用后生效".to_string()
        } else {
            "配置已保存".to_string()
        },
        data: json!({
            "mode": req.mode,
            "restart_required": restart_required,
            "main_port": endpoints.main_port,
            "setup_port": endpoints.setup_port,
            "main_url": endpoints.main_url,
            "setup_url": endpoints.setup_url,
            "accessible_urls": endpoints.accessible_urls,
        }),
    }))
}

/// 前端已消费 apply 响应并准备跳转到主端口后，确认可以关闭一次性 Setup 服务。
async fn finish_setup(
    state: axum::extract::State<SharedState>,
) -> Json<ApiResponse<serde_json::Value>> {
    let endpoints = endpoint_info(&state, &[]);
    Json(ApiResponse::with_message(
        json!({
            "main_url": endpoints.main_url,
            "accessible_urls": endpoints.accessible_urls,
        }),
        "Setup 交接已确认",
    ))
}

#[derive(Deserialize)]
struct AiSkillRequest {
    enabled: bool,
}

/// 仅更新 AI Skill 开关，不触碰网络安全模式。
async fn update_ai_skill(
    state: axum::extract::State<SharedState>,
    Json(req): Json<AiSkillRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, AppError> {
    if let Err(error) =
        crate::app::onboarding::StartupState::save_ai_flag(&state.infra.paths.data_dir, req.enabled)
    {
        return Err(AppError::Internal(format!(
            "保存 AI Skill 设置失败: {error}"
        )));
    }
    state
        .infra
        .ai_skill_enabled
        .store(req.enabled, Ordering::Relaxed);
    Ok(Json(ApiResponse {
        code: 0,
        message: "AI Skill 设置已保存".to_string(),
        data: json!({ "ai_skill_enabled": req.enabled }),
    }))
}

#[derive(Serialize)]
struct DetectResult {
    ipv4: Vec<String>,
    ipv6: Vec<String>,
}

async fn detect_network() -> Json<ApiResponse<DetectResult>> {
    let ips = detect_local_ips();
    let mut ipv4 = Vec::new();
    let mut ipv6 = Vec::new();
    for ip in ips {
        if ip.contains(':') {
            ipv6.push(ip);
        } else {
            ipv4.push(ip);
        }
    }
    Json(ApiResponse {
        code: 0,
        message: "ok".to_string(),
        data: DetectResult { ipv4, ipv6 },
    })
}

/// 检测本机所有 IP 地址。
/// 使用简单的方式：通过 UDP connect 获取本机出口 IP，加上已知的回环地址。
fn detect_local_ips() -> Vec<String> {
    let mut ips = vec!["127.0.0.1".to_string()];

    // 通过 UDP connect 检测本机出口 IPv4。
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip().to_string();
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
        }
    }

    // 尝试检测 IPv6 出口
    if let Ok(socket) = std::net::UdpSocket::bind("[::]:0") {
        if socket.connect("[2001:4860:4860::8888]:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = format!("[{}]", addr.ip());
                if !ips.contains(&ip) {
                    ips.push(ip);
                }
            }
        }
    }

    ips
}

/// 返回实际绑定的主端口和 Setup 端口，供前端跳转使用。
async fn get_ports(state: axum::extract::State<SharedState>) -> Json<ApiResponse<PortsResult>> {
    let endpoints = endpoint_info(&state, &detect_local_ips());
    Json(ApiResponse {
        code: 0,
        message: "ok".to_string(),
        data: PortsResult {
            main_port: endpoints.main_port,
            setup_port: endpoints.setup_port,
            main_url: endpoints.main_url,
            setup_url: endpoints.setup_url,
            accessible_urls: endpoints.accessible_urls,
        },
    })
}

#[derive(Serialize)]
struct PortsResult {
    main_port: u16,
    setup_port: u16,
    main_url: Option<String>,
    setup_url: Option<String>,
    accessible_urls: Vec<String>,
}

struct EndpointInfo {
    main_port: u16,
    setup_port: u16,
    main_url: Option<String>,
    setup_url: Option<String>,
    accessible_urls: Vec<String>,
}

fn endpoint_info(state: &SharedState, _ips: &[String]) -> EndpointInfo {
    let main_port = state.infra.actual_main_port.load(Ordering::Relaxed);
    let setup_port = state.infra.actual_setup_port.load(Ordering::Relaxed);
    let accessible_urls = crate::app::server::accessible_main_urls(state);
    let main_url = (main_port != 0).then(|| crate::app::server::main_url(state));
    let setup_url = (setup_port != 0).then(|| format!("http://127.0.0.1:{setup_port}"));
    EndpointInfo {
        main_port,
        setup_port,
        main_url,
        setup_url,
        accessible_urls,
    }
}
