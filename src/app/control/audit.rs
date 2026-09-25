//! 主题 6：审计与事件命令（`audit list` / `audit by-target` / `events`）。
//!
//! 只读查询命令，不写审计日志；`events --watch` 的流式订阅由 IPC 层拦截处理。

use crate::app::control::helpers::{
    parse_limit_flag, parse_since_flag, parse_source_flag, parse_target_type, required,
};
use crate::error::{AppError, AppResult};
use crate::state::SharedState;
use serde_json::{json, Value};

/// `audit list [--source <s>] [--since <1h|24h|7d>] [--limit N]`
/// `audit by-target <task|blogger|cookie|session> <id>`
pub(super) async fn audit_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：audit list|by-target".to_string(),
        ));
    };
    match sub {
        "list" => {
            let source = parse_source_flag(args)?;
            let since = parse_since_flag(args)?;
            let limit = parse_limit_flag(args, 100)?;
            let rows = state
                .infra
                .audit_log
                .list(source, since.as_deref(), limit)
                .await?;
            Ok(json!({
                "count": rows.len(),
                "logs": rows.iter().map(|r| r.to_api()).collect::<Vec<_>>(),
            }))
        }
        "by-target" => {
            let target_str = required(args, 2, "audit by-target 需要 target_type")?;
            let target_id = required(args, 3, "audit by-target 需要 target_id")?;
            let target = parse_target_type(target_str)?;
            let limit = parse_limit_flag(args, 100)?;
            let rows = state
                .infra
                .audit_log
                .by_target(target, target_id, limit)
                .await?;
            Ok(json!({
                "count": rows.len(),
                "logs": rows.iter().map(|r| r.to_api()).collect::<Vec<_>>(),
            }))
        }
        _ => Err(AppError::BadRequest(format!(
            "未知 audit 子命令 `{sub}`；用法：audit list|by-target"
        ))),
    }
}

/// `events [--watch] [--limit N]`
/// - 无 `--watch`：返回最近 N 条审计事件（非流式，IPC 友好）
/// - `--watch`：流式订阅（仅 IPC 连接支持，stdin loop 走 println）
pub(super) async fn events_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    // 只接受 --watch/--limit：未知 flag 报错而非静默忽略，避免拼错参数后
    // 拿到看似成功的结果。
    if let Some(unknown) = args
        .iter()
        .skip(1)
        .find(|arg| arg.starts_with("--") && !matches!(arg.as_str(), "--watch" | "--limit"))
    {
        return Err(AppError::BadRequest(format!(
            "events 不支持参数 `{unknown}`；用法：events [--watch] [--limit N]"
        )));
    }
    let watch = args.iter().any(|a| a == "--watch");
    let limit = parse_limit_flag(args, 50)?;
    if watch {
        // 流式模式由 handle_streaming 拦截，execute 不应被调到这里
        return Err(AppError::BadRequest(
            "events --watch 需通过 IPC 流式连接调用，stdin/TUI 暂不支持".to_string(),
        ));
    }
    let rows = state.infra.audit_log.list(None, None, limit).await?;
    Ok(json!({
        "count": rows.len(),
        "events": rows.iter().map(|r| r.to_api()).collect::<Vec<_>>(),
    }))
}
