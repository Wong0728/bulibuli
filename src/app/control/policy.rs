//! 主题 1：策略命令（`ai` / `access` / `mode` / `geo` / `trust`）与旧名 alias 命令
//! （`pair` / `quit` / `revoke` / `status`）。
//!
//! 这些命令改写安全配置或执行敏感操作，统一经由 `audit_only_op` /
//! `with_guarded_op` 记审计；AI 模式门控由 `execute_from` 入口统一检查。

use crate::app::control::helpers::{
    audit_only_op, ctl_audit_ctx, error_code_for, parse_action, parse_minutes, parse_network,
    required, with_guarded_op,
};
use crate::app::onboarding::StartupState;
use crate::error::{AppError, AppResult};
use crate::models::operation_log::{OperationOutcome, OperationTarget};
use crate::services::security_config::{AccessAction, AccessMode};
use crate::state::SharedState;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

/// `pair [close]`：开启/关闭配对模式。
/// 开启配对会生成配对码，这是敏感操作（一次性访问凭证）；审计走 record_silent，不广播事件。
/// AI 模式开启后 AI 与人工拥有相同权限，配对始终允许（不再需要短时授权层）。
pub(super) async fn pair_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    // 只接受缺省（开启）或 close：未知参数报错，避免 `pair xxx` 被静默当作开配对。
    if let Some(unknown) = args.get(1).filter(|value| value.as_str() != "close") {
        return Err(AppError::BadRequest(format!(
            "pair 不支持参数 `{unknown}`；用法：pair [close]"
        )));
    }
    let is_close = args.get(1).is_some_and(|value| value == "close");
    let ctx = ctl_audit_ctx(
        &args.join(" "),
        OperationTarget::Session,
        None,
        if is_close { "pair_close" } else { "pair_open" },
        None,
    );
    let auth = state.bili.auth.clone();
    let result: AppResult<Value> = if is_close {
        auth.close_pairing().await;
        Ok(json!({"pairing_open": false}))
    } else {
        let code = auth.open_pairing().await;
        Ok(json!({"pairing_code": format!("{}-{}", &code[..4], &code[4..])}))
    };
    match result {
        Ok(value) => {
            state
                .infra
                .audit_log
                .record_silent(&ctx, OperationOutcome::Success, None, None, None)
                .await;
            Ok(value)
        }
        Err(error) => {
            let code = error_code_for(&error);
            state
                .infra
                .audit_log
                .record_silent(
                    &ctx,
                    OperationOutcome::Error,
                    None,
                    Some(code),
                    Some(json!({"error": error.to_string()})),
                )
                .await;
            Err(error)
        }
    }
}

/// `quit`：优雅关停。审计后取消 cancellation token。
pub(super) async fn quit_command(state: &SharedState) -> AppResult<Value> {
    let ctx = ctl_audit_ctx("quit", OperationTarget::Session, None, "quit", None);
    state
        .infra
        .audit_log
        .record(&ctx, OperationOutcome::Success, None, None, None)
        .await;
    state.infra.cancellation.cancel();
    Ok(json!({"shutting_down": true}))
}

pub(super) async fn ai_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(value) = args.get(1).map(String::as_str) else {
        return Ok(json!({
            "ai_skill_enabled": state.infra.ai_skill_enabled.load(Ordering::Relaxed),
        }));
    };
    let enabled = match value {
        "on" => true,
        "off" => false,
        _ => return Err(AppError::BadRequest("用法：ai on|off".to_string())),
    };
    audit_only_op(
        state,
        &format!("ai {value}"),
        OperationTarget::Settings,
        None,
        "ai_toggle",
        || async move {
            // 1. 更新内存门控标志（ctl 命令门控以此为准）
            state
                .infra
                .ai_skill_enabled
                .store(enabled, Ordering::Relaxed);
            // 2. 持久化到 startup_state.json（下次启动沿用）
            StartupState::save_ai_flag(&state.infra.paths.data_dir, enabled)
                .map_err(|error| AppError::Internal(format!("写入 AI 开关失败: {error}")))?;
            Ok(json!({
                "ai_skill_enabled": enabled,
                "note": if enabled {
                    "AI Skill 模式已启用，AI 助手可执行与人工相同的全部 ctl 命令（含 mode/access/geo/trust/pair）"
                } else {
                    "AI Skill 模式已关闭，ctl 放行 status/help/quit/ai/pair/sys status"
                },
            }))
        },
    )
    .await
}

pub(super) async fn legacy_status(state: &SharedState) -> Value {
    json!({
        "mode": state.bili.security.current().mode,
        "pairing": state.bili.auth.pairing_state().await,
        "sessions": state.bili.auth.list_sessions().await.map(|s| s.len()).unwrap_or(0),
        "ai_skill_enabled": state.infra.ai_skill_enabled.load(Ordering::Relaxed),
    })
}

pub(super) async fn revoke_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let id = required(args, 1, "revoke 需要 session-id 或 all")?;
    let ctx = ctl_audit_ctx(
        &format!("revoke {id}"),
        OperationTarget::Session,
        Some(id.to_string()),
        "revoke",
        None,
    );
    let auth = state.bili.auth.clone();
    let ws = state.infra.ws.clone();
    let id_owned = id.to_string();
    let revoked = with_guarded_op(
        state,
        ctx,
        OperationTarget::Session,
        Some(id),
        None,
        move |_guard| async move {
            let revoked = auth.revoke(&id_owned).await?;
            ws.disconnect_session(&id_owned)
                .await
                .map_err(|error| AppError::Internal(error.to_string()))?;
            Ok::<_, AppError>(json!({"revoked": revoked}))
        },
    )
    .await?;
    Ok(revoked)
}

pub(super) async fn access_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    // list 仅查询，不写入审计日志
    if args.get(1).map(String::as_str) == Some("list") {
        return Ok(serde_json::to_value(
            state.bili.security.current().access_rules,
        )?);
    }
    let cmd_str = args.join(" ");
    let args_owned: Vec<String> = args.to_vec();
    audit_only_op(
        state,
        &cmd_str,
        OperationTarget::Settings,
        None,
        "access",
        || async move {
            match args_owned.get(1).map(String::as_str) {
                Some("default") => {
                    let action = parse_action(required(&args_owned, 2, "缺少 allow/deny")?)?;
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.access_default = action;
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"default": action}))
                }
                Some("allow" | "deny") => {
                    let action = parse_action(args_owned[1].as_str())?;
                    let network = parse_network(required(&args_owned, 2, "缺少 IP 或 CIDR")?)?;
                    let minutes = parse_minutes(&args_owned)?;
                    let id = state
                        .bili
                        .security
                        .add_rule(action, network, minutes)
                        .await?;
                    Ok(json!({"rule_id": id}))
                }
                Some("remove") => {
                    let id = required(&args_owned, 2, "缺少规则 ID")?;
                    Ok(json!({"removed": state.bili.security.remove_rule(id).await?}))
                }
                _ => Err(AppError::BadRequest(
                    "用法：access default|allow|deny|remove|list".to_string(),
                )),
            }
        },
    )
    .await
}

pub(super) async fn mode_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(mode) = args.get(1).map(String::as_str) else {
        let active_mode = state.bili.security.current().mode;
        let configured_mode = state.bili.security.configured().mode;
        return Ok(json!({
            "mode": active_mode,
            "active_mode": active_mode,
            "configured_mode": configured_mode,
            "restart_required": active_mode != configured_mode,
        }));
    };
    let cmd_str = args.join(" ");
    let args_owned: Vec<String> = args.to_vec();
    let mode_owned = mode.to_string();
    audit_only_op(
        state,
        &cmd_str,
        OperationTarget::Settings,
        None,
        "mode",
        || async move {
            state
                .bili
                .security
                .update(|config| {
                    match mode_owned.as_str() {
                        "local" => {
                            config.mode = AccessMode::Local;
                            config.proxy_domain = None;
                        }
                        "lan" => {
                            config.mode = AccessMode::Lan;
                            config.proxy_domain = None;
                        }
                        "proxy" => {
                            let domain =
                                required(&args_owned, 2, "proxy 模式缺少域名")?.to_ascii_lowercase();
                            config.mode = AccessMode::Proxy;
                            config.proxy_domain = Some(domain);
                            config.geo_cn = true;
                        }
                        _ => {
                            return Err(AppError::BadRequest(
                                "模式必须是 local/lan/proxy".to_string(),
                            ))
                        }
                    }
                    Ok(())
                })
                .await?;
            // lan 模式下默认放行 + 无任何规则时，局域网内任意 IP 均可达（HTTP 明文），
            // 返回警告提醒先配置允许网段或将默认策略改为 deny。
            let current = state.bili.security.current();
            let configured = state.bili.security.configured();
            if mode_owned == "lan"
                && configured.access_default == AccessAction::Allow
                && configured.access_rules.is_empty()
            {
                return Ok(json!({
                    "mode": mode_owned,
                    "active_mode": current.mode,
                    "configured_mode": configured.mode,
                    "restart_required": current.mode != configured.mode,
                    "warning": "lan 模式当前为默认放行且无任何访问规则，建议先执行：access default deny 并 access allow <可信网段>",
                }));
            }
            Ok(json!({
                "mode": mode_owned,
                "active_mode": current.mode,
                "configured_mode": configured.mode,
                "restart_required": current.mode != configured.mode,
            }))
        },
    )
    .await
}

pub(super) async fn geo_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let cmd_str = args.join(" ");
    let args_owned: Vec<String> = args.to_vec();
    audit_only_op(
        state,
        &cmd_str,
        OperationTarget::Settings,
        None,
        "geo",
        || async move {
            match (
                args_owned.get(1).map(String::as_str),
                args_owned.get(2).map(String::as_str),
            ) {
                (Some("cn"), Some(value @ ("on" | "off"))) => {
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.geo_cn = value == "on";
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"geo_cn": value == "on"}))
                }
                (Some("db"), Some("remove")) => {
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.geo_db = None;
                            Ok(())
                        })
                        .await?;
                    Ok(json!({
                        "geo_db": null,
                        "effective_geo_db": state.bili.security.effective_geo_db(),
                    }))
                }
                (Some("db"), Some(path)) => {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() || !path.is_file() {
                        return Err(AppError::BadRequest(
                            "GeoIP 数据库必须是存在的绝对文件路径".to_string(),
                        ));
                    }
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.geo_db = Some(path.clone());
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"geo_db_updated": true, "effective_geo_db": path}))
                }
                _ => Err(AppError::BadRequest(
                    "用法：geo cn on|off 或 geo db <absolute-path>|remove".to_string(),
                )),
            }
        },
    )
    .await
}

pub(super) async fn trust_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let cmd_str = args.join(" ");
    let args_owned: Vec<String> = args.to_vec();
    audit_only_op(
        state,
        &cmd_str,
        OperationTarget::Settings,
        None,
        "trust",
        || async move {
            match (
                args_owned.get(1).map(String::as_str),
                args_owned.get(2).map(String::as_str),
            ) {
                (Some("aria2"), Some("remove")) => {
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.trusted_aria2_endpoint = None;
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"aria2_trust": null}))
                }
                (Some("aria2"), Some(endpoint)) => {
                    let parsed = url::Url::parse(endpoint).map_err(|_| {
                        AppError::BadRequest("aria2 endpoint 必须是 URL".to_string())
                    })?;
                    if !matches!(parsed.scheme(), "http" | "https") {
                        return Err(AppError::BadRequest(
                            "aria2 endpoint 仅支持 HTTP(S)".to_string(),
                        ));
                    }
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.trusted_aria2_endpoint = Some(endpoint.to_string());
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"aria2_trust": endpoint}))
                }
                (Some("ffmpeg"), Some("remove")) => {
                    state
                        .bili
                        .security
                        .update(|config| {
                            config.trusted_ffmpeg_paths.clear();
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"ffmpeg_trust": []}))
                }
                (Some("ffmpeg"), Some(path)) => {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() || !path.is_file() {
                        return Err(AppError::BadRequest(
                            "FFmpeg 必须是存在的绝对文件路径".to_string(),
                        ));
                    }
                    state
                        .bili
                        .security
                        .update(|config| {
                            if !config.trusted_ffmpeg_paths.contains(&path) {
                                config.trusted_ffmpeg_paths.push(path);
                            }
                            Ok(())
                        })
                        .await?;
                    Ok(json!({"ffmpeg_trust_updated": true}))
                }
                _ => Err(AppError::BadRequest(
                    "用法：trust aria2|ffmpeg <value|remove>".to_string(),
                )),
            }
        },
    )
    .await
}
