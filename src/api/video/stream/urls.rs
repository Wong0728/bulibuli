//! 视频取流接口：视频/音频流获取、视频信息查询与下载权限门禁。

use crate::error::{ApiResponse, AppError};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub(crate) struct GetVideoUrlsRequest {
    bvid: String,
    fnval: Option<i32>,
    qn: Option<i32>,
    /// 多P时指定分P cid；为空则取默认（P1）cid，保持现状。
    cid: Option<i64>,
}

pub(crate) async fn get_video_urls(
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
pub(crate) struct GetAudioUrlRequest {
    bvid: String,
}

pub(crate) async fn get_audio_url(
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
pub(crate) struct VideoInfoQuery {
    bvid: String,
}

/// 视频信息接口（抽屉"刷新数据"用）。
/// 只返回前端需要的字段，**不入库**。带 5min 内存缓存防止狂点触发风控。
pub(crate) async fn get_video_info(
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

#[derive(Deserialize)]
pub(crate) struct GateDownloadRequest {
    bvid: String,
}

pub(crate) async fn gate_download(
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
