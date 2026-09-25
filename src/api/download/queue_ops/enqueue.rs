//! 手动添加下载任务接口（`POST /api/download/add`）。

use super::common::outcome_to_response;
use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::Value;
use tracing::error;

#[derive(Deserialize)]
pub(crate) struct AddDownloadRequest {
    bvid: String,
    /// 可选：缺省或为空时自动通过 get_video_info 取标题（与 /api/download/start 同口径）。
    #[serde(default)]
    title: Option<String>,
    url: String,
    quality: Option<i32>,
    #[serde(rename = "type")]
    task_type: Option<String>,
}

pub(crate) async fn add_download(
    State(state): State<SharedState>,
    Json(req): Json<AddDownloadRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    let bvid = req.bvid.trim();
    let url = req.url.trim();
    let provided_title = req.title.unwrap_or_default();
    let title = provided_title.trim();

    info!(
        "[API] /api/download/add 请求: bvid={}, title={}, type={:?}, quality={:?}",
        bvid, title, req.task_type, req.quality
    );

    if bvid.is_empty() || url.is_empty() {
        info!(
            "[API] /api/download/add 请求失败: bvid或url为空 bvid={}",
            bvid
        );
        return Err(AppError::BadRequest("请提供BV号和下载链接".to_string()));
    }
    crate::services::bili_url_policy::validate_download_url(url).await?;
    let cookies = state.infra.settings_service.cookie_header().await?;
    // 未提供标题时自动补全：直接 POST {"bvid": ...} 不再因 missing field title 失败。
    let title = if title.is_empty() {
        let info = state
            .bili
            .bili_api
            .get_video_info(bvid, &cookies)
            .await
            .map_err(|e| {
                error!("/api/download/add 获取视频信息失败 bvid={bvid}: {e}");
                AppError::from(e)
            })?;
        if info.title.is_empty() {
            bvid.to_string()
        } else {
            info.title.clone()
        }
    } else {
        title.to_string()
    };
    let default_quality = state.infra.settings_service.current().query.video_quality;
    let quality = req.quality.unwrap_or(default_quality);
    let task_type = req.task_type.unwrap_or_else(|| "video".to_string());
    let outcome = state
        .media
        .download_manager
        .add_task(
            bvid, &title, url, &cookies, quality, &task_type, None, "manual", None, None,
        )
        .await
        .map_err(|e| {
            error!("/api/download/add 添加下载任务失败 bvid={bvid} type={task_type}: {e}");
            AppError::from(e)
        })?;

    if outcome.ok {
        info!(
            "[API] /api/download/add 成功: bvid={}, type={}",
            bvid, task_type
        );
    } else {
        info!(
            "[API] /api/download/add 拒绝: bvid={}, type={}, message={}",
            bvid, task_type, outcome.message
        );
    }
    outcome_to_response(outcome)
}
