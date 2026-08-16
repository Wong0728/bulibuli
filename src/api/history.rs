//! 下载历史与看板 API。
//!
//! 路由：
//! - `GET /api/history/list?tab=downloading|completed|failed`：按博主分组返回看板数据。
//! - `GET /api/history/list?bvid=...`：返回单个视频的详情（抽屉用）。
//! - `GET /api/history/by-uid?uid=...`：返回某博主的历史记录。
//! - `POST /api/history/delete`：删除历史记录。
//! - `GET /api/history/search?keyword=...`：搜索历史记录。
//! - `GET /api/history/file-download?bvid=...&path=...`：抽屉浏览器下载产物文件。

mod board;
mod crud;
mod file_download;

use crate::state::SharedState;
use axum::{
    routing::{get, post},
    Router,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/history/list", get(board::list_history))
        .route("/api/history/by-uid", get(crud::get_by_uid))
        .route("/api/history/delete", post(crud::delete_history))
        .route("/api/history/open-directory", post(crud::open_directory))
        .route("/api/history/search", get(crud::search_history))
        .route(
            "/api/history/file-download",
            get(file_download::download_history_file),
        )
}
