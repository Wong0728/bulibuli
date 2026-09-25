//! 媒体链接解析接口：把用户输入解析为 BV/AV/ep/ss/fp，并拉取番剧/课程季分集信息。

use crate::error::{ApiResponse, AppError};
use crate::models::history;
use crate::services::url_parser::{resolve_media_input, ResolvedMedia};
use crate::state::SharedState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

#[derive(Deserialize)]
pub(crate) struct ResolveVideoRequest {
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
pub(crate) async fn resolve_video(
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
