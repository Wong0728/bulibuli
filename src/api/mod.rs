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

use crate::state::SharedState;
use axum::Router;

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
