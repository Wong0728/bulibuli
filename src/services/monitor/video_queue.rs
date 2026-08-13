//! 新视频入队：博主策略过滤、付费拦截调度与弹幕/评论/字幕自动下载。

use crate::models::{blogger, download_task};
use crate::services::bili_api::models::user::UserVideo;
use crate::services::danmaku::SidecarArchivePolicy;
use crate::services::download::PageInfo;
use anyhow::Result;
use chrono::Local;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::Value;
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
        let video_quality = settings
            .get("query")
            .and_then(|q| q.get("video_quality"))
            .and_then(|v| v.as_i64())
            .unwrap_or(80) as i32;
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
            // 命中拦截：落 history 记录，不入队
            let (state, pay_note) = pay_reason_to_state(&reason);
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

            let selected = urls
                .qualities
                .iter()
                .find(|q| q.quality <= video_quality)
                .or_else(|| urls.qualities.first())
                .cloned();

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

    async fn auto_download_danmaku(
        &self,
        uid: &str,
        bvid: &str,
        title: &str,
        cookies: &str,
        pub_timestamp: Option<i64>,
    ) -> Result<()> {
        let settings = self.settings_cached().await?;
        let dc = settings.get("danmaku_comment").cloned().unwrap_or_default();
        let smart = dc["enable_smart_download"].as_bool().unwrap_or(true);
        let min_hours = dc["min_publish_hours"].as_i64().unwrap_or(1) as f64;
        let time_points = dc["download_time_points"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        self.add_log(
            Some(uid),
            Some(bvid),
            &format!("检查视频 {} 的弹幕/评论下载策略...", title),
            "info",
        )
        .await;

        if !smart {
            self.add_log(
                Some(uid),
                Some(bvid),
                "智能下载已关闭，直接下载弹幕和评论",
                "info",
            )
            .await;
            self.do_download_danmaku(uid, bvid, title, cookies, &dc, None)
                .await?;
            return Ok(());
        }

        let now_ts = Local::now().timestamp();
        // 与 Python 一致：pub_timestamp 缺失时 hours_since = inf，视为已满足条件
        let hours_since = pub_timestamp
            .map(|ts| (now_ts - ts) as f64 / 3600.0)
            .unwrap_or(f64::INFINITY);

        if hours_since >= min_hours {
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!(
                    "视频已发布 {:.1} 小时，满足下载条件，开始下载弹幕和评论",
                    hours_since
                ),
                "info",
            )
            .await;
            self.do_download_danmaku(uid, bvid, title, cookies, &dc, None)
                .await?;

            let next_index = time_points
                .iter()
                .position(|p| p.as_f64().unwrap_or(0.0) > hours_since)
                .unwrap_or(time_points.len()) as i32;
            self.set_history_next_index(uid, bvid, title, next_index, pub_timestamp)
                .await?;
        } else {
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!(
                    "视频刚发布 {:.1} 小时，未达到 {} 小时下载条件，暂不下载弹幕和评论",
                    hours_since, min_hours
                ),
                "info",
            )
            .await;
            self.set_history_next_index(uid, bvid, title, 0, pub_timestamp)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn do_download_danmaku(
        &self,
        uid: &str,
        bvid: &str,
        title: &str,
        cookies: &str,
        dc: &Value,
        page: Option<i32>,
    ) -> Result<()> {
        let archive_policy = SidecarArchivePolicy::new(
            dc["sidecar_archive_mode"].as_str().unwrap_or("overwrite"),
            dc["sidecar_archive_limit"].as_i64().unwrap_or(3),
        );
        let save_dir = self
            .download_manager
            .artifact_dir_for_bvid_page(bvid, "auto", page)
            .await;

        // 保存视频元数据 info.json（与视频同目录）
        if let Some(ref dir) = save_dir {
            match self.bili_api.get_video_info(bvid, cookies).await {
                Ok(info) => {
                    let info_json = serde_json::to_string_pretty(&info).unwrap_or_default();
                    let info_path = dir.join(format!("{bvid}_info.json"));
                    if let Err(e) = tokio::fs::write(&info_path, info_json).await {
                        warn!("[元数据] 写入 info.json 失败 {bvid}: {e}");
                    } else {
                        tracing::info!("[元数据] 已保存: {}", info_path.display());
                    }
                }
                Err(e) => {
                    warn!("[元数据] 获取视频信息失败 {bvid}: {e}");
                }
            }
        }

        if dc["auto_download_danmaku"].as_bool().unwrap_or(true) {
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!("正在下载视频 {} 的弹幕...", title),
                "info",
            )
            .await;
            match self
                .danmaku_service
                .download_danmaku_to(
                    bvid,
                    page,
                    Some(cookies),
                    Some(uid),
                    archive_policy,
                    save_dir.as_deref(),
                )
                .await
            {
                Ok(r) if r["success"].as_bool().unwrap_or(false) => {
                    let count = r["count"].as_i64().unwrap_or(0);
                    if count > 0 {
                        self.add_log(
                            Some(uid),
                            Some(bvid),
                            &format!("弹幕下载成功: {} ({}条)", title, count),
                            "success",
                        )
                        .await;
                    } else {
                        self.add_log(
                            Some(uid),
                            Some(bvid),
                            &format!("视频 {} 暂无弹幕", title),
                            "info",
                        )
                        .await;
                    }
                }
                Ok(r) => {
                    let msg = r["message"].as_str().unwrap_or("");
                    self.add_log(
                        Some(uid),
                        Some(bvid),
                        &format!("弹幕下载失败: {}", msg),
                        "warning",
                    )
                    .await;
                }
                Err(e) => {
                    self.add_log(
                        Some(uid),
                        Some(bvid),
                        &format!("弹幕下载出错: {}", e),
                        "error",
                    )
                    .await;
                }
            }
        }
        if dc["auto_download_comments"].as_bool().unwrap_or(true) {
            let main_limit = dc["comments_main_limit"].as_i64().unwrap_or(30) as usize;
            let reply_mode = dc["comments_reply_mode"].as_str().unwrap_or("hot3");
            let filter_regex = dc["comments_filter_regex"].as_str().unwrap_or("");
            let reply_desc = if reply_mode == "all" {
                "全部"
            } else {
                "约 3 条"
            };
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!(
                    "正在下载视频 {} 的评论（主评论: {}, 回复: {}）...",
                    title, main_limit, reply_desc
                ),
                "info",
            )
            .await;
            match self
                .danmaku_service
                .download_comments_to(
                    bvid,
                    Some(cookies),
                    Some(uid),
                    main_limit,
                    reply_mode,
                    filter_regex,
                    archive_policy,
                    save_dir.as_deref(),
                )
                .await
            {
                Ok(r) if r["success"].as_bool().unwrap_or(false) => {
                    let count = r["count"].as_i64().unwrap_or(0);
                    if count > 0 {
                        self.add_log(
                            Some(uid),
                            Some(bvid),
                            &format!("评论下载成功: {} ({}条主评论)", title, count),
                            "success",
                        )
                        .await;
                    } else {
                        self.add_log(
                            Some(uid),
                            Some(bvid),
                            &format!("视频 {} 暂无评论", title),
                            "info",
                        )
                        .await;
                    }
                }
                Ok(r) => {
                    let msg = r["message"].as_str().unwrap_or("");
                    self.add_log(
                        Some(uid),
                        Some(bvid),
                        &format!("评论下载失败: {}", msg),
                        "warning",
                    )
                    .await;
                }
                Err(e) => {
                    self.add_log(
                        Some(uid),
                        Some(bvid),
                        &format!("评论下载出错: {}", e),
                        "error",
                    )
                    .await;
                }
            }
        }
        // CC 字幕下载：读取 subtitle.enabled，关闭则跳过；无字幕时静默跳过（记 info 日志，不报错）
        let subtitle_settings = self.subtitle_settings().await;
        if subtitle_settings.enabled {
            self.add_log(
                Some(uid),
                Some(bvid),
                &format!("正在下载视频 {} 的 CC 字幕...", title),
                "info",
            )
            .await;
            // 获取 cid：单P用 info.cid，多P用 pages[0].cid（字幕以 bvid 粒度下载一次）
            let cid = match self.bili_api.get_video_info(bvid, cookies).await {
                Ok(info) => {
                    if info.cid > 0 {
                        info.cid
                    } else if let Some(first_page) = info.pages.first() {
                        first_page.cid
                    } else {
                        0
                    }
                }
                Err(e) => {
                    warn!("[字幕] 获取视频信息失败 {bvid}: {e}");
                    self.add_log(
                        Some(uid),
                        Some(bvid),
                        &format!("字幕下载跳过：获取视频信息失败: {e}"),
                        "info",
                    )
                    .await;
                    return Ok(());
                }
            };
            if cid <= 0 {
                self.add_log(
                    Some(uid),
                    Some(bvid),
                    &format!("视频 {} 暂无有效 cid，跳过字幕下载", title),
                    "info",
                )
                .await;
            } else {
                match self
                    .subtitle_service
                    .download_subtitles_to(
                        crate::services::subtitle_fetch::SubtitleDownloadRequest {
                            bvid,
                            cid,
                            cookies: Some(cookies),
                            uid: Some(uid),
                            archive_policy,
                            settings: &subtitle_settings,
                            save_dir_override: save_dir.as_deref(),
                        },
                    )
                    .await
                {
                    Ok(r) if r["success"].as_bool().unwrap_or(false) => {
                        let count = r["count"].as_i64().unwrap_or(0);
                        if count > 0 {
                            self.add_log(
                                Some(uid),
                                Some(bvid),
                                &format!("字幕下载成功: {} ({}条)", title, count),
                                "success",
                            )
                            .await;
                        } else {
                            self.add_log(
                                Some(uid),
                                Some(bvid),
                                &format!("视频 {} 暂无字幕", title),
                                "info",
                            )
                            .await;
                        }
                    }
                    Ok(r) => {
                        let msg = r["message"].as_str().unwrap_or("");
                        self.add_log(
                            Some(uid),
                            Some(bvid),
                            &format!("字幕下载失败: {}", msg),
                            "warning",
                        )
                        .await;
                    }
                    Err(e) => {
                        self.add_log(
                            Some(uid),
                            Some(bvid),
                            &format!("字幕下载出错: {}", e),
                            "error",
                        )
                        .await;
                    }
                }
            }
        }
        Ok(())
    }
}
