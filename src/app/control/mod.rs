//! 本机控制通道：提供 IPC 服务并分发两套命令入口。
//!
//! 命令体系（P1 重构后）：
//! - **专家模式**：扁平命令，`bulibuli.exe ctl <command> [args...]` 直接调用，AI 脚本化友好
//! - **引导模式**：TUI 内的菜单包装，最终翻译为扁平命令调用 `execute()`（UI 在 `crate::app::tui`）
//! - **`>` 前缀**：TUI/stdin 输入时以 `>` 开头表示专家模式直输，由 `submit` 层剥离后调用 `execute()`
//!
//! 命令分为 5 个主题，另保留旧名 alias：
//! - `ai` / `dl` / `blg` / `sys` / `cred` —— 主题前缀，二级分发到各主题命令
//! - `status` / `pair` / `sessions` / `revoke` / `access` / `mode` / `geo` / `trust` / `config` / `quit` / `help`
//!   —— 旧名 alias 保留，避免破坏存量脚本
//!
//! AI 模式门控（P3）：未启用时放行 `status` / `help` / `quit` / `ai on` / `pair`，
//! 以及只读诊断 `sys status`。
//! 门控在 [`execute_from`] 入口检查 `ai_skill_enabled` 标志，拒绝时返回 `AI_SKILL_DISABLED`。
//!
//! 模块划分：
//! - [`registry`]：命令清单（help / skill.md 的唯一真实来源）
//! - [`transport`]：IPC 传输层（Windows 命名管道 / Unix socket，客户端 + 服务端）
//! - [`stream`]：请求/响应帧读写与事件流
//! - [`helpers`]：参数解析、审计上下文与乐观锁包装
//! - [`policy`] / [`download`] / [`blogger`] / [`system`] / [`credential`] / [`audit`]：各主题命令

#[path = "../control_origin.rs"]
mod control_origin;

mod audit;
mod blogger;
mod credential;
mod download;
mod helpers;
mod policy;
mod registry;
mod stream;
mod system;
mod transport;

use crate::error::{AppError, AppResult};
use crate::state::SharedState;
use serde_json::Value;
use std::sync::atomic::Ordering;

pub use transport::{run_client, start_server, start_stdin_loop};

pub(crate) use control_origin::CommandOrigin;
pub(crate) use helpers::{format_response, split_command, strip_expert_prefix};

use audit::{audit_command, events_command};
use blogger::blg_command;
use credential::cred_command;
use download::dl_command;
use policy::{
    access_command, ai_command, geo_command, legacy_status, mode_command, pair_command,
    quit_command, revoke_command, trust_command,
};
use registry::help;
use system::{sys_command, sys_config_value};

/// 执行通过仅供 AI 使用的 `ctl` IPC 到达的命令。
pub async fn execute(state: &SharedState, args: &[String]) -> AppResult<Value> {
    execute_from(state, args, CommandOrigin::AiCtl).await
}

/// 按可信的本机来源执行命令。只有 TUI/stdin 可以传入 `HumanTerminal`；
/// IPC 调用方始终使用上面的 `execute` 包装函数。
pub(crate) async fn execute_from(
    state: &SharedState,
    args: &[String],
    origin: CommandOrigin,
) -> AppResult<Value> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(help());
    };
    // AI Skill 模式门控：仅在 AI IPC 来源时生效，人工终端（TUI/stdin）无条件放行。
    // 未启用时放行 status / help / quit / ai（用于重新启用）/ pair，
    // 以及只读诊断 `sys status`（--help 推荐新用户首先执行的命令）。
    let sys_status_probe = command == "sys" && args.get(1).map(String::as_str) == Some("status");
    if matches!(origin, CommandOrigin::AiCtl)
        && !state.infra.ai_skill_enabled.load(Ordering::Relaxed)
        && !matches!(command, "status" | "help" | "quit" | "ai" | "pair")
        && !sys_status_probe
    {
        return Err(AppError::AiSkillDisabled(format!(
            "AI Skill 模式未启用，ctl 放行 status/help/quit/ai/pair/sys status；当前命令 `{command}` 被拒绝。使用 `ai on` 启用"
        )));
    }
    match command {
        // 旧名 alias（保留以兼容现有脚本）
        "help" => Ok(help()),
        "status" => Ok(legacy_status(state).await),
        "pair" => pair_command(state, args).await,
        "sessions" => Ok(serde_json::to_value(
            state.bili.auth.list_sessions().await?,
        )?),
        "revoke" => revoke_command(state, args).await,
        "access" => access_command(state, args).await,
        "mode" => mode_command(state, args).await,
        "geo" => geo_command(state, args).await,
        "trust" => trust_command(state, args).await,
        "config" => Ok(sys_config_value(state)),
        "quit" => quit_command(state).await,
        // 新主题分发
        "ai" => ai_command(state, args).await,
        "dl" => dl_command(state, args).await,
        "blg" => blg_command(state, args).await,
        "sys" => sys_command(state, args).await,
        "cred" => cred_command(state, args).await,
        // P2 审计与事件
        "audit" => audit_command(state, args).await,
        "events" => events_command(state, args).await,
        _ => Err(AppError::BadRequest(format!(
            "未知控制命令 `{command}`；使用 help 查看"
        ))),
    }
}
