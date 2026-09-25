//! 主题 2：下载命令（`dl`）：入队、暂停/恢复、重试、移除、优先级。
//!
//! 写操作统一走 `with_guarded_op`（乐观锁 + 审计），入参是 bvid 或 task_id；
//! 字幕烧录编排仅 Web 支持，ctl 不提供半实现入口。

use crate::app::control::helpers::{
    ctl_audit_ctx, extract_expected_version, outcome_to_value, parse_task_id_or_all,
    require_bili_login, required, with_guarded_op,
};
use crate::error::{AppError, AppResult};
use crate::models::operation_log::OperationTarget;
use crate::services::download::RetryTrigger;
use crate::services::url_parser::{parse_media_input, ResolvedMedia};
use crate::state::SharedState;
use serde_json::{json, Value};

pub(super) async fn dl_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：dl status|add|pause|resume|retry|remove|priority".to_string(),
        ));
    };
    match sub {
        "status" => Ok(json!({
            "queue": state.media.download_manager.queue_metrics().await?,
            "health": state.media.download_manager.get_health().await,
        })),
        "add" => dl_add(state, args).await,
        "pause" => dl_pause_resume(state, args, "pause").await,
        "resume" => dl_pause_resume(state, args, "resume").await,
        "retry" => dl_retry(state, args).await,
        "remove" => dl_remove(state, args).await,
        "priority" => dl_priority(state, args).await,
        // 字幕烧录编排仅 Web 支持（前端 /api/download/burn），ctl 不提供半实现入口，
        // 避免"帮助可见、调用必错"的假命令。
        _ => Err(AppError::BadRequest(format!(
            "未知 dl 子命令 `{sub}`；用法：dl status|add|pause|resume|retry|remove|priority（烧录请用前端网页）"
        ))),
    }
}

/// `dl add <BV>`：取流 + 入队（source=manual）。
/// 仅支持 BV；AV/ep/ss/fp 在 P1 暂未支持，明确返回错误。
///
/// 审计：target=Task, target_id=bvid, action=add。幂等入队（同 bvid 返回同一 task_id），
/// 不要求 expected_version；outcome 由 add_task 返回值决定（accepted/rejected/done）。
async fn dl_add(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let input = required(args, 2, "dl add 需要 BV 号")?;
    let media =
        parse_media_input(input).map_err(|error| AppError::BadRequest(error.to_string()))?;
    let bvid = match media {
        ResolvedMedia::VideoBv(b) => b,
        ResolvedMedia::VideoAv(_)
        | ResolvedMedia::Episode(_)
        | ResolvedMedia::Season(_)
        | ResolvedMedia::Course(_) => {
            return Err(AppError::BadRequest(
                "AV/ep/ss/fp 暂未支持，请用 BV 号（dl add BV1xx411c7mD）".to_string(),
            ))
        }
    };

    let cookies = require_bili_login(state).await?;
    let settings = state.infra.settings_service.current();
    let qn = settings.query.video_quality;
    let fnval = settings.query.video_format;

    // 获取标题
    let info = state
        .bili
        .bili_api
        .get_video_info(&bvid, &cookies)
        .await
        .map_err(|e| AppError::Internal(format!("获取视频信息失败: {e}")))?;
    let title = if info.title.is_empty() {
        bvid.clone()
    } else {
        info.title.clone()
    };

    // 获取单 P 流（cid=None 使用默认分 P）
    let streams = state
        .bili
        .bili_api
        .get_video_urls(&bvid, &cookies, fnval, Some(qn), None)
        .await
        .map_err(|e| AppError::Internal(format!("获取视频流失败: {e}")))?;
    let selected = streams
        .qualities
        .iter()
        .filter(|q| q.quality <= qn)
        .max_by_key(|q| q.quality)
        .or_else(|| streams.qualities.first())
        .ok_or_else(|| AppError::NotFound("该视频无可用的视频流".to_string()))?;
    if selected.url.is_empty() {
        return Err(AppError::NotFound("获取视频下载链接失败".to_string()));
    }

    let ctx = ctl_audit_ctx(
        &format!("dl add {bvid}"),
        OperationTarget::Task,
        Some(bvid.clone()),
        "add",
        None,
    );
    let dm = state.media.download_manager.clone();
    let url = selected.url.clone();
    let quality = selected.quality;
    let bvid_for_closure = bvid.clone();
    let title_for_closure = title.clone();
    let outcome = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        Some(&bvid),
        None,
        move |_guard| async move {
            dm.add_task(
                &bvid_for_closure,
                &title_for_closure,
                &url,
                &cookies,
                quality,
                "video",
                None,
                "manual",
                None,
                None,
            )
            .await
            .map_err(AppError::from)
        },
    )
    .await?;
    Ok(outcome_to_value(outcome))
}

/// `dl pause <task_id|all> [--expected-version N]` / `dl resume <task_id|all> [--expected-version N]`
///
/// 乐观锁：`task_id` 为具体数字且传 `--expected-version` 时启用；`all` 跳过乐观锁（审计 only）。
async fn dl_pause_resume(state: &SharedState, args: &[String], action: &str) -> AppResult<Value> {
    let (args, expected_version) = extract_expected_version(args)?;
    let target = required(&args, 2, &format!("dl {action} 需要 task_id 或 all"))?;
    let task_id = parse_task_id_or_all(target)?;
    // all 模式为批量操作，不使用乐观锁（影响多行，version 无意义）
    let (target_id_for_guard, version_for_ctx) = match task_id {
        Some(id) => (Some(id.to_string()), expected_version),
        None => (None, None),
    };
    let ctx = ctl_audit_ctx(
        &format!("dl {action} {target}"),
        OperationTarget::Task,
        target_id_for_guard.clone(),
        action,
        version_for_ctx,
    );
    let dm = state.media.download_manager.clone();
    let outcome = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        target_id_for_guard.as_deref(),
        expected_version,
        move |_guard| async move {
            if action == "pause" {
                dm.pause_task(task_id).await.map_err(AppError::from)
            } else {
                dm.resume_task(task_id).await.map_err(AppError::from)
            }
        },
    )
    .await?;
    Ok(outcome_to_value(outcome))
}

/// `dl retry <bvid> [video|audio] | all-failed`
///
/// 审计：target=Task, target_id=bvid 或 "all-failed", action=retry。
/// 入参是 bvid（非 task_id），不传 expected_version；按"最后写入胜出"语义。
async fn dl_retry(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let target = required(args, 2, "dl retry 需要 bvid 或 all-failed")?;
    if target == "all-failed" {
        let ctx = ctl_audit_ctx(
            "dl retry all-failed",
            OperationTarget::Task,
            None,
            "retry_all_failed",
            None,
        );
        let dm = state.media.download_manager.clone();
        let outcome = with_guarded_op(
            state,
            ctx,
            OperationTarget::Task,
            None,
            None,
            move |_guard| async move { dm.retry_all_failed(None).await.map_err(AppError::from) },
        )
        .await?;
        return Ok(outcome_to_value(outcome));
    }
    let task_type = args.get(3).map(String::as_str).unwrap_or("video");
    if !matches!(task_type, "video" | "audio") {
        return Err(AppError::BadRequest(
            "task_type 必须是 video 或 audio".to_string(),
        ));
    }
    let ctx = ctl_audit_ctx(
        &format!("dl retry {target} {task_type}"),
        OperationTarget::Task,
        Some(target.to_string()),
        "retry",
        None,
    );
    let dm = state.media.download_manager.clone();
    let bvid = target.to_string();
    let outcome = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        Some(target),
        None,
        move |_guard| async move {
            dm.retry_task(&bvid, task_type, RetryTrigger::Manual)
                .await
                .map_err(AppError::from)
        },
    )
    .await?;
    Ok(outcome_to_value(outcome))
}

/// `dl remove <bvid> [video|audio]`
///
/// 审计：target=Task, target_id=bvid, action=remove。
async fn dl_remove(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let bvid = required(args, 2, "dl remove 需要 bvid")?;
    let task_type = args.get(3).map(String::as_str).unwrap_or("video");
    if !matches!(task_type, "video" | "audio") {
        return Err(AppError::BadRequest(
            "task_type 必须是 video 或 audio".to_string(),
        ));
    }
    let ctx = ctl_audit_ctx(
        &format!("dl remove {bvid} {task_type}"),
        OperationTarget::Task,
        Some(bvid.to_string()),
        "remove",
        None,
    );
    let dm = state.media.download_manager.clone();
    let bvid_owned = bvid.to_string();
    let outcome = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        Some(bvid),
        None,
        move |_guard| async move {
            dm.remove_task(&bvid_owned, task_type)
                .await
                .map_err(AppError::from)
        },
    )
    .await?;
    Ok(outcome_to_value(outcome))
}

/// `dl priority <bvid> <level> [video|audio]`
///
/// 审计：target=Task, target_id=bvid, action=priority。
async fn dl_priority(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let bvid = required(args, 2, "dl priority 需要 bvid")?;
    let level_str = required(args, 3, "dl priority 需要 level（1..=300）")?;
    let level: i32 = level_str
        .parse()
        .map_err(|_| AppError::BadRequest("level 必须是 1..=300 的整数".to_string()))?;
    if !(1..=300).contains(&level) {
        return Err(AppError::BadRequest(
            "level 必须在 1..=300 范围内".to_string(),
        ));
    }
    let task_type = args.get(4).map(String::as_str).unwrap_or("video");
    if !matches!(task_type, "video" | "audio") {
        return Err(AppError::BadRequest(
            "task_type 必须是 video 或 audio".to_string(),
        ));
    }
    let ctx = ctl_audit_ctx(
        &format!("dl priority {bvid} {level} {task_type}"),
        OperationTarget::Task,
        Some(bvid.to_string()),
        "priority",
        None,
    );
    let dm = state.media.download_manager.clone();
    let bvid_owned = bvid.to_string();
    let result = with_guarded_op(
        state,
        ctx,
        OperationTarget::Task,
        Some(bvid),
        None,
        move |_guard| async move {
            dm.set_priority(&bvid_owned, task_type, level)
                .await
                .map_err(|e| AppError::BadRequest(e.to_string()))
        },
    )
    .await?;
    Ok(result)
}
