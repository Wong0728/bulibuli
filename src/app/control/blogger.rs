//! 主题 3：博主命令（`blg`）：搜索、添加监控、列表、删除、启停监控。
//!
//! 写操作统一走 `with_guarded_op`（乐观锁 + 审计）；拉取资料失败时兜底空资料不阻断添加。

use crate::app::control::helpers::{
    ctl_audit_ctx, extract_expected_version, require_bili_login, required, with_guarded_op,
};
use crate::error::{AppError, AppResult};
use crate::models::operation_log::OperationTarget;
use crate::services::blogger::NewBlogger;
use crate::state::SharedState;
use serde_json::{json, Value};

pub(super) async fn blg_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：blg search|add|list|del|monitor".to_string(),
        ));
    };
    match sub {
        "search" => blg_search(state, args).await,
        "add" => blg_add(state, args).await,
        "list" => blg_list(state, args).await,
        "del" => blg_del(state, args).await,
        "monitor" => blg_monitor(state, args).await,
        _ => Err(AppError::BadRequest(format!(
            "未知 blg 子命令 `{sub}`；用法：blg search|add|list|del|monitor"
        ))),
    }
}

/// `blg search <keyword>`：B 站用户搜索（page=1, page_size=10）
async fn blg_search(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let keyword = required(args, 2, "blg search 需要 keyword")?;
    let cookies = require_bili_login(state).await?;
    let page = state
        .bili
        .bili_api
        .search_users(keyword, &cookies, 1, 10)
        .await
        .map_err(|e| AppError::Internal(format!("搜索 UP 主失败: {e}")))?;
    Ok(json!({
        "total": page.total,
        "users": page.users,
    }))
}

/// `blg add <uid>`：加为监控任务（has_auto_task=true, monitor_enabled=true）
///
/// 审计：target=Blogger, target_id=uid, action=add。
async fn blg_add(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let uid = required(args, 2, "blg add 需要 uid")?;
    let uid_i64: i64 = uid
        .parse()
        .map_err(|_| AppError::BadRequest("uid 必须是整数".to_string()))?;
    let cookies = require_bili_login(state).await?;

    // 预查重复：与 Web（manage.rs）一致返回友好 Conflict，而不是数据库唯一键错误
    // 被包成 INTERNAL "添加博主失败"。
    if let Some(existing) = state
        .business
        .blogger_service
        .find_by_uid(uid)
        .await
        .map_err(|e| AppError::Internal(format!("查询博主失败: {e}")))?
    {
        if existing.has_auto_task {
            return Err(AppError::Conflict("该博主已有自动任务".to_string()));
        }
    }

    // 拉取资料（失败时回退到空资料，不阻断添加）
    let (name, face, sign, level, fans) = match state
        .bili
        .bili_api
        .get_user_info(uid_i64, &cookies, false)
        .await
    {
        Ok(profile) => (
            Some(profile.name).filter(|s| !s.is_empty()),
            Some(profile.face).filter(|s| !s.is_empty()),
            Some(profile.sign).filter(|s| !s.is_empty()),
            Some(profile.level as i32),
            Some(profile.fans),
        ),
        Err(error) => {
            tracing::warn!(uid, %error, "blg add 拉取资料失败，使用兜底信息");
            (None, None, None, None, None)
        }
    };

    let ctx = ctl_audit_ctx(
        &format!("blg add {uid}"),
        OperationTarget::Blogger,
        Some(uid.to_string()),
        "add",
        None,
    );
    let svc = state.business.blogger_service.clone();
    let uid_for_closure = uid.to_string();
    let blogger = with_guarded_op(
        state,
        ctx,
        OperationTarget::Blogger,
        Some(uid),
        None,
        move |_guard| async move {
            svc.add_blogger(NewBlogger {
                uid: uid_for_closure,
                name,
                min_interval: 60,
                max_interval: 300,
                face,
                sign,
                level,
                fans,
                download_video: true,
                download_danmaku: true,
                download_comments: true,
                download_cover: true,
                burn_danmaku: false,
                burn_subtitle: false,
                series_filter_regex: None,
                active_windows: None,
                monitor_enabled: true,
                is_saved: false,
                has_auto_task: true,
            })
            .await
            .map_err(|e| AppError::Internal(format!("添加博主失败: {e}")))
        },
    )
    .await?;
    Ok(json!({
        "blogger": blogger,
        "message": "博主已添加为监控任务",
    }))
}

/// `blg list [monitor|saved]`
async fn blg_list(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let filter = args.get(2).map(String::as_str);
    let mut result = json!({});
    if matches!(filter, None | Some("monitor")) {
        let monitor = state
            .business
            .blogger_service
            .list_auto_tasks()
            .await
            .map_err(|e| AppError::Internal(format!("列出监控博主失败: {e}")))?;
        result["monitor"] = serde_json::to_value(monitor)?;
    }
    if matches!(filter, None | Some("saved")) {
        let saved = state
            .business
            .blogger_service
            .list_saved()
            .await
            .map_err(|e| AppError::Internal(format!("列出收藏博主失败: {e}")))?;
        result["saved"] = serde_json::to_value(saved)?;
    }
    if let Some(other) = filter.filter(|f| !matches!(*f, "monitor" | "saved")) {
        return Err(AppError::BadRequest(format!(
            "blg list 参数必须是 monitor|saved，收到 `{other}`"
        )));
    }
    Ok(result)
}

/// `blg del <uid> [--expected-version N]`：先试监控，再试收藏
///
/// 乐观锁：传 `--expected-version` 时校验 blogger.version；不匹配返回 CONFLICT。
async fn blg_del(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let (args, expected_version) = extract_expected_version(args)?;
    let uid = required(&args, 2, "blg del 需要 uid")?;
    // find_by_uid 拿 id（监控和收藏共用同一张表，uid 唯一）
    let blogger = state
        .business
        .blogger_service
        .find_by_uid(uid)
        .await
        .map_err(|e| AppError::Internal(format!("查询博主失败: {e}")))?
        .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?;
    let id = blogger.id;
    let ctx = ctl_audit_ctx(
        &format!("blg del {uid}"),
        OperationTarget::Blogger,
        Some(id.to_string()),
        "delete",
        expected_version,
    );
    let svc = state.business.blogger_service.clone();
    let result = with_guarded_op(
        state,
        ctx,
        OperationTarget::Blogger,
        Some(&id.to_string()),
        expected_version,
        move |_guard| async move {
            if blogger.has_auto_task {
                let removed_uid = svc
                    .remove_auto_task(id)
                    .await
                    .map_err(|e| AppError::Internal(format!("删除监控博主失败: {e}")))?
                    .ok_or_else(|| AppError::NotFound("未找到监控博主".to_string()))?;
                Ok(json!({"removed": removed_uid, "kind": "monitor"}))
            } else if blogger.is_saved {
                let removed_uid = svc
                    .remove_saved(id)
                    .await
                    .map_err(|e| AppError::Internal(format!("删除收藏博主失败: {e}")))?
                    .ok_or_else(|| AppError::NotFound("未找到收藏博主".to_string()))?;
                Ok(json!({"removed": removed_uid, "kind": "saved"}))
            } else {
                Err(AppError::NotFound(
                    "该博主既不是监控也不是收藏，无法删除".to_string(),
                ))
            }
        },
    )
    .await?;
    Ok(result)
}

/// `blg monitor on|off <uid> [--expected-version N]`
async fn blg_monitor(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let (args, expected_version) = extract_expected_version(args)?;
    let action = required(&args, 2, "blg monitor 需要 on|off")?;
    let uid = required(&args, 3, "blg monitor 需要 uid")?;
    let running = match action {
        "on" => true,
        "off" => false,
        _ => {
            return Err(AppError::BadRequest(
                "blg monitor 第二个参数必须是 on|off".to_string(),
            ))
        }
    };
    // 解析 UID → ID 用于乐观锁（find_by_uid 内部缓存，开销小）。
    let blogger = state
        .business
        .blogger_service
        .find_by_uid(uid)
        .await
        .map_err(|e| AppError::Internal(format!("查询博主失败: {e}")))?
        .ok_or_else(|| AppError::NotFound("未找到该博主".to_string()))?;
    let id_str = blogger.id.to_string();
    let ctx = ctl_audit_ctx(
        &format!("blg monitor {action} {uid}"),
        OperationTarget::Blogger,
        Some(id_str.clone()),
        if running { "monitor_on" } else { "monitor_off" },
        expected_version,
    );
    let svc = state.business.blogger_service.clone();
    let result = with_guarded_op(
        state,
        ctx,
        OperationTarget::Blogger,
        Some(&id_str),
        expected_version,
        move |_guard| async move {
            let toggle = svc
                .set_monitor_running(uid, running)
                .await
                .map_err(|e| AppError::Internal(format!("切换监控状态失败: {e}")))?;
            let message = match toggle {
                crate::services::blogger::MonitorToggle::NotFound => {
                    return Err(AppError::NotFound("未找到该博主".to_string()))
                }
                crate::services::blogger::MonitorToggle::AlreadyInState => {
                    if running {
                        "博主监控已是运行中状态"
                    } else {
                        "博主监控已是停止状态"
                    }
                }
                crate::services::blogger::MonitorToggle::Updated => {
                    if running {
                        "博主监控已启动"
                    } else {
                        "博主监控已停止"
                    }
                }
            };
            Ok(json!({
                "monitor_running": running,
                "message": message,
            }))
        },
    )
    .await?;
    Ok(result)
}
