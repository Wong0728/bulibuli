//! 新视频入队：博主策略过滤、付费拦截调度与下载任务创建。
//! 入队成功后的弹幕 / 评论 / 字幕自动下载见 `video_sidecar`。

use crate::models::{blogger, download_task};
use crate::services::bili_api::models::user::UserVideo;
use crate::services::download::PageInfo;
use anyhow::Result;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tracing::warn;

use super::{pay_reason_to_state, MonitorService};

impl MonitorService {
    pub(super) async fn add_video_to_queue(
        &self,
        uid: &str,
        video: &UserVideo,
        cookies: &str,
        blogger: &blogger::Model,
    ) -> Result<()> {
        let bvid = video.bvid.as_str();
        let title = if video.title.is_empty() {
            "未知标题"
        } else {
            video.title.as_str()
        };
        // 与 Python 一致：created 为 0 时视为未获取到发布时间
        let pub_timestamp = Some(video.created).filter(|&ts| ts > 0);
        let pic = Some(video.pic.clone());

        // 博主级下载策略：不下载视频则直接跳过
        if !blogger.download_video.unwrap_or(true) {
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!("博主配置不下载视频，跳过 {}", title),
                "info",
            )
            .await;
            return Ok(());
        }

        // 合集白名单正则过滤（仅在视频信息包含 series_name 时生效）
        if let Some(re_str) = blogger.series_filter_regex.as_deref() {
            if !re_str.is_empty() {
                match regex::Regex::new(re_str) {
                    Ok(re) => {
                        let series_name = video.series_name.as_deref().unwrap_or("");
                        if !series_name.is_empty() && !re.is_match(series_name) {
                            self.add_log(
                                Some(uid),
                                Some(bvid),
                                &format!("合集 '{}' 不匹配白名单正则，跳过 {}", series_name, title),
                                "info",
                            )
                            .await;
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        warn!("博主 {} 合集白名单正则无效: {e}", uid);
                    }
                }
            }
        }

        self.add_log(
            Some(uid),
            Some(bvid),
            &format!("检查视频 {} 是否已在下载队列...", title),
            "info",
        )
        .await;
        let existing_task = download_task::Entity::find()
            .filter(crate::models::download_task::Column::Bvid.eq(bvid))
            .filter(crate::models::download_task::Column::TaskType.eq("video"))
            .one(&self.db)
            .await?;
        if let Some(existing) = existing_task {
            tracing::debug!(
                "视频 {} 已在下载队列中（task_id={}），跳过",
                title,
                existing.id
            );
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!("视频 {} 已在下载队列中，跳过", title),
                "info",
            )
            .await;
            return Ok(());
        }

        let settings = self.settings_cached().await?;
        let mut video_quality = settings
            .get("query")
            .and_then(|q| q.get("video_quality"))
            .and_then(|v| v.as_i64())
            .unwrap_or(80) as i32;
        // 与手动路径（/api/download/start）同一 qn 白名单：只接受 B 站真实存在的
        // 画质 code，126/127（杜比/8K）下载暂不支持；非法配置回退默认 80，
        // 不透传给 playurl。
        if !matches!(
            video_quality,
            16 | 32 | 64 | 74 | 80 | 112 | 116 | 120 | 125
        ) {
            warn!("自动下载配置 video_quality={video_quality} 不在支持的白名单内（126/127 暂不支持），回退 80");
            video_quality = 80;
        }
        let min_video_quality = settings
            .get("query")
            .and_then(|q| q.get("min_video_quality"))
            .and_then(|v| v.as_i64())
            .unwrap_or(64) as i32;
        let allow_quality_fallback = settings
            .get("query")
            .and_then(|q| q.get("allow_quality_fallback"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let prefer_codecs: Vec<String> = settings
            .get("query")
            .and_then(|q| q.get("prefer_codecs"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_else(|| vec!["av1".to_string(), "hevc".to_string(), "avc".to_string()]);
        let skip_charge = settings
            .get("query")
            .and_then(|q| q.get("skip_charge_videos"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // --- 充电/付费前置校验 ---
        // 入口前先调 get_video_info，检查 rights/state/is_upower_* 字段；命中以下任一条件则不入队、直接落 history：
        // - state == -100（用户删除）→ state='removed', pay_note='state_deleted'
        // - state in (-1, -6)（审核中/修复审核中）→ state='pay_blocked', pay_note='state_under_review'
        // - is_upower_exclusive 且无权限 → state='pay_blocked', pay_note='upower_no_permission'
        // - is_upower_exclusive 且已充电但开启跳过 → state='pay_blocked', pay_note='upower_paid'（不自动入队，抽屉手动重试）
        // - rights.ugc_pay==1 且不可下载 → state='pay_blocked', pay_note='ugc_pay_no_permission'
        // - rights.ugc_pay==1 且可下载 → state='pay_blocked', pay_note='ugc_pay_paid'（不自动入队，抽屉手动重试）
        // - rights.pay==1 同理区分 pay_paid / pay_no_permission
        self.add_log(
            Some(uid),
            Some(bvid),
            &format!("正在校验视频 {} 的访问权限...", title),
            "info",
        )
        .await;
        let gate = self.gate_download(bvid, title, cookies, skip_charge).await;
        if let Err(reason) = gate {
            let Some((state, pay_note)) = pay_reason_to_state(&reason) else {
                // 瞬时错误（网络失败/风控/获取视频信息失败）：不落库、不改写状态，
                // 下个监控周期会重新视为新视频自然重试；落库成 removed 会被
                // 前端展示成"已下架"，造成视频仍在却标记下架的误判。
                self.add_log(
                    Some(uid),
                    Some(bvid),
                    &format!(
                        "视频 {} 权限校验暂时失败，将在下个周期重试: {}",
                        title, reason
                    ),
                    "warning",
                )
                .await;
                return Ok(());
            };
            // 命中拦截：落 history 记录，不入队
            self.upsert_pay_blocked_history(
                bvid,
                title,
                uid,
                pub_timestamp,
                pic.as_deref(),
                state,
                pay_note,
            )
            .await;
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!("视频 {} 被拦截：{}（{}）", title, pay_note, reason),
                "warning",
            )
            .await;
            return Ok(());
        }

        // 决定分P入队范围：默认/first 仅入队 P1（page=None，保持存量行为）；
        // all 模式且确为多P时，遍历所有分P各自独立入队。
        let multi_page_mode = settings
            .get("monitor")
            .and_then(|m| m.get("multi_page_mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("first")
            .to_string();
        let pages_to_enqueue: Vec<Option<PageInfo>> = if multi_page_mode == "all" {
            match self.bili_api.get_video_info(bvid, cookies).await {
                Ok(info) if info.pages.len() > 1 => info
                    .pages
                    .iter()
                    .map(|p| {
                        Some(PageInfo {
                            cid: p.cid,
                            page: p.page,
                            part_title: p.part.clone(),
                        })
                    })
                    .collect(),
                // 单P或获取失败：回退为单任务（page=None），与现状一致。
                _ => vec![None],
            }
        } else {
            vec![None]
        };
        let is_multi_page = pages_to_enqueue.len() > 1;

        let mut any_enqueued = false;
        for page in &pages_to_enqueue {
            let cid = page.as_ref().map(|p| p.cid);
            // 多P日志带分P标识，单P保持原标题。
            let page_desc = match page {
                Some(p) if !p.part_title.is_empty() => {
                    format!("{} P{} {}", title, p.page, p.part_title)
                }
                Some(p) => format!("{} P{}", title, p.page),
                None => title.to_string(),
            };
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!(
                    "正在获取视频 {} 的下载链接（清晰度: {}）...",
                    page_desc, video_quality
                ),
                "info",
            )
            .await;
            // fnval 统一为 4048（全量 DASH），与手动下载路径一致，避免两条路径取流范围不同
            let urls = match self
                .bili_api
                .get_video_urls(bvid, cookies, 4048, Some(video_quality), cid)
                .await
            {
                Ok(streams) if !streams.qualities.is_empty() => streams,
                outcome => {
                    let msg = match outcome {
                        Ok(_) => "未找到视频流".to_string(),
                        Err(e) => e.to_string(),
                    };
                    if is_multi_page {
                        // 多P时单个分P失败不阻断其余分P，仅记日志后继续。
                        self.add_log(
                            Some(uid),
                            Some(bvid),
                            &format!("{} 获取下载链接失败: {}，已跳过该分P", page_desc, msg),
                            "warning",
                        )
                        .await;
                        continue;
                    }
                    // 单P：落 history 防死循环（不落库的话下个监控周期仍被视为“新视频”，每 10 秒重试一次）
                    self.upsert_pay_blocked_history(
                        bvid,
                        title,
                        uid,
                        pub_timestamp,
                        pic.as_deref(),
                        "failed",
                        "playurl_failed",
                    )
                    .await;
                    self.add_log(
                        Some(uid),
                        Some(bvid),
                        &format!(
                            "视频 {} 获取下载链接失败: {}，已记录为失败，可在看板手动重试",
                            title, msg
                        ),
                        "warning",
                    )
                    .await;
                    return Ok(());
                }
            };

            // 与手动下载路径走同一 qn 白名单校验（choose_video_stream）：
            // 只允许向下回退（<= 目标画质）且不低于最低画质、遵循编码偏好，
            // 不再用 or_else 兜底取最高画质绕过用户配置的上限。
            let selected = crate::services::bili_api::choose_video_stream(
                &urls.qualities,
                video_quality,
                min_video_quality,
                &prefer_codecs,
                allow_quality_fallback,
            );

            if let Some(sel) = selected {
                let url = sel.url.as_str();
                let quality = sel.quality;
                self.add_log(
                    Some(uid),
                    Some(bvid),
                    &format!("正在添加视频 {} 到下载队列...", page_desc),
                    "info",
                )
                .await;
                let result = self
                    .download_manager
                    .add_task(
                        bvid,
                        title,
                        url,
                        cookies,
                        quality,
                        "video",
                        Some(uid),
                        "auto",
                        page.as_ref(),
                        None,
                    )
                    .await?;
                if result.ok {
                    any_enqueued = true;
                    let qname = if sel.quality_name.is_empty() {
                        "未知清晰度"
                    } else {
                        sel.quality_name.as_str()
                    };
                    self.add_log(
                        Some(uid),
                        Some(bvid),
                        &format!("已添加视频到下载队列: {} ({})", page_desc, qname),
                        "success",
                    )
                    .await;
                } else {
                    self.add_log(
                        Some(uid),
                        Some(bvid),
                        &format!("添加视频下载任务失败: {}", result.message),
                        "error",
                    )
                    .await;
                }
            } else {
                self.add_log(
                    Some(uid),
                    Some(bvid),
                    &format!("视频 {} 未找到合适的清晰度", page_desc),
                    "warning",
                )
                .await;
            }
        }
        // 弹幕/评论以 bvid 粒度下载一次（sidecar 服务默认取 P1/默认 cid）。
        if any_enqueued {
            self.auto_download_danmaku(uid, bvid, title, cookies, pub_timestamp)
                .await?;
        }
        Ok(())
    }
}
