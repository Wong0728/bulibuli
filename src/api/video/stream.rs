//! 视频解析与取流：链接解析、投稿列表、视频/音频流获取、视频信息与下载权限校验。

use crate::error::{ApiResponse, AppError};
use crate::models::history;
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

/// 查询已下载分集的 bvid 集合：对照 history 中存在完成时间的记录。
/// 分块查询：SQLite 默认变量上限 999，长季（上千集）一次 is_in 会超限。
async fn downloaded_bvids(
    db: &sea_orm::DatabaseConnection,
    bvids: &[String],
) -> std::collections::HashSet<String> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    const CHUNK_SIZE: usize = 400;
    let mut downloaded = std::collections::HashSet::new();
    for chunk in bvids.chunks(CHUNK_SIZE) {
        let rows = history::Entity::find()
            .filter(history::Column::Bvid.is_in(chunk.to_vec()))
            .filter(history::Column::DownloadTime.is_not_null())
            .all(db)
            .await;
        match rows {
            Ok(rows) => downloaded.extend(rows.into_iter().map(|h| h.bvid)),
            Err(e) => {
                // 查询失败不阻塞解析：downloaded 标记缺失只影响默认勾选体验
                info!("[API] /api/video/resolve 查询已下载分集失败: {e}");
                return downloaded;
            }
        }
    }
    downloaded
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
            let episode_bvids: Vec<String> =
                season.episodes.iter().map(|e| e.bvid.clone()).collect();
            let downloaded = downloaded_bvids(&state.infra.db, &episode_bvids).await;
            let downloaded_count = season
                .episodes
                .iter()
                .filter(|e| downloaded.contains(&e.bvid))
                .count();
            Ok(Json(ApiResponse::success(json!({
                "media": resolved,
                "media_type": "pgc",
                "season_title": season.title(),
                "cover": season.cover,
                "current_ep_id": ep_id,
                "default_quality": state.infra.settings_service.current().query.video_quality,
                "downloaded_count": downloaded_count,
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
                    "downloaded": downloaded.contains(&e.bvid),
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
            let episode_bvids: Vec<String> =
                season.episodes.iter().map(|e| e.bvid.clone()).collect();
            let downloaded = downloaded_bvids(&state.infra.db, &episode_bvids).await;
            let downloaded_count = season
                .episodes
                .iter()
                .filter(|e| downloaded.contains(&e.bvid))
                .count();
            Ok(Json(ApiResponse::success(json!({
                "media": resolved,
                "media_type": "pgc",
                "season_title": season.title(),
                "cover": season.cover,
                "default_quality": state.infra.settings_service.current().query.video_quality,
                "downloaded_count": downloaded_count,
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
                    "downloaded": downloaded.contains(&e.bvid),
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
            let flat_episodes = season.flatten_episodes();
            let mut episodes: Vec<Value> = Vec::new();
            let episode_bvids: Vec<String> =
                flat_episodes.iter().map(|(_, e)| e.bvid.clone()).collect();
            let downloaded = downloaded_bvids(&state.infra.db, &episode_bvids).await;
            let downloaded_count = downloaded.len();
            for (section_title, ep) in flat_episodes {
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
                    "downloaded": downloaded.contains(&ep.bvid),
                }));
            }
            Ok(Json(ApiResponse::success(json!({
                "media": resolved,
                "media_type": "cheese",
                "season_title": season.title,
                "cover": season.cover,
                "default_quality": state.infra.settings_service.current().query.video_quality,
                "downloaded_count": downloaded_count,
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

/// B 站 `space/wbi/arc/search` 的单页上限。`bili_api::user_space::get_user_videos_page`
/// 内部把 `ps` 钳制到 `1..=50` 并原样回写 `page_size`/`offset`，API 层换算页码时
/// 必须用同一个有效页大小：否则 limit 落在 51..=100 时会"按 limit 算页码、按 50 取
/// 窗口"，相邻两次请求的窗口既重叠又漏项，且客户端回显服务端 `offset` 后再叠加自己的
/// limit 会撞上 `offset % limit != 0` 的 400，"加载更多"就此卡死。
const MAX_PAGE_SIZE: i32 = 50;

/// 把客户端的 `(limit, offset)` 换算成 B 站的 `(pn, ps)`。
///
/// 返回 `(pn, ps)`，其中 `ps = min(limit, MAX_PAGE_SIZE)` 就是真正发出去的页大小，
/// 与响应体里的 `page_size` 一致，调用方按 `ps` 递增 `offset` 即可无重叠无漏项地翻页。
fn resolve_page_window(limit: i32, offset: i64) -> Result<(i32, i32), String> {
    // 与 settings.rs 的 manual_query_limit 校验（1..=100）保持一致：
    // 此前 1..=50 会让把设置调到 51-100 的用户所有默认请求恒 400。
    if !(1..=100).contains(&limit) {
        return Err("视频列表 limit 必须在 1 到 100 之间".to_string());
    }
    let page_size = limit.min(MAX_PAGE_SIZE);
    if offset < 0 || offset % i64::from(page_size) != 0 {
        return Err("视频列表 offset 必须是非负 page size 整数倍".to_string());
    }
    let page = i32::try_from(offset / i64::from(page_size) + 1)
        .map_err(|_| "视频列表页码超出范围".to_string())?;
    Ok((page, page_size))
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
    let offset = req.offset.unwrap_or(0);
    let (page, page_size) = resolve_page_window(limit, offset).map_err(AppError::BadRequest)?;

    let cookies = state.infra.settings_service.cookie_header().await?;
    let result = state
        .bili
        .bili_api
        .get_user_videos_page(uid_int, &cookies, page, page_size, false)
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

#[cfg(test)]
mod tests {
    use super::resolve_page_window;

    /// limit 未超 B 站单页上限时，页大小原样透传，页码按 offset/limit 递增。
    #[test]
    fn resolve_page_window_keeps_limit_at_or_below_bilibili_cap() {
        assert_eq!(resolve_page_window(20, 0).unwrap(), (1, 20));
        assert_eq!(resolve_page_window(20, 20).unwrap(), (2, 20));
        assert_eq!(resolve_page_window(20, 40).unwrap(), (3, 20));
        assert_eq!(resolve_page_window(50, 0).unwrap(), (1, 50));
    }

    /// 回归：limit 落在 51..=100 时，页码必须按钳制后的 ps=50 换算。
    /// 此前页码按客户端 limit 计算、窗口按 50 抓取，两者错位导致"加载更多"
    /// 反复取回同一段数据；回显服务端 offset 后还会撞上 offset 校验的 400。
    #[test]
    fn resolve_page_window_clamps_effective_size_above_bilibili_cap() {
        assert_eq!(resolve_page_window(100, 0).unwrap(), (1, 50));
        assert_eq!(resolve_page_window(100, 50).unwrap(), (2, 50));
        assert_eq!(resolve_page_window(100, 100).unwrap(), (3, 50));
        assert_eq!(resolve_page_window(60, 300).unwrap(), (7, 50));
    }

    /// 按返回的 ps 递增 offset 连续翻页：窗口既不重叠也不留空洞。
    #[test]
    fn resolve_page_window_walks_contiguous_windows() {
        let (limit, mut offset, mut previous_end) = (100i32, 0i64, 0i64);
        for expected_page in 1..=6i32 {
            let (page, ps) = resolve_page_window(limit, offset).unwrap();
            assert_eq!(page, expected_page);
            let start = i64::from(page - 1) * i64::from(ps);
            assert_eq!(start, previous_end, "第 {page} 页窗口起点应衔接上一页");
            previous_end = start + i64::from(ps);
            offset += i64::from(ps);
        }
    }

    #[test]
    fn resolve_page_window_rejects_out_of_range_limit() {
        for limit in [0, -1, 101, i32::MAX] {
            let error = resolve_page_window(limit, 0).unwrap_err();
            assert!(error.contains("limit"), "错误文案应指向 limit: {error}");
        }
    }

    #[test]
    fn resolve_page_window_rejects_misaligned_or_negative_offset() {
        // 20 在钳制区间内：offset 必须是 20 的整数倍。
        assert!(resolve_page_window(20, 10).is_err());
        assert!(resolve_page_window(20, -20).is_err());
        // 100 的有效页大小是 50：50 的整数倍可接受（旧实现会误判成 400）。
        assert!(resolve_page_window(100, 50).is_ok());
        assert!(resolve_page_window(100, 30).is_err());
    }
}
