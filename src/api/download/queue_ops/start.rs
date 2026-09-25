//! 一键启动下载接口（`POST /api/download/start`）：普通视频单任务与多P逐分P入队。

use super::common::outcome_to_response;
use super::StartDownloadRequest;
use crate::error::{ApiResponse, AppError};
use crate::services::download::{PageInfo, TaskOutcome};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde_json::{json, Value};
use tracing::error;

fn ensure_multi_page_started(
    ok_count: usize,
    first_failure: Option<AppError>,
) -> Result<(), AppError> {
    if ok_count == 0 {
        return Err(first_failure
            .unwrap_or_else(|| AppError::NotFound("全部分P取流失败，未创建任务".to_string())));
    }
    Ok(())
}

/// 一键启动下载：前端只需传 bvid + 期望质量，后端自动解析视频 URL 并添加任务。
/// 若携带 pages，则逐个分P解析 cid 取流并独立入队。
/// 若 media_type=pgc/cheese，则走番剧/课程取流路径，每个 PageSelector 必须携带 ep_id。
pub(crate) async fn start_download(
    State(state): State<SharedState>,
    Json(req): Json<StartDownloadRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    let bvid = req.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供BV号".to_string()));
    }
    let cookies = state.infra.settings_service.cookie_header().await?;
    let qn = req.qn.unwrap_or(80);
    // B 站画质 code 为有限集合（16/32/64/74/80/112/116/120/125），
    // 拒绝任意值直进 playurl 参数（负数/超大值会原样透传给上游）。
    if !matches!(qn, 16 | 32 | 64 | 74 | 80 | 112 | 116 | 120 | 125) {
        return Err(AppError::BadRequest(format!("不支持的视频画质代码: {qn}")));
    }
    let default_fnval = state.infra.settings_service.current().query.video_format;
    let media_type = req.media_type.as_deref().unwrap_or("video");

    info!(
        "[API] /api/download/start 请求: bvid={}, qn={}, pages={}, media_type={}",
        bvid,
        qn,
        req.pages.len(),
        media_type
    );

    // 番剧/课程：复用分 P 机制，每集当作一个"分P"入队，但取流走 pgc/cheese 客户端
    if media_type == "pgc" || media_type == "cheese" {
        return super::season::start_download_season(
            &state,
            &cookies,
            qn,
            default_fnval,
            media_type,
            &req,
        )
        .await;
    }

    // 普通视频路径：先获取视频标题（单/多P 共用）。
    let info = state
        .bili
        .bili_api
        .get_video_info(bvid, &cookies)
        .await
        .map_err(|e| {
            error!("/api/download/start 获取视频信息失败 bvid={bvid}: {e}");
            AppError::from(e)
        })?;
    let title = if info.title.is_empty() {
        bvid.to_string()
    } else {
        info.title.clone()
    };

    // 多P：前端传来所选分P列表时逐个下载；否则单任务（默认 cid，page=None）保持现状。
    let pages: Vec<Option<PageInfo>> = if req.pages.is_empty() {
        vec![None]
    } else {
        req.pages
            .iter()
            .map(|p| {
                Some(PageInfo {
                    cid: p.cid,
                    page: p.page,
                    part_title: p.part.clone(),
                })
            })
            .collect()
    };
    let is_multi = pages.len() > 1;

    let mut ok_count = 0usize;
    let mut last_outcome: Option<TaskOutcome> = None;
    let mut first_failure: Option<AppError> = None;
    for page in &pages {
        let cid = page.as_ref().map(|p| p.cid);
        // 1. 获取分P视频流
        let streams_result = state
            .bili
            .bili_api
            .get_video_urls(bvid, &cookies, default_fnval, Some(qn), cid)
            .await;
        let streams = match streams_result {
            Ok(streams) if !streams.qualities.is_empty() => streams,
            Ok(_) => {
                let failure = AppError::NotFound("未找到视频流".to_string());
                error!(
                    bvid,
                    ?cid,
                    stage = "playurl-empty",
                    error = %failure,
                    "/api/download/start 取流失败"
                );
                if is_multi {
                    first_failure.get_or_insert(failure);
                    continue;
                }
                return Err(failure);
            }
            Err(error) => {
                let failure = AppError::from(error);
                error!(
                    bvid,
                    ?cid,
                    stage = "playurl-request",
                    error = %failure,
                    "/api/download/start 取流失败"
                );
                if is_multi {
                    first_failure.get_or_insert(failure);
                    continue;
                }
                return Err(failure);
            }
        };

        // 2. 从 qualities 中选择最佳匹配（≤ qn 的最高质量）
        let selected = streams
            .qualities
            .iter()
            .filter(|q| q.quality <= qn)
            .max_by_key(|q| q.quality)
            .or_else(|| streams.qualities.first());
        let Some(selected) = selected else {
            if is_multi {
                continue;
            }
            return Err(AppError::NotFound("该视频无可用的视频流".to_string()));
        };
        let url = selected.url.clone();
        let actual_quality = selected.quality;
        if url.is_empty() {
            if is_multi {
                continue;
            }
            return Err(AppError::NotFound("获取视频下载链接失败".to_string()));
        }

        // 3. 添加下载任务（手动入口，标记 source=manual）
        let outcome = state
            .media
            .download_manager
            .add_task(
                bvid,
                &title,
                &url,
                &cookies,
                actual_quality,
                "video",
                req.uid.as_deref(),
                "manual",
                page.as_ref(),
                None,
            )
            .await
            .map_err(|e| {
                error!("/api/download/start 添加下载任务失败 bvid={bvid}: {e}");
                AppError::from(e)
            })?;
        if outcome.ok {
            ok_count += 1;
        }
        last_outcome = Some(outcome);
    }

    if is_multi {
        ensure_multi_page_started(ok_count, first_failure)?;
        info!(
            "[API] /api/download/start 多P完成: bvid={}, 成功 {}/{}",
            bvid,
            ok_count,
            pages.len()
        );
        return Ok(Json(ApiResponse::with_message(
            json!({ "ok_count": ok_count, "total": pages.len() }),
            format!("已提交 {}/{} 个分P下载", ok_count, pages.len()),
        )));
    }

    let outcome =
        last_outcome.ok_or_else(|| AppError::NotFound("获取视频下载链接失败".to_string()))?;
    if outcome.ok {
        info!("[API] /api/download/start 成功: bvid={}", bvid);
    } else {
        info!(
            "[API] /api/download/start 拒绝: bvid={}, message={}",
            bvid, outcome.message
        );
    }
    outcome_to_response(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_multi_page_failures_return_an_error() {
        let error = ensure_multi_page_started(
            0,
            Some(AppError::Upstream("playurl parse failed".to_string())),
        )
        .expect_err("zero successful pages must fail");
        assert!(matches!(error, AppError::Upstream(_)));
    }

    #[test]
    fn partial_multi_page_success_is_accepted() {
        ensure_multi_page_started(1, Some(AppError::NotFound("one page missing".to_string())))
            .expect("at least one successful page is a partial success");
    }
}
