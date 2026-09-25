//! 具体刷新任务：L1 视频实时数据批量刷新、L2 博主资料批量刷新与单个视频的
//! 手动刷新，含 B 站返回信息到 history/blogger 模型的字段映射。

use super::gate::RefreshReport;
use crate::error::BiliErrorKind;
use crate::models::{blogger, history};
use crate::services::bili_api::models::user::same_image_url;
use crate::services::bili_api::models::video::VideoInfo;
use crate::services::bili_api::BiliApi;
use anyhow::Result;
use chrono::Local;
use futures::{stream, StreamExt};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

/// L1：抽 50 条最久未刷的视频，有界并发调 get_video_info 写回 view/state。
/// 返回成功刷新的条数。
pub(super) async fn refresh_video_stats(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    cookies: &str,
) -> Result<RefreshReport> {
    // 选 state in (completed, pay_blocked, removed) 且按 view_refreshed_at ASC 的 50 条
    let rows = history::Entity::find()
        .filter(history::Column::State.is_in(vec!["completed", "pay_blocked", "removed"]))
        .order_by_asc(history::Column::ViewRefreshedAt)
        .limit(super::L1_BATCH)
        .all(db)
        .await?;

    if rows.is_empty() {
        return Ok(RefreshReport::default());
    }

    let row_count = rows.len();
    info!("[refresh L1] 开始刷新 {row_count} 条视频的实时数据");
    let risk_limited = Arc::new(AtomicBool::new(false));
    let success = stream::iter(rows)
        .map(|h| {
            let risk_limited = risk_limited.clone();
            async move {
                match bili_api.get_video_info(&h.bvid, cookies).await {
                    Ok(info) => {
                        let model = map_video_info(&h, &info);
                        if let Err(e) = model.update(db).await {
                            warn!("[refresh L1] 更新 {} 失败: {e}", h.bvid);
                            false
                        } else {
                            true
                        }
                    }
                    Err(e) => {
                        warn!("[refresh L1] 获取 {} 出错: {e}", h.bvid);
                        if is_risk_or_permission(&e) {
                            risk_limited.store(true, Ordering::Relaxed);
                        }
                        if !is_risk_or_permission(&e) {
                            touch_view_refreshed_at(db, h.id).await;
                        }
                        false
                    }
                }
            }
        })
        .buffer_unordered(4)
        .filter(|updated| futures::future::ready(*updated))
        .count()
        .await;
    info!("[refresh L1] 完成，成功 {success}/{row_count} 条");
    Ok(RefreshReport {
        success,
        risk_limited: risk_limited.load(Ordering::Relaxed),
    })
}

fn is_risk_or_permission(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<crate::error::BiliApiError>()
            .is_some_and(|bili| {
                matches!(
                    bili.kind,
                    BiliErrorKind::RiskControl
                        | BiliErrorKind::Unauthorized
                        | BiliErrorKind::ChargeRequired
                        | BiliErrorKind::GeoRestricted
                )
            })
    })
}

pub(super) async fn refresh_all_bloggers_report(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    cookies: &str,
) -> Result<RefreshReport> {
    let bloggers = blogger::Entity::find().all(db).await?;
    if bloggers.is_empty() {
        return Ok(RefreshReport::default());
    }
    let blogger_count = bloggers.len();
    info!("[refresh L2] 开始刷新 {blogger_count} 个博主资料");
    let outcomes = stream::iter(bloggers)
        .map(|blogger| refresh_blogger_profile(db, bili_api, blogger, cookies))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let success = outcomes.iter().filter(|(updated, _)| *updated).count();
    let risk_limited = outcomes.iter().any(|(_, risk)| *risk);
    info!("[refresh L2] 完成，成功 {success}/{blogger_count} 个");
    Ok(RefreshReport {
        success,
        risk_limited,
    })
}

async fn refresh_blogger_profile(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    blogger: blogger::Model,
    cookies: &str,
) -> (bool, bool) {
    let uid: i64 = match blogger.uid.parse() {
        Ok(uid) => uid,
        Err(_) => return (false, false),
    };
    let info = match bili_api.get_user_info(uid, cookies, true).await {
        Ok(info) => info,
        Err(error) => {
            warn!("[refresh L2] 获取博主 {} 出错: {error}", blogger.uid);
            return (false, is_risk_or_permission(&error));
        }
    };
    let mut model: blogger::ActiveModel = blogger.clone().into();
    let mut changed = false;
    if Some(info.name.as_str()) != blogger.name.as_deref()
        && blogger.last_seen_name.is_none()
        && blogger.name.is_some()
    {
        model.last_seen_name = Set(blogger.name.clone());
        model.last_seen_at = Set(Some(Local::now()));
        changed = true;
    }
    if Some(info.name.as_str()) != blogger.name.as_deref() {
        model.name = Set(Some(info.name.clone()));
        changed = true;
    }
    // 头像用"同图"比较：同一张图的 CDN 主机会在 i0/i1/i2 间轮换，
    // 整串比较会误报改头像。
    if !same_image_url(Some(info.face.as_str()), blogger.face.as_deref())
        && blogger.last_seen_face.is_none()
        && blogger.face.is_some()
    {
        model.last_seen_face = Set(blogger.face.clone());
        model.last_seen_at = Set(Some(Local::now()));
        changed = true;
    }
    if !same_image_url(Some(info.face.as_str()), blogger.face.as_deref()) {
        model.face = Set(Some(info.face.clone()));
        changed = true;
    }
    if Some(info.sign.as_str()) != blogger.sign.as_deref() {
        model.sign = Set(Some(info.sign));
        changed = true;
    }
    if Some(info.level as i32) != blogger.level {
        model.level = Set(Some(info.level as i32));
        changed = true;
    }
    if !changed {
        return (true, false);
    }
    model.updated_at = Set(Some(Local::now()));
    if let Err(error) = model.update(db).await {
        warn!("[refresh L2] 更新博主 {} 失败: {error}", blogger.uid);
        (false, false)
    } else {
        (true, false)
    }
}

/// 手动刷新单个视频：写回 view/state/view_refreshed_at。
/// history 查不到记录时返回 NotFound（审计 B6：此前静默返回成功，API 假成功）。
pub(super) async fn refresh_single_video(
    db: &DatabaseConnection,
    bili_api: &BiliApi,
    bvid: &str,
    cookies: &str,
) -> crate::error::AppResult<()> {
    use crate::error::AppError;
    let h = history::Entity::find()
        .filter(history::Column::Bvid.eq(bvid))
        .one(db)
        .await
        .map_err(|error| AppError::Internal(format!("查询视频记录失败: {error}")))?;
    let Some(h) = h else {
        return Err(AppError::NotFound("未找到该视频的下载记录".to_string()));
    };
    let info = bili_api
        .get_video_info(bvid, cookies)
        .await
        .map_err(|error| AppError::Internal(format!("获取视频信息失败: {error}")))?;
    map_video_info(&h, &info)
        .update(db)
        .await
        .map_err(|error| AppError::Internal(format!("更新视频记录失败: {error}")))?;
    Ok(())
}

fn map_video_info(history: &history::Model, info: &VideoInfo) -> history::ActiveModel {
    let mut model: history::ActiveModel = history.clone().into();
    model.view = Set(Some(info.stat.view));
    let new_state = match info.state {
        -100 => "removed".to_string(),
        -1 | -6 => "pay_blocked".to_string(),
        _ => history
            .state
            .clone()
            .unwrap_or_else(|| "completed".to_string()),
    };
    if history.state.as_deref() != Some(&new_state) {
        model.state = Set(Some(new_state));
    }
    model.view_refreshed_at = Set(Some(Local::now()));
    model.view_source = Set(Some("live".to_string()));
    // uid 缺失的旧记录（完成时 get_video_info 失败落库）：用视频信息回填，
    // 否则看板会永久归入 "unknown" 分组（add_to_history 对已完成记录不回头补）。
    if history.uid.is_none() && info.owner.mid != 0 {
        model.uid = Set(Some(info.owner.mid.to_string()));
    }
    if history.owner_name.is_none() && !info.owner.name.is_empty() {
        model.owner_name = Set(Some(info.owner.name.clone()));
    }
    if history.owner_face.is_none() && !info.owner.face.is_empty() {
        let face = if info.owner.face.starts_with("http") {
            info.owner.face.clone()
        } else {
            format!("https:{}", info.owner.face)
        };
        model.owner_face = Set(Some(face));
    }
    model
}

/// 失败时也更新 view_refreshed_at，避免该条一直被选中。
async fn touch_view_refreshed_at(db: &DatabaseConnection, id: i32) {
    if let Ok(Some(h)) = history::Entity::find_by_id(id).one(db).await {
        let mut model: history::ActiveModel = h.into();
        model.view_refreshed_at = Set(Some(Local::now()));
        if let Err(error) = model.update(db).await {
            warn!("[refresh] 持久化刷新结果失败: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_risk_or_permission;
    use crate::error::{BiliApiError, BiliErrorKind};

    #[test]
    fn risk_or_permission_classifies_bili_errors() {
        let risk = BiliApiError {
            code: -352,
            kind: BiliErrorKind::RiskControl,
            message: "请求被拦截".to_string(),
            retryable: false,
        };
        assert!(is_risk_or_permission(&anyhow::Error::new(risk)));
        let wrapped = anyhow::Error::new(BiliApiError {
            code: 87007,
            kind: BiliErrorKind::ChargeRequired,
            message: "充电专享".to_string(),
            retryable: false,
        })
        .context("外层错误");
        assert!(is_risk_or_permission(&wrapped));
        // 普通网络/解析错误不算风控（不触发触碰 refreshed_at 的跳过逻辑）
        assert!(!is_risk_or_permission(&anyhow::anyhow!("网络超时")));
    }
}
