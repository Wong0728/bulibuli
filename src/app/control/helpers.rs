//! ctl 命令公共辅助：参数解析、审计上下文、乐观锁包装与通用小工具。
//!
//! 各主题命令模块（policy/download/blogger/...）与本模块单向依赖，
//! 本模块不感知具体命令，保证行为与原单文件实现完全一致。

use crate::error::{AppError, AppResult};
use crate::models::operation_log::{OperationOutcome, OperationSource, OperationTarget};
use crate::services::audit_log::AuditContext;
use crate::services::conflict_guard::ConflictGuard;
use crate::services::credential::Credential;
use crate::services::download::TaskOutcome;
use crate::services::security_config::AccessAction;
use crate::state::SharedState;
use ipnet::IpNet;
use serde_json::{json, Value};
use std::net::IpAddr;

/// 从参数中提取 `--expected-version N`，返回 (剩余参数, expected_version)。
/// 调用方据此决定是否启用乐观锁。
/// flag 存在但值缺失/非法时报 BAD_REQUEST 而非静默忽略——否则拼错的乐观锁
/// 参数会让写操作悄悄退化成"最后写入胜出"，破坏调用方的并发假设。
pub(super) fn extract_expected_version(args: &[String]) -> AppResult<(Vec<String>, Option<i32>)> {
    let Some(pos) = args.iter().position(|a| a == "--expected-version") else {
        return Ok((args.to_vec(), None));
    };
    let mut remaining: Vec<String> = Vec::with_capacity(args.len().saturating_sub(2));
    remaining.extend_from_slice(&args[..pos]);
    let value = args.get(pos + 1).ok_or_else(|| {
        AppError::BadRequest("--expected-version 缺少数值（如 --expected-version 42）".to_string())
    })?;
    let parsed = value.parse::<i32>().map_err(|_| {
        AppError::BadRequest(format!("--expected-version 非法值 `{value}`，需要整数"))
    })?;
    remaining.extend_from_slice(&args[pos + 2..]);
    Ok((remaining, Some(parsed)))
}

/// 解析 `--source <src>` flag 为 OperationSource。非法值报 BAD_REQUEST 而非静默忽略，
/// 避免拼错参数后静默得到全量审计结果。
pub(super) fn parse_source_flag(args: &[String]) -> AppResult<Option<OperationSource>> {
    let Some(val) = parse_flag_value(args, "--source") else {
        return Ok(None);
    };
    val.parse::<OperationSource>()
        .map(Some)
        .map_err(|_| AppError::BadRequest(format!("--source 非法值 `{val}`")))
}

/// 解析 `--since <duration>`：仅接受 Nd/Nh/Nm（如 1h / 24h / 7d）。非法值报 BAD_REQUEST，
/// 与 --source 一致，避免静默忽略过滤条件。
pub(super) fn parse_since_flag(args: &[String]) -> AppResult<Option<String>> {
    let Some(val) = parse_flag_value(args, "--since") else {
        return Ok(None);
    };
    validate_since_duration(val)?;
    Ok(Some(val.to_string()))
}

pub(super) fn validate_since_duration(value: &str) -> AppResult<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    // strip_suffix 按字符边界切分：多字节输入（如 `1小时`）不会像 split_at 那样 panic。
    let valid = ["h", "d", "m"].iter().any(|unit| {
        trimmed
            .strip_suffix(unit)
            .is_some_and(|num| num.parse::<i64>().is_ok())
    });
    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "--since 非法值 `{value}`，支持格式如 1h / 24h / 7d"
        )))
    }
}

/// 解析 `--target-type` 字符串为 OperationTarget。
pub(super) fn parse_target_type(s: &str) -> AppResult<OperationTarget> {
    match s {
        "task" => Ok(OperationTarget::Task),
        "blogger" => Ok(OperationTarget::Blogger),
        "history" => Ok(OperationTarget::History),
        "settings" => Ok(OperationTarget::Settings),
        "cookie" => Ok(OperationTarget::Cookie),
        "session" => Ok(OperationTarget::Session),
        other => Err(AppError::BadRequest(format!(
            "target_type 必须是 task|blogger|history|settings|cookie|session，收到 `{other}`"
        ))),
    }
}

/// 取 `--flag value` 的 value 部分。
fn parse_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let pos = args.iter().position(|a| a == flag)?;
    args.get(pos + 1).map(String::as_str)
}

/// 取 `--limit N`（默认值由调用方给）。值缺失或非正整数时报 BAD_REQUEST，
/// 不静默回退默认值——拼错 limit 会悄悄拿到全量/空结果，掩盖调用方错误。
pub(super) fn parse_limit_flag(args: &[String], default: u64) -> AppResult<u64> {
    let Some(value) = parse_flag_value(args, "--limit") else {
        return Ok(default);
    };
    let parsed = value
        .parse::<u64>()
        .map_err(|_| AppError::BadRequest(format!("--limit 非法值 `{value}`，需要正整数")))?;
    if parsed == 0 {
        return Err(AppError::BadRequest("--limit 必须大于 0".to_string()));
    }
    Ok(parsed)
}

/// 为本机 ctl 调用构造审计上下文（复用 AuditContext::for_ctl，简化调用）。
/// request_id 自动生成 UUID，便于跨服务追踪同一次调用。
pub(super) fn ctl_audit_ctx(
    command: &str,
    target: OperationTarget,
    target_id: Option<String>,
    action: &str,
    expected_version: Option<i32>,
) -> AuditContext {
    let request_id = uuid::Uuid::new_v4().to_string();
    AuditContext::for_ctl(
        command,
        target,
        target_id,
        action,
        expected_version,
        &request_id,
    )
}

/// 写操作失败时统一记审计 + 回滚 guard（如果持有）。
/// 错误码：CONFLICT / AI_SKILL_DISABLED / BILI_NOT_LOGGED_IN / INTERNAL / BAD_REQUEST。
pub(super) fn error_code_for(error: &AppError) -> &'static str {
    match error {
        AppError::Conflict(_) => "CONFLICT",
        AppError::Unauthorized(_) => "UNAUTHORIZED",
        AppError::AiSkillDisabled(_) => "AI_SKILL_DISABLED",
        AppError::BiliNotLoggedIn(_) => "BILI_NOT_LOGGED_IN",
        AppError::NotFound(_) => "NOT_FOUND",
        AppError::BadRequest(_) => "BAD_REQUEST",
        AppError::RiskControl(_) => "RISK_CONTROL",
        _ => "INTERNAL",
    }
}

/// 把 ConflictGuard 包到写操作里：成功 commit + 审计，失败 rollback + 审计。
/// `op` 收到 `Option<&ConflictGuard>`（None=未启用乐观锁），返回操作结果。
pub(super) async fn with_guarded_op<T, F, Fut>(
    state: &SharedState,
    ctx: AuditContext,
    target: OperationTarget,
    target_id: Option<&str>,
    expected_version: Option<i32>,
    op: F,
) -> AppResult<T>
where
    F: FnOnce(Option<&ConflictGuard>) -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    // 1. 乐观锁校验（仅在 expected_version 和 target_id 都存在时启用）
    let guard: Option<ConflictGuard> = if let (Some(ev), Some(id)) = (expected_version, target_id) {
        match state
            .infra
            .conflict_guard
            .check_and_bump(target, id, Some(ev))
            .await
        {
            Ok(g) => Some(g),
            Err(error) => {
                // 冲突：审计 + 返回错误
                state
                    .infra
                    .audit_log
                    .record(
                        &ctx,
                        OperationOutcome::Conflict,
                        None,
                        Some(error_code_for(&error)),
                        None,
                    )
                    .await;
                return Err(error);
            }
        }
    } else {
        None
    };

    // 2. 执行操作
    let result = op(guard.as_ref()).await;

    match result {
        Ok(value) => {
            // 成功：commit guard + 审计
            if let Some(g) = guard {
                let new_v = g.new_version();
                g.commit();
                state
                    .infra
                    .audit_log
                    .record(&ctx, OperationOutcome::Success, Some(new_v), None, None)
                    .await;
            } else {
                state
                    .infra
                    .audit_log
                    .record(&ctx, OperationOutcome::Success, None, None, None)
                    .await;
            }
            Ok(value)
        }
        Err(error) => {
            let code = error_code_for(&error);
            // 失败：rollback guard + 审计
            if let Some(g) = guard {
                g.rollback().await.ok();
            }
            state
                .infra
                .audit_log
                .record(
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

/// 仅审计（不走乐观锁）：成功记 Success，失败记 Error。
/// 用于 settings/access/mode/geo/trust/ai 等不涉及 version 的写操作。
/// `op` 不接收 guard 参数（这些操作永远不启用乐观锁）。
pub(super) async fn audit_only_op<T, F, Fut>(
    state: &SharedState,
    command: &str,
    target: OperationTarget,
    target_id: Option<String>,
    action: &str,
    op: F,
) -> AppResult<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = AppResult<T>>,
{
    let ctx = ctl_audit_ctx(command, target, target_id, action, None);
    let result = op().await;
    match result {
        Ok(value) => {
            state
                .infra
                .audit_log
                .record(&ctx, OperationOutcome::Success, None, None, None)
                .await;
            Ok(value)
        }
        Err(error) => {
            let code = error_code_for(&error);
            state
                .infra
                .audit_log
                .record(
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

pub(super) fn required<'a>(args: &'a [String], index: usize, message: &str) -> AppResult<&'a str> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| AppError::BadRequest(message.to_string()))
}

pub(super) fn parse_action(value: &str) -> AppResult<AccessAction> {
    match value {
        "allow" => Ok(AccessAction::Allow),
        "deny" => Ok(AccessAction::Deny),
        _ => Err(AppError::BadRequest(
            "访问动作必须是 allow/deny".to_string(),
        )),
    }
}

pub(super) fn parse_network(value: &str) -> AppResult<IpNet> {
    value
        .parse::<IpNet>()
        .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))
        .map_err(|_| AppError::BadRequest("IP 或 CIDR 格式无效".to_string()))
}

pub(super) fn parse_minutes(args: &[String]) -> AppResult<Option<u64>> {
    let Some(position) = args.iter().position(|value| value == "--minutes") else {
        return Ok(None);
    };
    let value = required(args, position + 1, "--minutes 缺少数值")?;
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| AppError::BadRequest("minutes 必须是正整数".to_string()))
}

// 高影响操作（网络暴露、GeoIP 和可信可执行文件）在 AI 模式开启后与人工同等可用。
// AI 模式门控由 `execute_from` 入口统一检查（未开启时整批拒绝），无需再叠加短时授权层。
// （原 `ai assist` 短时授权层已按 2026-08-18 审计整改删除。）

/// `dl pause/resume <task_id|all>`：`all` → None（全局），数字 → Some(id)
pub(super) fn parse_task_id_or_all(target: &str) -> AppResult<Option<i32>> {
    if target == "all" {
        return Ok(None);
    }
    target
        .parse::<i32>()
        .map(Some)
        .map_err(|_| AppError::BadRequest("task_id 必须是整数或 all".to_string()))
}

/// 把 TaskOutcome 转成 ctl 返回的 JSON 信封。
pub(super) fn outcome_to_value(outcome: TaskOutcome) -> Value {
    json!({
        "ok": outcome.ok,
        "message": outcome.message,
        "download_id": outcome.download_id,
    })
}

/// 涉及 B 站 API 的命令前置：取 Cookie 并校验已登录，未登录返回结构化错误。
pub(super) async fn require_bili_login(state: &SharedState) -> AppResult<String> {
    let cookies = state.infra.settings_service.cookie_header().await?;
    if !Credential::from_cookie_header(&cookies).is_logged_in() {
        return Err(AppError::BiliNotLoggedIn(
            "B 站未登录，请先执行 cred qrcode 扫码登录".to_string(),
        ));
    }
    Ok(cookies)
}

pub(crate) fn split_command(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// `>` 前缀专家模式：TUI/stdin 输入 `> dl status` 时剥离 `>` 走扁平命令路径。
/// IPC 客户端 `ctl dl status` 不带 `>`，直接走 `execute()`。
pub(crate) fn strip_expert_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix('>')
        .map(|rest| rest.trim_start())
        .unwrap_or(trimmed)
}

pub(crate) fn format_response(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::download::TaskOutcome;

    #[test]
    fn strip_expert_prefix_handles_gt_prefix() {
        assert_eq!(strip_expert_prefix("> dl status"), "dl status");
        assert_eq!(strip_expert_prefix(">dl status"), "dl status");
        assert_eq!(strip_expert_prefix("  > dl status"), "dl status");
        assert_eq!(strip_expert_prefix("dl status"), "dl status");
        assert_eq!(strip_expert_prefix(""), "");
    }

    #[test]
    fn validate_since_duration_multibyte_does_not_panic() {
        // 尾字符为多字节时 split_at 字节切分会 panic；非法值应返回 BadRequest。
        assert!(validate_since_duration("1小时").is_err());
        assert!(validate_since_duration("小时").is_err());
        assert!(validate_since_duration("24小时").is_err());
        // 合法值不受影响
        assert!(validate_since_duration("1h").is_ok());
        assert!(validate_since_duration("24h").is_ok());
        assert!(validate_since_duration("7d").is_ok());
        assert!(validate_since_duration("30m").is_ok());
    }

    #[test]
    fn parse_task_id_or_all_works() {
        assert_eq!(parse_task_id_or_all("all").unwrap(), None);
        assert_eq!(parse_task_id_or_all("123").unwrap(), Some(123));
        assert!(parse_task_id_or_all("abc").is_err());
    }

    #[test]
    fn outcome_to_value_preserves_fields() {
        let accepted = TaskOutcome::accepted("已入队", 42);
        let value = outcome_to_value(accepted);
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["message"], json!("已入队"));
        assert_eq!(value["download_id"], json!(42));

        let rejected = TaskOutcome::rejected("重复任务");
        let value = outcome_to_value(rejected);
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["download_id"], json!(null));
    }
}
