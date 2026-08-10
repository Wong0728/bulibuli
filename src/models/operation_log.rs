//! 审计日志实体：记录所有写操作的来源、目标、版本与结果，供多端冲突追溯与 `ctl audit` 查询。
//!
//! 表结构由 `m20260803_000003_add_version_and_operation_log` 迁移创建。
//! 写入由 `services/audit_log.rs::AuditLogService` 统一封装，确保字段语义一致。

use chrono::Utc;
use sea_orm::entity::prelude::*;
use serde::Serialize;

/// 操作来源：标识哪个通道触发了写操作，用于审计追溯与 `ctl audit --source` 过滤。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationSource {
    /// 前端 Web GUI（HTTP handler）
    Frontend,
    /// TUI 控制台（ratatui / stdin loop）
    Tui,
    /// AI Skill（IPC `ctl` 子命令）
    AiSkill,
    /// 系统自动操作（监控/刷新/烧录等后台 worker）
    System,
}

impl OperationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frontend => "frontend",
            Self::Tui => "tui",
            Self::AiSkill => "ai_skill",
            Self::System => "system",
        }
    }
}

impl std::str::FromStr for OperationSource {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "frontend" => Ok(Self::Frontend),
            "tui" => Ok(Self::Tui),
            "ai_skill" => Ok(Self::AiSkill),
            "system" => Ok(Self::System),
            other => Err(format!("未知 OperationSource: {other}")),
        }
    }
}

/// 操作结果：success / conflict / error。决定 `error_code` 字段是否非空。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Success,
    Conflict,
    Error,
}

impl OperationOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Conflict => "conflict",
            Self::Error => "error",
        }
    }
}

/// 目标资源类型：与 `target_id` 配合定位被修改的资源。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationTarget {
    Task,
    Blogger,
    Settings,
    Cookie,
    Session,
}

impl OperationTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Blogger => "blogger",
            Self::Settings => "settings",
            Self::Cookie => "cookie",
            Self::Session => "session",
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "operation_log")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// ISO8601 (UTC) 时间戳
    pub at: String,
    /// `frontend` / `tui` / `ai_skill` / `system`
    pub source: String,
    /// 调用方标识：session_id / ctl_pid / "system"
    pub caller_id: String,
    /// 路由或命令：`/api/v1/download/cancel` 或 `dl cancel`
    pub route_or_command: String,
    /// `task` / `blogger` / `settings` / `cookie` / `session`
    pub target_type: String,
    /// 任务 id / uid / null
    pub target_id: Option<String>,
    /// `cancel` / `resume` / `add` / `delete` 等
    pub action: String,
    /// 调用方传入的版本号（NULL=未传，按"最后写入胜出"语义）
    pub expected_version: Option<i32>,
    /// 执行后的新版本号（失败时为 NULL）
    pub new_version: Option<i32>,
    /// `success` / `conflict` / `error`
    pub outcome: String,
    /// 失败时的错误码：`CONFLICT` / `AI_SKILL_DISABLED` / `BILI_NOT_LOGGED_IN` 等
    pub error_code: Option<String>,
    /// 全链路追踪 ID（每次调用方生成一个，便于跨服务关联）
    pub request_id: String,
    /// 自由扩展字段（JSON 字符串），如冲突时的"当前持有者"信息
    pub detail: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// 序列化为 API 返回的 JSON：把字符串字段还原为枚举（便于前端展示）。
    pub fn to_api(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "at": self.at,
            "source": self.source,
            "caller_id": self.caller_id,
            "route_or_command": self.route_or_command,
            "target_type": self.target_type,
            "target_id": self.target_id,
            "action": self.action,
            "expected_version": self.expected_version,
            "new_version": self.new_version,
            "outcome": self.outcome,
            "error_code": self.error_code,
            "request_id": self.request_id,
            "detail": self.detail,
        })
    }
}

/// 生成 ISO8601 UTC 时间戳字符串，供审计日志 `at` 字段使用。
pub fn now_utc_iso8601() -> String {
    Utc::now().to_rfc3339()
}
