//! 下载 API：任务队列操作（`queue_ops`）、烧录任务（`burn`）与代理下载（`proxy`）。

mod burn;
mod proxy;
mod queue_ops;

pub use proxy::is_allowed_proxy_url;

use crate::state::SharedState;
use axum::{
    routing::{get, post},
    Router,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/download/add", post(queue_ops::add_download))
        .route("/api/download/start", post(queue_ops::start_download))
        .route("/api/download/retry", post(queue_ops::retry_download))
        .route("/api/download/retry-all", post(queue_ops::retry_all))
        .route("/api/download/remove", post(queue_ops::remove_download))
        .route("/api/download/status", get(queue_ops::get_status))
        .route("/api/download/health", get(queue_ops::get_health))
        .route("/api/download/metrics", get(queue_ops::queue_metrics))
        .route("/api/download/priority", post(queue_ops::set_priority))
        .route("/api/download/pause", post(queue_ops::pause_download))
        .route("/api/download/resume", post(queue_ops::resume_download))
        .route("/api/download/burn", post(burn::burn))
        .route(
            "/api/download/burn/status/{task_id}",
            get(burn::burn_status),
        )
        .route("/api/download/proxy", get(proxy::download_proxy))
}
