//! 视频附属产物自动下载：入队成功后按智能策略下载弹幕 / 评论 / CC 字幕
//! （含 info.json 元数据落盘与分时间点补下载的进度登记）。

use crate::services::danmaku::SidecarArchivePolicy;
use anyhow::Result;
use chrono::Local;
use serde_json::Value;
use tracing::warn;

use super::MonitorService;

/// 智能补下载策略：从下载时间点表中找出下一个未完成的时间点下标。
fn next_download_index(time_points: &[Value], hours_since: f64) -> i32 {
    time_points
        .iter()
        .position(|point| point.as_f64().unwrap_or(0.0) > hours_since)
        .unwrap_or(time_points.len()) as i32
}

impl MonitorService {
    pub(super) async fn auto_download_danmaku(
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

            let next_index = next_download_index(&time_points, hours_since);
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

#[cfg(test)]
mod tests {
    use super::next_download_index;
    use serde_json::json;

    #[test]
    fn next_download_index_skips_completed_points_and_handles_invalid_values() {
        let points = vec![json!(1), json!(6), json!(24)];
        assert_eq!(next_download_index(&points, 0.5), 0);
        assert_eq!(next_download_index(&points, 6.0), 2);
        assert_eq!(next_download_index(&points, 48.0), 3);

        let invalid = vec![json!("later"), json!(2)];
        assert_eq!(next_download_index(&invalid, 0.0), 1);
    }
}
