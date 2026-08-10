//! 博主 API：监控列表管理（`manage`）与 B 站侧检索/合集查询（`discover`）。

mod actions;
mod discover;
mod manage;

use crate::state::SharedState;
use axum::{
    routing::{get, post},
    Router,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/blogger/list", get(manage::list_bloggers))
        .route("/api/blogger/add", post(manage::add_blogger))
        .route("/api/blogger/update", post(manage::update_blogger))
        .route("/api/blogger/delete", post(manage::delete_blogger))
        .route("/api/blogger/saved/list", get(manage::list_saved_bloggers))
        .route("/api/blogger/saved/add", post(manage::add_saved_blogger))
        .route(
            "/api/blogger/saved/delete",
            post(manage::delete_saved_blogger),
        )
        .route("/api/blogger/search", get(discover::search_bloggers))
        .route("/api/blogger/validate-uid", get(discover::validate_uid))
        .route("/api/blogger/cleanup-now", post(actions::cleanup_now))
        .route(
            "/api/blogger/acknowledge",
            post(actions::acknowledge_profile_change),
        )
        .route(
            "/api/blogger/acknowledge-batch",
            post(manage::acknowledge_profile_changes),
        )
        .route("/api/blogger/series", get(discover::get_series))
        .route(
            "/api/blogger/series-videos",
            get(discover::get_series_videos),
        )
}
