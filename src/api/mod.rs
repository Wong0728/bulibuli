pub(crate) mod auth;
mod backup;
mod bili_resource;
mod blogger;
mod cookies;
mod cover;
pub(crate) mod download;
mod foundation;
mod health;
mod history;
pub(crate) mod live;
mod logs;
mod refresh;
mod settings;
pub(crate) mod setup;
mod task;
mod update;
mod video;

use crate::error::AppError;
use crate::state::SharedState;
use axum::Router;

const MAX_BILI_ID: i64 = 1_000_000_000_000;

pub(crate) fn validate_bili_id(name: &str, value: i64) -> Result<(), AppError> {
    if !(1..=MAX_BILI_ID).contains(&value) {
        return Err(AppError::BadRequest(format!(
            "{name} 必须是 1 到 {MAX_BILI_ID} 之间的正整数"
        )));
    }
    Ok(())
}

pub(crate) fn validate_fnval(value: i32) -> Result<(), AppError> {
    if !(0..=4096).contains(&value) {
        return Err(AppError::BadRequest("fnval 超出允许范围".to_string()));
    }
    Ok(())
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .merge(auth::router())
        .merge(backup::router())
        .merge(health::router())
        .merge(video::router())
        .merge(blogger::router())
        .merge(download::router())
        .merge(task::router())
        .merge(history::router())
        .merge(settings::router())
        .merge(logs::router())
        .merge(cookies::router())
        .merge(cover::router())
        .merge(refresh::router())
        .merge(live::router())
        .merge(foundation::router())
        .merge(update::router())
        // Setup 向导 API 同时挂到主端口：新框架 SPA（SetupView）在主端口同源调用
        // /api/setup/*，否则未完成初始化的用户在 SPA 内全部 404。
        // 独立 setup 端口（app/setup_server.rs）仍直接引用 setup::router()，行为不变。
        // 主端口上这些接口与其它 /api/* 一致，走 enforce_request_security 的
        // 会话认证 + CSRF/Origin 校验；免认证入口始终是仅回环的独立 setup 端口。
        .merge(setup::router())
}

#[cfg(test)]
mod tests;
