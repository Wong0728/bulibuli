//! 下载任务队列接口：添加/一键启动/重试/移除/状态查询/优先级/打开目录。

use crate::error::{ApiResponse, AppError};
use crate::services::download::{PageInfo, TaskOutcome};
use crate::state::SharedState;
use axum::{extract::Query, extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::error;

/// 把任务入队/重试/移除的强类型结果转成前端信封。
/// `ok=false` 是业务拒绝（重复任务、退避中、非法参数），映射为 400 让前端走失败提示，
/// 与旧 `{success:false}` 契约经中间件归一为 code=400 的行为一致。
fn outcome_to_response(outcome: TaskOutcome) -> Result<Json<ApiResponse<Value>>, AppError> {
    if outcome.ok {
        Ok(Json(ApiResponse::with_message(
            json!({ "download_id": outcome.download_id }),
            outcome.message,
        )))
    } else {
        Err(AppError::BadRequest(outcome.message))
    }
}

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

pub(super) async fn queue_metrics(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    Ok(Json(ApiResponse::success(
        state.media.download_manager.queue_metrics().await?,
    )))
}

pub(super) async fn get_health(
    State(state): State<SharedState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    Ok(Json(ApiResponse::success(
        state.media.download_manager.get_health().await,
    )))
}

#[derive(Deserialize)]
pub(super) struct PriorityRequest {
    bvid: String,
    #[serde(rename = "type")]
    task_type: String,
    priority: i32,
}

pub(super) async fn set_priority(
    State(state): State<SharedState>,
    Json(request): Json<PriorityRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let result = state
        .media
        .download_manager
        .set_priority(&request.bvid, &request.task_type, request.priority)
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    Ok(Json(ApiResponse::success(result)))
}

#[derive(Deserialize)]
pub(super) struct AddDownloadRequest {
    bvid: String,
    title: String,
    url: String,
    quality: Option<i32>,
    #[serde(rename = "type")]
    task_type: Option<String>,
}

pub(super) async fn add_download(
    State(state): State<SharedState>,
    Json(req): Json<AddDownloadRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    let bvid = req.bvid.trim();
    let title = req.title.trim();
    let url = req.url.trim();

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
    crate::services::bili_url_policy::validate(url).await?;
    let cookies = state.infra.settings_service.cookie_header().await?;
    let default_quality = state.infra.settings_service.current().query.video_quality;
    let quality = req.quality.unwrap_or(default_quality);
    let task_type = req.task_type.unwrap_or_else(|| "video".to_string());
    let outcome = state
        .media
        .download_manager
        .add_task(
            bvid, title, url, &cookies, quality, &task_type, None, "manual", None, None,
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

#[derive(Deserialize)]
pub(super) struct PageSelector {
    cid: i64,
    page: i32,
    #[serde(default)]
    part: String,
    /// 番剧/课程分集专用：ep_id（番剧）或 ep_id（课程，对应 B 站 id 字段）。
    /// 普通视频分 P 不需要传。
    #[serde(default)]
    ep_id: Option<u64>,
    /// 番剧/课程分集专用：该集对应的 bvid（番剧每集有独立 bvid）。
    /// 普通视频分 P 不需要传，沿用顶层 bvid。
    #[serde(default)]
    bvid: Option<String>,
    /// 课程分集专用：avid（课程 playurl 接口要求 avid 而非 bvid）。
    #[serde(default)]
    aid: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct StartDownloadRequest {
    bvid: String,
    qn: Option<i32>,
    uid: Option<String>,
    /// 多P视频时前端携带所选分P列表；为空时下默认 cid（单任务，保持现状）。
    #[serde(default)]
    pages: Vec<PageSelector>,
    /// 媒体类型：缺省/"video" 走普通视频取流；"pgc" 走番剧取流；"cheese" 走课程取流。
    /// 番剧/课程分集下载时，每个 PageSelector 携带 ep_id + bvid（+ aid 用于课程）。
    #[serde(default)]
    media_type: Option<String>,
    /// 番剧/课程下载时由前端透传的季标题（用作任务主标题，避免逐集再查一次）。
    #[serde(default)]
    season_title: Option<String>,
}

/// 一键启动下载：前端只需传 bvid + 期望质量，后端自动解析视频 URL 并添加任务。
/// 若携带 pages，则逐个分P解析 cid 取流并独立入队。
/// 若 media_type=pgc/cheese，则走番剧/课程取流路径，每个 PageSelector 必须携带 ep_id。
pub(super) async fn start_download(
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
        return start_download_season(&state, &cookies, qn, default_fnval, media_type, &req).await;
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

/// 番剧/课程分集下载：复用分 P 任务模型（每集当分P），取流走 pgc/cheese 客户端。
/// 每集携带独立 bvid（番剧每集有独立 bvid），任务主标题用季标题，part_title 用分集标题。
/// 取流失败（大会员专享 / 未购买）时单集跳过不阻断其余，最终汇总成功数；全部失败时返回可读错误。
async fn start_download_season(
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
    let mut last_outcome: Option<TaskOutcome> = None;
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

#[derive(Deserialize)]
pub(super) struct RetryDownloadRequest {
    bvid: String,
    #[serde(rename = "type")]
    task_type: String,
}

pub(super) async fn retry_download(
    State(state): State<SharedState>,
    Json(req): Json<RetryDownloadRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!(
        "[API] /api/download/retry 请求: bvid={}, type={}",
        req.bvid, req.task_type
    );

    let outcome = state
        .media
        .download_manager
        .retry_task(&req.bvid, &req.task_type)
        .await?;

    if outcome.ok {
        info!(
            "[API] /api/download/retry 成功: bvid={}, type={}",
            req.bvid, req.task_type
        );
    } else {
        info!(
            "[API] /api/download/retry 拒绝: bvid={}, type={}, message={}",
            req.bvid, req.task_type, outcome.message
        );
    }

    outcome_to_response(outcome)
}

#[derive(Deserialize)]
pub(super) struct RetryAllQuery {
    since: Option<i64>,
}

pub(super) async fn retry_all(
    State(state): State<SharedState>,
    Query(q): Query<RetryAllQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!("[API] /api/download/retry-all 请求 since={:?}", q.since);
    let outcome = state
        .media
        .download_manager
        .retry_all_failed(q.since)
        .await?;

    info!(
        "[API] /api/download/retry-all 完成: message={}",
        outcome.message
    );

    outcome_to_response(outcome)
}

#[derive(Deserialize)]
pub(super) struct RemoveDownloadRequest {
    bvid: String,
    #[serde(rename = "type")]
    task_type: String,
}

pub(super) async fn remove_download(
    State(state): State<SharedState>,
    Json(req): Json<RemoveDownloadRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!(
        "[API] /api/download/remove 请求: bvid={}, type={}",
        req.bvid, req.task_type
    );

    let outcome = state
        .media
        .download_manager
        .remove_task(&req.bvid, &req.task_type)
        .await?;

    info!(
        "[API] /api/download/remove 完成: bvid={}, type={}",
        req.bvid, req.task_type
    );

    outcome_to_response(outcome)
}

#[derive(Deserialize)]
pub(super) struct StatusQuery {
    uid: Option<String>,
}

pub(super) async fn get_status(
    State(state): State<SharedState>,
    Query(q): Query<StatusQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    Ok(Json(ApiResponse::success(
        state
            .media
            .download_manager
            .get_status(q.uid.as_deref())
            .await?,
    )))
}

/// 暂停/恢复请求体：`task_id` 缺省时表示全局操作。
/// 与 retry/remove 不同，pause/resume 用 task_id 而非 (bvid, task_type)，
/// 因为单次操作只针对一个任务行（不展开多 P）。
#[derive(Deserialize)]
pub(super) struct PauseResumeRequest {
    pub task_id: Option<i32>,
}

pub(super) async fn pause_download(
    State(state): State<SharedState>,
    Json(req): Json<PauseResumeRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!("[API] /api/download/pause 请求: task_id={:?}", req.task_id);
    let outcome = state
        .media
        .download_manager
        .pause_task(req.task_id)
        .await
        .map_err(|e| {
            error!("/api/download/pause 暂停下载任务失败: {e}");
            AppError::from(e)
        })?;
    if outcome.ok {
        info!("[API] /api/download/pause 完成: {}", outcome.message);
    } else {
        info!("[API] /api/download/pause 拒绝: {}", outcome.message);
    }
    outcome_to_response(outcome)
}

pub(super) async fn resume_download(
    State(state): State<SharedState>,
    Json(req): Json<PauseResumeRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    info!("[API] /api/download/resume 请求: task_id={:?}", req.task_id);
    let outcome = state
        .media
        .download_manager
        .resume_task(req.task_id)
        .await
        .map_err(|e| {
            error!("/api/download/resume 恢复下载任务失败: {e}");
            AppError::from(e)
        })?;
    if outcome.ok {
        info!("[API] /api/download/resume 完成: {}", outcome.message);
    } else {
        info!("[API] /api/download/resume 拒绝: {}", outcome.message);
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
