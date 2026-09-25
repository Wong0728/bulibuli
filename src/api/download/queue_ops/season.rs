//! 番剧/课程分集批量下载：复用分 P 任务模型，取流分别走 pgc / cheese 客户端。

use super::common::outcome_to_response;
use super::StartDownloadRequest;
use crate::error::{ApiResponse, AppError};
use crate::services::download::PageInfo;
use crate::state::SharedState;
use axum::Json;
use serde_json::{json, Value};
use tracing::error;

/// 番剧/课程分集下载：复用分 P 任务模型（每集当分P），取流走 pgc/cheese 客户端。
/// 每集携带独立 bvid（番剧每集有独立 bvid），任务主标题用季标题，part_title 用分集标题。
/// 取流失败（大会员专享 / 未购买）时单集跳过不阻断其余，最终汇总成功数；全部失败时返回可读错误。
pub(crate) async fn start_download_season(
    state: &SharedState,
    cookies: &str,
    qn: i32,
    fnval: i32,
    media_type: &str,
    req: &StartDownloadRequest,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    let season_title = req.season_title.as_deref().unwrap_or("").to_string();
    let pages = &req.pages;
    if pages.is_empty() {
        return Err(AppError::BadRequest(
            "番剧/课程下载必须携带分集列表".to_string(),
        ));
    }
    let is_multi = pages.len() > 1;

    let mut ok_count = 0usize;
    let mut last_outcome: Option<crate::services::download::TaskOutcome> = None;
    let mut first_error: Option<String> = None;

    for (idx, p) in pages.iter().enumerate() {
        let ep_id = match p.ep_id {
            Some(v) => v,
            None => {
                if first_error.is_none() {
                    first_error = Some(format!("第 {} 集缺少 ep_id", idx + 1));
                }
                continue;
            }
        };
        let ep_bvid = p.bvid.as_deref().unwrap_or(req.bvid.as_str());
        if ep_bvid.is_empty() {
            if first_error.is_none() {
                first_error = Some(format!("第 {} 集缺少 bvid", idx + 1));
            }
            continue;
        }

        // 取流：番剧走 pgc，课程走 cheese
        let streams = if media_type == "pgc" {
            state
                .bili
                .bili_api
                .get_pgc_video_urls(ep_id, p.cid, cookies, fnval, Some(qn))
                .await
        } else {
            // 课程需要 aid（avid）；缺失时直接报错
            let aid = p.aid.unwrap_or(0);
            if aid == 0 {
                if first_error.is_none() {
                    first_error = Some(format!("第 {} 集缺少 aid", idx + 1));
                }
                continue;
            }
            state
                .bili
                .bili_api
                .get_cheese_video_urls(ep_id, aid, p.cid, cookies, Some(qn))
                .await
        };

        let streams = match streams {
            Ok(s) if !s.qualities.is_empty() => s,
            other => {
                let msg = match other {
                    Ok(_) => "未找到视频流（可能需要大会员或未购买）".to_string(),
                    Err(e) => e.to_string(),
                };
                if first_error.is_none() {
                    first_error = Some(msg.clone());
                }
                error!(
                    "/api/download/start 番剧/课程取流失败 media_type={media_type} ep_id={ep_id} cid={}: {msg}",
                    p.cid
                );
                continue;
            }
        };

        // 选流：≤ qn 的最高质量
        let selected = streams
            .qualities
            .iter()
            .filter(|q| q.quality <= qn)
            .max_by_key(|q| q.quality)
            .or_else(|| streams.qualities.first());
        let Some(selected) = selected else {
            if first_error.is_none() {
                first_error = Some("该分集无可用的视频流".to_string());
            }
            continue;
        };
        let url = selected.url.clone();
        let actual_quality = selected.quality;
        if url.is_empty() {
            if first_error.is_none() {
                first_error = Some("获取分集下载链接失败".to_string());
            }
            continue;
        }

        // 任务主标题：季标题；分集标题作为 part_title（驱动文件命名 {part} 变量）
        let task_title = if season_title.is_empty() {
            ep_bvid.to_string()
        } else {
            season_title.clone()
        };
        let page_info = PageInfo {
            cid: p.cid,
            page: p.page,
            part_title: p.part.clone(),
        };

        let outcome = state
            .media
            .download_manager
            .add_task(
                ep_bvid,
                &task_title,
                &url,
                cookies,
                actual_quality,
                "video",
                req.uid.as_deref(),
                "manual",
                Some(&page_info),
                None,
            )
            .await
            .map_err(|e| {
                error!(
                    "/api/download/start 番剧/课程任务入队失败 media_type={media_type} ep_id={ep_id}: {e}"
                );
                AppError::from(e)
            })?;
        if outcome.ok {
            ok_count += 1;
        } else if first_error.is_none() {
            first_error = Some(outcome.message.clone());
        }
        last_outcome = Some(outcome);
    }

    if ok_count == 0 {
        let msg = first_error.unwrap_or_else(|| "全部分集取流失败，未创建任务".to_string());
        info!("[API] /api/download/start 番剧/课程全部失败 media_type={media_type}: {msg}");
        return Err(AppError::NotFound(format!("番剧/课程下载失败：{msg}")));
    }

    if is_multi {
        info!(
            "[API] /api/download/start 番剧/课程完成: media_type={media_type}, 成功 {}/{}",
            ok_count,
            pages.len()
        );
        return Ok(Json(ApiResponse::with_message(
            json!({ "ok_count": ok_count, "total": pages.len() }),
            format!("已提交 {}/{} 个分集下载", ok_count, pages.len()),
        )));
    }

    let outcome = last_outcome
        .ok_or_else(|| AppError::NotFound("番剧/课程下载失败：未创建任务".to_string()))?;
    if outcome.ok {
        info!(
            "[API] /api/download/start 番剧/课程成功: media_type={media_type}, ep_id={:?}",
            pages[0].ep_id
        );
    }
    outcome_to_response(outcome)
}
