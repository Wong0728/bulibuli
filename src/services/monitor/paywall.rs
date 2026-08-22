//! 充电/付费前置校验：gate_download 判定与 pay_blocked 历史记录落库。

use crate::models::history;
use chrono::DateTime;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use tracing::warn;

use super::MonitorService;

impl MonitorService {
    /// 充电/付费前置校验。Ok(()) 表示可入队；Err(reason) 表示被拦截，reason 用于落 pay_note。
    /// `skip_charge`：是否跳过充电专属视频（开启时即使已充电也不自动入队，仅落记录供手动重试）。
    pub async fn gate_download(
        &self,
        bvid: &str,
        _title: &str,
        cookies: &str,
        skip_charge: bool,
    ) -> Result<(), String> {
        let info = self
            .bili_api
            .get_video_info(bvid, cookies)
            .await
            .map_err(|e| format!("获取视频信息失败: {e}"))?;

        match info.state {
            -100 => return Err("state_deleted".to_string()),
            -1 | -6 => return Err("state_under_review".to_string()),
            _ => {}
        }

        // 充电专属视频：必须用 is_upower_exclusive 判定（充电视频的 rights.ugc_pay/pay 均为 0），
        // is_upower_play 直接给出当前账号权限，无需再探测 playurl
        if info.is_upower_exclusive {
            if !info.is_upower_play {
                return Err("upower_no_permission".to_string());
            }
            if skip_charge {
                return Err("upower_paid".to_string());
            }
            // 已充电且未开启“跳过充电视频”→ 放行自动下载
        }

        if info.rights.ugc_pay == 1 {
            // 试一次 get_video_urls 看是否有 dash.video[]
            return self
                .probe_download_permission(bvid, cookies, "ugc_pay")
                .await;
        }
        if info.rights.pay == 1 {
            return self.probe_download_permission(bvid, cookies, "pay").await;
        }
        Ok(())
    }

    /// 用 get_video_urls 探测当前 Cookie 能否下载付费视频。
    /// 有 dash.video[] → *_paid；权限型拦截（试看片段 durl）→ *_no_permission。
    /// 其余请求失败（网络/风控）属于瞬时错误：返回未映射的原始错误，
    /// 由调用方仅告警并下周期重试，而不是误落 pay_blocked。
    async fn probe_download_permission(
        &self,
        bvid: &str,
        cookies: &str,
        kind: &str,
    ) -> Result<(), String> {
        match self
            .bili_api
            .get_video_urls(bvid, cookies, 4048, Some(80), None)
            .await
        {
            Ok(streams) => {
                if !streams.qualities.is_empty() {
                    Err(format!("{kind}_paid"))
                } else {
                    Err(format!("{kind}_no_permission"))
                }
            }
            Err(error) => {
                // 未购买的付费视频 playurl 有两种确定性无权限信号：
                // - 业务码 87007（需要充电/付费）
                // - 回"试看"durl，被服务层以权限型 StreamUnavailableError 拒绝
                // 两者都归类为 no_permission 落库，不能当瞬时错误无限重试
                // （此前会每个监控周期刷一遍探测日志）。
                if let Some(bili_error) = error.downcast_ref::<crate::error::BiliApiError>() {
                    if bili_error.kind == crate::error::BiliErrorKind::ChargeRequired {
                        return Err(format!("{kind}_no_permission"));
                    }
                }
                if error
                    .downcast_ref::<crate::services::bili_api::StreamUnavailableError>()
                    .is_some_and(|e| e.permission)
                {
                    return Err(format!("{kind}_no_permission"));
                }
                Err(format!("探测{kind}下载权限失败: {error}"))
            }
        }
    }

    /// 命中付费/下架拦截时落 history（如已有则更新）。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn upsert_pay_blocked_history(
        &self,
        bvid: &str,
        title: &str,
        uid: &str,
        pub_timestamp: Option<i64>,
        pic: Option<&str>,
        state: &str,
        pay_note: &str,
    ) {
        let existing = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await
            .ok()
            .flatten();
        let pub_date = pub_timestamp.and_then(|ts| {
            DateTime::from_timestamp(ts, 0).map(|d| d.format("%Y-%m-%d").to_string())
        });
        if let Some(h) = existing {
            let mut model: history::ActiveModel = h.into();
            model.state = Set(Some(state.to_string()));
            model.pay_note = Set(Some(pay_note.to_string()));
            if let Some(p) = pic {
                model.pic = Set(Some(p.to_string()));
            }
            if let Err(e) = model.update(&self.db).await {
                warn!("视频删除事件处理失败: bvid={}, error={}", bvid, e);
            }
        } else {
            let new_history = history::ActiveModel {
                uid: Set(Some(uid.to_string())),
                bvid: Set(bvid.to_string()),
                title: Set(Some(title.to_string())),
                pub_date: Set(pub_date),
                pub_timestamp: Set(pub_timestamp),
                pic: Set(pic.map(|s| s.to_string())),
                state: Set(Some(state.to_string())),
                pay_note: Set(Some(pay_note.to_string())),
                view_source: Set(Some("snapshot".to_string())),
                ..Default::default()
            };
            if let Err(e) = new_history.insert(&self.db).await {
                warn!("视频删除事件处理失败: bvid={}, error={}", bvid, e);
            }
        }
    }
}

/// 把 `gate_download` 返回的 reason 字符串映射成 `(state, pay_note)`。
/// - `state_deleted` → `removed` / `state_deleted`
/// - `state_under_review` → `pay_blocked` / `state_under_review`
/// - `upower_paid` → `pay_blocked` / `upower_paid`（充电专属且已充电，手动重试）
/// - `upower_no_permission` → `pay_blocked` / `upower_no_permission`（充电专属且无权限）
/// - `ugc_pay_paid` → `pay_blocked` / `ugc_pay_paid`
/// - `ugc_pay_no_permission` → `pay_blocked` / `ugc_pay_no_permission`
/// - `pay_paid` → `pay_blocked` / `pay_paid`
/// - `pay_no_permission` → `pay_blocked` / `pay_no_permission`
///
/// 返回 `None` 表示瞬时错误（网络失败、风控、获取视频信息失败等）：
/// 这类原因**不能**落库为 `removed`（会被前端展示成"已下架"造成误判），
/// 调用方应仅告警并让下个监控周期自然重试。
pub(crate) fn pay_reason_to_state(reason: &str) -> Option<(&'static str, &'static str)> {
    match reason {
        "state_deleted" => Some(("removed", "state_deleted")),
        "state_under_review" => Some(("pay_blocked", "state_under_review")),
        "upower_paid" => Some(("pay_blocked", "upower_paid")),
        "upower_no_permission" => Some(("pay_blocked", "upower_no_permission")),
        "ugc_pay_paid" => Some(("pay_blocked", "ugc_pay_paid")),
        "ugc_pay_no_permission" => Some(("pay_blocked", "ugc_pay_no_permission")),
        "pay_paid" => Some(("pay_blocked", "pay_paid")),
        "pay_no_permission" => Some(("pay_blocked", "pay_no_permission")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pay_reason_to_state_mapping() {
        assert_eq!(
            pay_reason_to_state("state_deleted"),
            Some(("removed", "state_deleted"))
        );
        assert_eq!(
            pay_reason_to_state("upower_paid"),
            Some(("pay_blocked", "upower_paid"))
        );
        assert_eq!(
            pay_reason_to_state("upower_no_permission"),
            Some(("pay_blocked", "upower_no_permission"))
        );
        assert_eq!(
            pay_reason_to_state("ugc_pay_paid"),
            Some(("pay_blocked", "ugc_pay_paid"))
        );
        assert_eq!(
            pay_reason_to_state("ugc_pay_no_permission"),
            Some(("pay_blocked", "ugc_pay_no_permission"))
        );
        assert_eq!(
            pay_reason_to_state("pay_paid"),
            Some(("pay_blocked", "pay_paid"))
        );
        assert_eq!(
            pay_reason_to_state("pay_no_permission"),
            Some(("pay_blocked", "pay_no_permission"))
        );
        assert_eq!(
            pay_reason_to_state("state_under_review"),
            Some(("pay_blocked", "state_under_review"))
        );
        // 瞬时错误（网络失败 / 获取视频信息失败等）不落库，返回 None
        assert_eq!(pay_reason_to_state("not_found"), None);
        assert_eq!(pay_reason_to_state("获取视频信息失败: 超时"), None);
        assert_eq!(pay_reason_to_state("xxx"), None);
    }
}
