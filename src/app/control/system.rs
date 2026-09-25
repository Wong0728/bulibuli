//! 主题 4：系统命令（`sys`）：状态、配置、Aria2 重启、FFmpeg 探测、日志路径、刷新触发。

use crate::app::control::helpers::{
    audit_only_op, ctl_audit_ctx, require_bili_login, required, with_guarded_op,
};
use crate::error::{AppError, AppResult};
use crate::models::operation_log::OperationTarget;
use crate::state::SharedState;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

pub(super) async fn sys_command(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let Some(sub) = args.get(1).map(String::as_str) else {
        return Err(AppError::BadRequest(
            "用法：sys status|config|aria2-restart|ffmpeg-test|logs|refresh".to_string(),
        ));
    };
    match sub {
        "status" => Ok(sys_status_value(state).await),
        "config" => Ok(sys_config_value(state)),
        "aria2-restart" => sys_aria2_restart(state).await,
        "ffmpeg-test" => sys_ffmpeg_test(state).await,
        "logs" => Ok(sys_logs_value(state)),
        "refresh" => sys_refresh(state, args).await,
        _ => Err(AppError::BadRequest(format!(
            "未知 sys 子命令 `{sub}`；用法：sys status|config|aria2-restart|ffmpeg-test|logs|refresh"
        ))),
    }
}

async fn sys_status_value(state: &SharedState) -> Value {
    let active_mode = state.bili.security.current().mode;
    let configured_mode = state.bili.security.configured().mode;
    let pairing = state.bili.auth.pairing_state().await;
    let sessions = state
        .bili
        .auth
        .list_sessions()
        .await
        .map(|s| s.len())
        .unwrap_or(0);
    let uptime_secs = state.infra.started_at.elapsed().as_secs();
    let aria2_status = state.media.aria2.status().await;
    let ai_enabled = state.infra.ai_skill_enabled.load(Ordering::Relaxed);
    json!({
        // `mode` 保留为当前兼容字段；新调用方应区分 active/configured。
        "mode": active_mode,
        "active_mode": active_mode,
        "configured_mode": configured_mode,
        "restart_required": active_mode != configured_mode,
        "pairing": pairing,
        "sessions": sessions,
        "ai_skill_enabled": ai_enabled,
        "uptime_secs": uptime_secs,
        "aria2_status": aria2_status,
    })
}

pub(super) fn sys_config_value(state: &SharedState) -> Value {
    let mut value =
        serde_json::to_value(state.bili.security.current()).unwrap_or_else(|_| json!({}));
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "effective_geo_db".to_string(),
            serde_json::to_value(state.bili.security.effective_geo_db()).unwrap_or(Value::Null),
        );
        obj.insert(
            "ai_skill_enabled".to_string(),
            json!(state.infra.ai_skill_enabled.load(Ordering::Relaxed)),
        );
    }
    value
}

async fn sys_aria2_restart(state: &SharedState) -> AppResult<Value> {
    audit_only_op(
        state,
        "sys aria2-restart",
        OperationTarget::Settings,
        None,
        "aria2_restart",
        || async move {
            let settings = state.infra.settings_service.current();
            // init 内部已先 stop 再 start，等价于 restart
            state
                .media
                .aria2
                .init(&settings)
                .await
                .map_err(|e| AppError::Internal(format!("重启 Aria2 失败: {e}")))?;
            let status = state.media.aria2.status().await;
            Ok(json!({
                "restarted": true,
                "aria2_status": status,
            }))
        },
    )
    .await
}

async fn sys_ffmpeg_test(state: &SharedState) -> AppResult<Value> {
    // 与 Web /api/settings/ffmpeg-test 保持一致：读取设置里的 ffmpeg.mode + custom_path，
    // 而不是固定 "auto" 探测——否则设置页配好自定义 FFmpeg 后 ctl 仍报"未找到 FFmpeg"。
    let settings = state.infra.settings_service.current();
    let mode = settings.ffmpeg.mode.clone();
    let custom_path = (!settings.ffmpeg.custom_path.trim().is_empty())
        .then(|| settings.ffmpeg.custom_path.clone());
    let (path, source) = state
        .media
        .video_processor
        .detect_ffmpeg(&mode, custom_path.as_deref())
        .await;
    let Some(path) = path else {
        return Ok(json!({
            "available": false,
            "path": null,
            "source": source,
            "version": null,
            "message": "未找到 FFmpeg",
        }));
    };
    let (ok, version) = state.media.video_processor.check_ffmpeg(&path).await;
    Ok(json!({
        "available": ok,
        "path": path.file_name().and_then(|name| name.to_str()).unwrap_or("ffmpeg"),
        "source": source,
        "version": version,
    }))
}

fn sys_logs_value(state: &SharedState) -> Value {
    json!({
        "logs_dir": state.infra.paths.data_dir.join("logs"),
        // 与 tracing_appender 实际产物一致：filename_prefix("app") + 日期 + suffix("log")
        // 生成 app.YYYY-MM-DD.log（不是 app.log.YYYY-MM-DD）。
        "hint": "日志按天滚动，文件名形如 app.YYYY-MM-DD.log",
    })
}

async fn sys_refresh(state: &SharedState, args: &[String]) -> AppResult<Value> {
    let scope = required(args, 2, "sys refresh 需要 scope（board|blogger|video）")?;
    match scope {
        "board" => {
            let ctx = ctl_audit_ctx(
                "sys refresh board",
                OperationTarget::Blogger,
                None,
                "refresh_board",
                None,
            );
            let refresh = state.business.refresh_service.clone();
            let outcome = with_guarded_op(
                state,
                ctx,
                OperationTarget::Blogger,
                None,
                None,
                move |_| async move {
                    let outcome = refresh
                        .trigger_l1()
                        .await
                        .map_err(|e| AppError::Internal(format!("触发 board 刷新失败: {e}")))?;
                    Ok(json!({"scope": "board", "triggered": outcome.refreshed, "skipped": outcome.skipped}))
                },
            )
            .await?;
            Ok(outcome)
        }
        "blogger" => {
            let ctx = ctl_audit_ctx(
                "sys refresh blogger",
                OperationTarget::Blogger,
                None,
                "refresh_blogger",
                None,
            );
            let refresh = state.business.refresh_service.clone();
            let outcome = with_guarded_op(
                state,
                ctx,
                OperationTarget::Blogger,
                None,
                None,
                move |_| async move {
                    let outcome = refresh
                        .trigger_l2()
                        .await
                        .map_err(|e| AppError::Internal(format!("触发 blogger 刷新失败: {e}")))?;
                    Ok(json!({"scope": "blogger", "triggered": outcome.refreshed, "skipped": outcome.skipped}))
                },
            )
            .await?;
            Ok(outcome)
        }
        "video" => {
            let bvid = required(args, 3, "sys refresh video 需要 bvid")?;
            // 单视频刷新走 B 站 API，需登录前置
            require_bili_login(state).await?;
            let ctx = ctl_audit_ctx(
                &format!("sys refresh video {bvid}"),
                OperationTarget::Task,
                Some(bvid.to_string()),
                "refresh_video",
                None,
            );
            let refresh = state.business.refresh_service.clone();
            let bvid_owned = bvid.to_string();
            let outcome = with_guarded_op(
                state,
                ctx,
                OperationTarget::Task,
                Some(bvid),
                None,
                move |_| async move {
                    // 直接透传 AppError（查不到记录时返回 NOT_FOUND，与 Web 一致）
                    refresh.trigger_video(&bvid_owned).await?;
                    Ok(json!({"scope": "video", "bvid": bvid_owned, "triggered": 1}))
                },
            )
            .await?;
            Ok(outcome)
        }
        "verify" => Err(AppError::BadRequest(
            "verify 是独立 worker，无法通过 ctl 触发".to_string(),
        )),
        _ => Err(AppError::BadRequest(format!(
            "未知 refresh scope `{scope}`；用法：sys refresh board|blogger|video <bvid>"
        ))),
    }
}
