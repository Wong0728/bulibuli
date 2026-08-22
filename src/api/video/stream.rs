//! 视频解析与取流：链接解析、投稿列表、视频/音频流获取、视频信息与下载权限校验。

use crate::error::{ApiResponse, AppError};
use crate::services::url_parser::{resolve_media_input, ResolvedMedia};
use crate::state::SharedState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

#[derive(Deserialize)]
pub(super) struct ResolveVideoRequest {
    input: String,
}

/// 解析用户输入的媒体链接：BV/AV/ep/ss/fp/b23.tv。
/// - BV/AV：仅返回 ResolvedMedia，前端走普通视频下载流程。
/// - ep/ss：拉番剧季信息，返回分集列表（ep 链接额外标记当前 ep_id 供前端默认勾选）。
/// - fp：拉课程季信息，返回分集列表（展平 sections）。
///
/// 番剧/课程季信息接口失败时返回 pay_blocked + 可读原因，不抛 500，便于前端友好提示。
pub(super) async fn resolve_video(
    State(state): State<SharedState>,
    Json(request): Json<ResolveVideoRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let input = request.input.trim();
    let resolved = resolve_media_input(state.bili.bili_api.client(), input).await?;

    match &resolved {
        ResolvedMedia::Episode(ep_id) => {
            let cookies = state.infra.settings_service.cookie_header().await?;
            let season = match state
                .bili
                .bili_api
                .get_pgc_season_info(Some(*ep_id), None, &cookies)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    info!("[API] /api/video/resolve 番剧季信息拉取失败 ep_id={ep_id}: {e}");
                    return Ok(Json(ApiResponse::success(json!({
                        "media": resolved,
                        "media_type": "pgc",
                        "pay_blocked": true,
                        "pay_reason": "pgc_no_permission",
                        "message": format!("番剧信息拉取失败：{e}"),
                    }))));
                }
            };
            Ok(Json(ApiResponse::success(json!({
                "media": resolved,
                "media_type": "pgc",
                "season_title": season.title(),
                "cover": season.cover,
                "current_ep_id": ep_id,
                "default_quality": state.infra.settings_service.current().query.video_quality,
                "episodes": season.episodes.iter().map(|e| json!({
                    "ep_id": e.ep_id,
                    "cid": e.cid,
                    "bvid": e.bvid,
                    "aid": e.aid,
                    "title": e.title,
                    "long_title": e.long_title,
                    "display_title": e.display_title(),
                    "duration": e.duration,
                    "badge": e.badge,
                })).collect::<Vec<_>>(),
            }))))
        }
        ResolvedMedia::Season(season_id) => {
            let cookies = state.infra.settings_service.cookie_header().await?;
            let season = match state
                .bili
                .bili_api
                .get_pgc_season_info(None, Some(*season_id), &cookies)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    info!("[API] /api/video/resolve 番剧季信息拉取失败 season_id={season_id}: {e}");
                    return Ok(Json(ApiResponse::success(json!({
                        "media": resolved,
                        "media_type": "pgc",
                        "pay_blocked": true,
                        "pay_reason": "pgc_no_permission",
                        "message": format!("番剧信息拉取失败：{e}"),
                    }))));
                }
            };
            Ok(Json(ApiResponse::success(json!({
                "media": resolved,
                "media_type": "pgc",
                "season_title": season.title(),
                "cover": season.cover,
                "default_quality": state.infra.settings_service.current().query.video_quality,
                "episodes": season.episodes.iter().map(|e| json!({
                    "ep_id": e.ep_id,
                    "cid": e.cid,
                    "bvid": e.bvid,
                    "aid": e.aid,
                    "title": e.title,
                    "long_title": e.long_title,
                    "display_title": e.display_title(),
                    "duration": e.duration,
                    "badge": e.badge,
                })).collect::<Vec<_>>(),
            }))))
        }
        ResolvedMedia::Course(season_id) => {
            let cookies = state.infra.settings_service.cookie_header().await?;
            let season = match state
                .bili
                .bili_api
                .get_cheese_season_info(Some(*season_id), None, &cookies)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    info!("[API] /api/video/resolve 课程季信息拉取失败 season_id={season_id}: {e}");
                    return Ok(Json(ApiResponse::success(json!({
                        "media": resolved,
                        "media_type": "cheese",
                        "pay_blocked": true,
                        "pay_reason": "cheese_no_permission",
                        "message": format!("课程信息拉取失败：{e}"),
                    }))));
                }
            };
            // 课程分集嵌套在 sections 中，这里展平为统一列表，并保留 section_title 供前端分组展示
            let mut episodes: Vec<Value> = Vec::new();
            for (section_title, ep) in season.flatten_episodes() {
                episodes.push(json!({
                    "ep_id": ep.ep_id,
                    "cid": ep.cid,
                    "bvid": ep.bvid,
                    "aid": ep.aid,
                    "title": ep.title,
                    "section_title": section_title,
                    "display_title": if ep.title.is_empty() { format!("ep{}", ep.ep_id) } else { ep.title.clone() },
                    "duration": ep.duration,
                    "status": ep.status,
                }));
            }
            Ok(Json(ApiResponse::success(json!({
                "media": resolved,
                "media_type": "cheese",
                "season_title": season.title,
                "cover": season.cover,
                "default_quality": state.infra.settings_service.current().query.video_quality,
                "episodes": episodes,
            }))))
        }
        // 普通视频（BV/AV）仅返回 ResolvedMedia，保持原有契约
        _ => Ok(Json(ApiResponse::success(json!({ "media": resolved })))),
    }
}

#[derive(Deserialize)]
pub(super) struct GateDownloadRequest {
    bvid: String,
}

#[derive(Deserialize)]
pub(super) struct GetVideosRequest {
    uid: String,
    limit: Option<i32>,
    offset: Option<i64>,
}

pub(super) async fn get_videos(
    State(state): State<SharedState>,
    Json(req): Json<GetVideosRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    let uid = req.uid.trim();
    if uid.is_empty() {
        info!("[API] /api/video/get-videos 请求失败: UID为空");
        return Err(AppError::BadRequest("请输入用户UID".to_string()));
    }
    let uid_int: i64 = uid.parse().map_err(|_| {
        info!(
            "[API] /api/video/get-videos 请求失败: UID格式错误 uid={}",
            uid
        );
        AppError::BadRequest("UID必须是数字".to_string())
    })?;
    crate::api::validate_bili_id("UID", uid_int)?;

    info!(
        "[API] /api/video/get-videos 请求: uid={}, limit={:?}",
        uid_int, req.limit
    );

    let default_limit = state
        .infra
        .settings_service
        .current()
        .query
        .manual_query_limit;
    let limit = req.limit.unwrap_or(default_limit);
    // 与 settings.rs 的 manual_query_limit 校验（1..=100）保持一致：
    // 此前 1..=50 会让把设置调到 51-100 的用户所有默认请求恒 400。
    if !(1..=100).contains(&limit) {
        return Err(AppError::BadRequest(
            "视频列表 limit 必须在 1 到 100 之间".to_string(),
        ));
    }
    let offset = req.offset.unwrap_or(0);
    if offset < 0 || offset % i64::from(limit) != 0 {
        return Err(AppError::BadRequest(
            "视频列表 offset 必须是非负 page size 整数倍".to_string(),
        ));
    }
    let page = i32::try_from(offset / i64::from(limit) + 1)
        .map_err(|_| AppError::BadRequest("视频列表页码超出范围".to_string()))?;

    let cookies = state.infra.settings_service.cookie_header().await?;
    let result = state
        .bili
        .bili_api
        .get_user_videos_page(uid_int, &cookies, page, limit)
        .await?;
    info!(
        "[API] /api/video/get-videos 成功: uid={}, 返回视频数={}",
        uid_int,
        result.videos.len()
    );
    Ok(Json(ApiResponse::success(serde_json::to_value(result)?)))
}

#[derive(Deserialize)]
pub(super) struct GetVideoUrlsRequest {
    bvid: String,
    fnval: Option<i32>,
    qn: Option<i32>,
    /// 多P时指定分P cid；为空则取默认（P1）cid，保持现状。
    cid: Option<i64>,
}

pub(super) async fn get_video_urls(
    State(state): State<SharedState>,
    Json(req): Json<GetVideoUrlsRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    let bvid = req.bvid.trim();
    if bvid.is_empty() {
        info!("[API] /api/video/get-video-urls 请求失败: BV号为空");
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }

    if let Some(cid) = req.cid {
        crate::api::validate_bili_id("CID", cid)?;
    }

    info!(
        "[API] /api/video/get-video-urls 请求: bvid={}, fnval={:?}",
        bvid, req.fnval
    );

    let settings = state.infra.settings_service.current();
    let fnval = req.fnval.unwrap_or(settings.query.video_format);
    crate::api::validate_fnval(fnval)?;
    let cookies = state.infra.settings_service.cookie_header().await?;
    let preferred_quality = req.qn.unwrap_or(settings.query.video_quality);
    // 与 /api/download/start（queue_ops.rs）保持同一画质白名单口径：
    // 拒绝任意值直进 playurl 参数（126/127 暂不支持）。
    if !matches!(
        preferred_quality,
        16 | 32 | 64 | 74 | 80 | 112 | 116 | 120 | 125
    ) {
        return Err(AppError::BadRequest(format!(
            "不支持的视频画质代码: {preferred_quality}"
        )));
    }
    let minimum_quality = settings.query.min_video_quality;
    let codecs = settings.query.prefer_codecs.clone();
    let allow_fallback = settings.query.allow_quality_fallback;
    let streams = state
        .bili
        .bili_api
        .get_video_urls(bvid, &cookies, fnval, Some(preferred_quality), req.cid)
        .await?;
    if streams.qualities.is_empty() {
        info!(
            "[API] /api/video/get-video-urls 失败: bvid={}, message=未找到视频流",
            bvid
        );
        return Err(AppError::BadRequest("未找到视频流".to_string()));
    }
    let selected = crate::services::bili_api::choose_video_stream(
        &streams.qualities,
        preferred_quality,
        minimum_quality,
        &codecs,
        allow_fallback,
    );
    let Some(selected) = selected else {
        info!(
            "[API] /api/video/get-video-urls 失败: bvid={}, message=没有满足最低画质与编码策略的可用流",
            bvid
        );
        return Err(AppError::BadRequest(
            "没有满足最低画质与编码策略的可用流".to_string(),
        ));
    };
    let fallback_reason = (selected.quality != preferred_quality)
        .then(|| format!("请求 {}，已降级至 {}", preferred_quality, selected.quality));

    info!(
        "[API] /api/video/get-video-urls 成功: bvid={}, 总视频流={}, 可用流={}",
        bvid,
        streams.qualities.len(),
        streams.accept_quality.len()
    );

    let mut payload = serde_json::to_value(&streams)?;
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "selected_quality".to_string(),
            serde_json::to_value(&selected)?,
        );
        object.insert(
            "fallback_reason".to_string(),
            fallback_reason.map_or(Value::Null, Value::String),
        );
        // 抽屉默认画质：来自设置 query.video_quality（默认 80/1080P），
        // 前端以 `default_quality ?? 80` 兜底，保证"用户改了设置就按用户设置"。
        object.insert(
            "default_quality".to_string(),
            serde_json::json!(settings.query.video_quality),
        );
    }
    Ok(Json(ApiResponse::success(payload)))
}

#[derive(Deserialize)]
pub(super) struct GetAudioUrlRequest {
    bvid: String,
}

pub(super) async fn get_audio_url(
    State(state): State<SharedState>,
    Json(req): Json<GetAudioUrlRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = req.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }
    let cookies = state.infra.settings_service.cookie_header().await?;
    let preference = state
        .infra
        .settings_service
        .current()
        .query
        .audio_quality_preference
        .as_str()
        .to_string();
    let audio = state
        .bili
        .bili_api
        .get_audio_url(bvid, None, &cookies, &preference)
        .await?
        .ok_or_else(|| AppError::NotFound("未找到音频流".to_string()))?;
    Ok(Json(ApiResponse::success(serde_json::to_value(audio)?)))
}

#[derive(Deserialize)]
pub(super) struct VideoInfoQuery {
    bvid: String,
}

/// 视频信息接口（抽屉"刷新数据"用）。
/// 只返回前端需要的字段，**不入库**。带 5min 内存缓存防止狂点触发风控。
pub(super) async fn get_video_info(
    State(state): State<SharedState>,
    axum::extract::Query(q): axum::extract::Query<VideoInfoQuery>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let bvid = q.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }

    let cookies = state.infra.settings_service.cookie_header().await?;
    let info = state.bili.bili_api.get_video_info(bvid, &cookies).await?;

    // 只返回前端需要的字段（pages 供多P多选下载展示）
    let result = json!({
        "bvid": bvid,
        "title": info.title,
        "pic": info.pic,
        "duration": info.duration,
        "pub_timestamp": info.created,
        "owner": info.owner,
        "stat": info.stat,
        "state": info.state,
        "rights": info.rights,
        "pages": info.pages,
        "default_quality": state.infra.settings_service.current().query.video_quality,
    });

    Ok(Json(ApiResponse::success(result)))
}

pub(super) async fn gate_download(
    State(state): State<SharedState>,
    Json(req): Json<GateDownloadRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    use tracing::info;

    let bvid = req.bvid.trim();
    if bvid.is_empty() {
        return Err(AppError::BadRequest("请提供视频BV号".to_string()));
    }

    info!("[API] /api/video/gate-download 请求: bvid={}", bvid);

    let cookies = state.infra.settings_service.cookie_header().await?;
    // 手动下载入口：skip_charge=false，已充电的充电专属视频允许直接下载（设置项仅约束自动下载）
    match state
        .business
        .monitor_service
        .gate_download(bvid, "", &cookies, false)
        .await
    {
        Ok(()) => Ok(Json(ApiResponse::success(json!({
            "allow": true,
            "state": "ok",
            "pay_note": null,
            "message": "可以下载",
        })))),
        Err(reason) => {
            // 未知原因视为瞬时错误：不能用 removed 兜底（前端会显示"已下架"）
            let (pay_state, pay_note) = crate::services::monitor::pay_reason_to_state(&reason)
                .unwrap_or(("error", "gate_failed"));
            let allow = matches!(pay_note, "ugc_pay_paid" | "pay_paid" | "upower_paid");
            info!(
                "[API] /api/video/gate-download 拦截: bvid={}, reason={}",
                bvid, reason
            );
            Ok(Json(ApiResponse::success(json!({
                "allow": allow,
                "state": pay_state,
                "pay_note": pay_note,
                "message": reason,
            }))))
        }
    }
}
