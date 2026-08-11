pub(crate) mod auth;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_positive_ids_and_fnval() {
        assert!(validate_bili_id("UID", 1).is_ok());
        assert!(validate_bili_id("UID", 0).is_err());
        assert!(validate_bili_id("UID", MAX_BILI_ID + 1).is_err());
        assert!(validate_fnval(4048).is_ok());
        assert!(validate_fnval(-1).is_err());
    }
}
