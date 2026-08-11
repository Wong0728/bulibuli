//! 单博主检查：视频扫描（含检查点增量分页）、下架/重投检测与资料变更检测。

use crate::models::{blogger, history};
use crate::services::bili_api::models::user::UserVideo;
use anyhow::Result;
use chrono::{DateTime, Local};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, Statement,
};
use std::collections::HashSet;
use tracing::{info, warn};

use super::video_window::{select_video_window, title_similarity};
use super::MonitorService;

impl MonitorService {
    pub(super) async fn check_blogger(&self, blogger: &blogger::Model) -> Result<()> {
        let uid = blogger.uid.clone();
        info!("开始检查博主: {uid}");
        self.add_log(Some(&uid), None, "开始检查博主...", "info")
            .await;

        let cookies = self.get_cookies_for_blogger(&uid).await;
        if cookies.is_empty() {
            self.add_log(
                Some(&uid),
                None,
                "错误: 未配置Cookies，无法获取视频",
                "error",
            )
            .await;
            self.schedule_next(blogger).await?;
            return Ok(());
        }

        let settings = self.settings_cached().await?;
        let query_limit = settings
            .get("query")
            .and_then(|q| q.get("auto_query_limit"))
            .and_then(|v| v.as_i64())
            .unwrap_or(3);
        let skip_charge = settings
            .get("query")
            .and_then(|q| q.get("skip_charge_videos"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let uid_i64: i64 = uid.parse().unwrap_or(0);
        self.add_log(
            Some(&uid),
            None,
            &format!("正在获取视频列表（最近{}个）...", query_limit),
            "info",
        )
        .await;

        // 跳过充电视频时多拉一段窗口，保证过滤后仍能凑满 query_limit 个非充电视频
        let fetch_limit = if skip_charge {
            query_limit + 10
        } else {
            query_limit
        };
        let result = self
            .bili_api
            .get_user_videos_page(uid_i64, &cookies, 1, fetch_limit as i32)
            .await;
        let result = match result {
            Ok(page) => page,
            Err(e) => {
                self.add_log(Some(&uid), None, &format!("获取视频列表失败: {e}"), "error")
                    .await;
                self.schedule_next(blogger).await?;
                return Ok(());
            }
        };

        let mut all_videos = result.videos;
        // 从检查点向后分页，直到命中上次成功扫描的 BVID 或已有历史记录。
        // 这条边界只用于停止扫描；实际去重仍以 history 为准，因此置顶和重投不会漏掉。
        let checkpoint_bvid = self
            .db
            .query_one_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "SELECT last_bvid FROM submission_checkpoints WHERE uid = ?".to_string(),
                [uid.clone().into()],
            ))
            .await?
            .and_then(|row| {
                row.try_get::<Option<String>>("", "last_bvid")
                    .ok()
                    .flatten()
            });
        let page_limit = settings
            .get("monitor")
            .and_then(|v| v.get("scan_page_limit"))
            .and_then(|v| v.as_i64())
            .unwrap_or(5)
            .clamp(1, 20) as i32;
        let mut reached_boundary = checkpoint_bvid
            .as_ref()
            .is_some_and(|checkpoint| all_videos.iter().any(|video| video.bvid == *checkpoint));
        for page in 2..=page_limit {
            if reached_boundary {
                break;
            }
            let page_result = self
                .bili_api
                .get_user_videos_page(uid_i64, &cookies, page, 50)
                .await?;
            let page_videos = page_result.videos;
            if page_videos.is_empty() {
                break;
            }
            reached_boundary = checkpoint_bvid.as_ref().is_some_and(|checkpoint| {
                page_videos.iter().any(|video| video.bvid == *checkpoint)
            });
            all_videos.extend(page_videos);
        }
        // 充电视频不占“最新 N”名额：从新到旧凑满 query_limit 个非充电视频即止；
        // 途中遇到的充电视频保留在列表里，由 gate_download 落 pay_blocked 记录（不入队）
        let videos = select_video_window(all_videos.clone(), query_limit as usize, skip_charge);
        self.add_log(
            Some(&uid),
            None,
            &format!("获取视频列表成功，共 {} 个视频", videos.len()),
            "success",
        )
        .await;

        // 批量查询已存在的 bvid，避免 N+1 查询
        self.add_log(Some(&uid), None, "正在检查已下载记录...", "info")
            .await;
        let bvids: Vec<String> = videos
            .iter()
            .filter(|v| !v.bvid.is_empty())
            .map(|v| v.bvid.clone())
            .collect();
        let existing_histories: Vec<history::Model> = if bvids.is_empty() {
            Vec::new()
        } else {
            history::Entity::find()
                .filter(history::Column::Bvid.is_in(&bvids))
                .all(&self.db)
                .await?
        };
        let existing_bvids: HashSet<String> =
            existing_histories.iter().map(|h| h.bvid.clone()).collect();
        self.add_log(
            Some(&uid),
            None,
            &format!("已检查 {} 个视频的下载记录", existing_bvids.len()),
            "info",
        )
        .await;

        // 下架检测必须查询该博主的全部 history，并只比较本轮实际扫描到的时间范围；
        // 否则可能把扫描深度之外的老视频误判为下架。
        let scanned_bvids: HashSet<String> = all_videos.iter().map(|v| v.bvid.clone()).collect();
        let oldest_scanned_ts = all_videos.iter().map(|v| v.created).min();
        let mut removed_count = 0;
        if let Some(oldest_ts) = oldest_scanned_ts {
            let uid_histories: Vec<history::Model> = history::Entity::find()
                .filter(history::Column::Uid.eq(&uid))
                .all(&self.db)
                .await?;
            for h in &uid_histories {
                // 终态记录不重复改写。
                let cur_state = h.state.as_deref().unwrap_or("completed");
                if !matches!(
                    cur_state,
                    "completed" | "pending" | "downloading" | "failed" | "tampered"
                ) {
                    continue;
                }
                // 只检查落在已扫描时间窗口内的记录
                match h.pub_timestamp {
                    Some(ts) if ts >= oldest_ts => {}
                    _ => continue,
                }
                if !scanned_bvids.contains(&h.bvid) {
                    let mut model: history::ActiveModel = h.clone().into();
                    model.state = Set(Some("removed".to_string()));
                    model.pay_note = Set(Some("state_deleted".to_string()));
                    model.update(&self.db).await?;
                    removed_count += 1;
                    self.add_log(
                        Some(&uid),
                        Some(&h.bvid),
                        &format!("视频已下架: {}", h.title.as_deref().unwrap_or(&h.bvid)),
                        "warning",
                    )
                    .await;
                }
            }
        }
        if removed_count > 0 {
            self.add_log(
                Some(&uid),
                None,
                &format!("检测到 {} 个视频被下架", removed_count),
                "warning",
            )
            .await;
        }

        // --- 重投检测（纯提示，可关闭）：新发现 bvid 与该博主最近 90 天 history 标题相似 ---
        let detect_reupload = settings
            .get("monitor")
            .and_then(|m| m.get("detect_reupload"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let new_videos: Vec<UserVideo> = videos
            .into_iter()
            .filter(|v| !existing_bvids.contains(&v.bvid))
            .collect();

        // 命中重投的 bvid 集合：只落 history 提示，不进入自动下载队列
        let mut reupload_bvids: HashSet<String> = HashSet::new();
        if detect_reupload && !new_videos.is_empty() {
            // 取该博主最近 90 天的 history（含已下架/付费），用于匹配标题
            let now_ts = Local::now().timestamp();
            let cutoff = now_ts - 90 * 24 * 3600;
            let recent: Vec<history::Model> = history::Entity::find()
                .filter(history::Column::Uid.eq(&uid))
                .filter(history::Column::PubTimestamp.gt(cutoff))
                .all(&self.db)
                .await?;
            for v in &new_videos {
                let new_bvid = v.bvid.as_str();
                if new_bvid.is_empty() {
                    continue;
                }
                let new_title = v.title.as_str();
                if new_title.is_empty() {
                    continue;
                }
                let mut best: Option<(&history::Model, f64)> = None;
                for h in &recent {
                    if let Some(old_title) = &h.title {
                        let sim = title_similarity(new_title, old_title);
                        if best
                            .as_ref()
                            .is_none_or(|(_, best_similarity)| sim > *best_similarity)
                        {
                            best = Some((h, sim));
                        }
                    }
                }
                if let Some((h, sim)) = best {
                    if sim >= 0.8 {
                        // 命中重投：写 reupload_of，不自动重下
                        let old_bvid = h.bvid.clone();
                        let new_history = history::ActiveModel {
                            uid: Set(Some(uid.clone())),
                            bvid: Set(new_bvid.to_string()),
                            title: Set(Some(v.title.clone())),
                            pub_timestamp: Set(Some(v.created)),
                            pub_date: Set(DateTime::from_timestamp(v.created, 0)
                                .map(|d| d.format("%Y-%m-%d").to_string())),
                            pic: Set(Some(v.pic.clone())),
                            state: Set(Some("pending".to_string())),
                            reupload_of: Set(Some(old_bvid.clone())),
                            view_source: Set(Some("snapshot".to_string())),
                            ..Default::default()
                        };
                        new_history.insert(&self.db).await?;
                        reupload_bvids.insert(new_bvid.to_string());
                        self.add_log(
                            Some(&uid),
                            Some(new_bvid),
                            &format!("疑似 {} 的重传（相似度 {:.2}）", old_bvid, sim),
                            "warning",
                        )
                        .await;
                    }
                }
            }
        }

        if !new_videos.is_empty() {
            self.add_log(
                Some(&uid),
                None,
                &format!("发现 {} 个新视频！", new_videos.len()),
                "success",
            )
            .await;
            for video in &new_videos {
                let title = if video.title.is_empty() {
                    "未知标题"
                } else {
                    video.title.as_str()
                };
                self.add_log(
                    Some(&uid),
                    None,
                    &format!("正在处理新视频: {}", title),
                    "info",
                )
                .await;
            }
            for video in new_videos {
                // 重投命中的视频不自动重下（history 已落 reupload_of 提示，可手动下载）
                if reupload_bvids.contains(&video.bvid) {
                    continue;
                }
                self.add_video_to_queue(&uid, &video, &cookies, blogger)
                    .await?;
            }
        } else {
            self.add_log(Some(&uid), None, "没有发现新视频", "info")
                .await;
        }

        // --- 博主黄点检测：拉取最新 face/name，与 last_seen_* 比对 ---
        // 注意：此处不写 last_seen_*，只写当前 face/name/level/sign；差异时落 last_seen_*=旧值
        self.check_blogger_profile_change(&uid, blogger).await;

        // --- 博主保留数清理：每轮监控跑完触发一次 ---
        if let Err(e) = self.blogger_service.enforce_retain(&uid).await {
            warn!("博主 {} 保留数清理失败: {e}", uid);
        }

        // 只有完整主流程成功后才推进检查点；失败时保留旧值以便下次补扫。
        if let Some(latest) = all_videos.first() {
            let bvid = latest.bvid.as_str();
            if !bvid.is_empty() {
                let published = Some(latest.created);
                self.db.execute_raw(Statement::from_sql_and_values(
                    self.db.get_database_backend(),
                    "INSERT INTO submission_checkpoints(uid,last_bvid,last_pub_timestamp,last_success_at,updated_at) VALUES(?,?,?,?,?) \
                     ON CONFLICT(uid) DO UPDATE SET last_bvid=excluded.last_bvid,last_pub_timestamp=excluded.last_pub_timestamp,last_success_at=excluded.last_success_at,updated_at=excluded.updated_at".to_string(),
                    [uid.clone().into(), bvid.to_string().into(), published.into(), Local::now().into(), Local::now().into()],
                )).await?;
            }
        }

        self.schedule_next(blogger).await?;
        Ok(())
    }

    /// 比对博主最新 face/name 与 last_seen_*，差异时落 last_seen_*=旧值 + last_seen_at=now。
    /// 同时把 face/name/sign/level/fans 更新为最新值。
    async fn check_blogger_profile_change(&self, uid: &str, current: &blogger::Model) {
        let uid_i64: i64 = match uid.parse() {
            Ok(v) => v,
            Err(e) => {
                warn!("博主资料变更检测失败: uid={} 解析失败, error={}", uid, e);
                return;
            }
        };
        let cookies = self.get_cookies_for_blogger(uid).await;
        let info = match self.bili_api.get_user_info(uid_i64, &cookies).await {
            Ok(v) => v,
            Err(e) => {
                warn!("博主资料变更检测失败: uid={}, error={}", uid, e);
                return;
            }
        };
        let fresh_name = Some(info.name.clone());
        let fresh_face = Some(info.face.clone());
        let fresh_sign = Some(info.sign.clone());
        let fresh_level = info.level as i32;

        let mut model: blogger::ActiveModel = current.clone().into();
        let mut changed = false;

        // 改名检测：fresh_name 与当前 name 不同，且 last_seen_name 还没记录过
        if let Some(ref new_name) = fresh_name {
            if Some(new_name.as_str()) != current.name.as_deref() {
                if current.last_seen_name.is_none() && current.name.is_some() {
                    model.last_seen_name = Set(current.name.clone());
                }
                model.name = Set(Some(new_name.clone()));
                changed = true;
            }
        }
        // 改头像检测
        if let Some(ref new_face) = fresh_face {
            if Some(new_face.as_str()) != current.face.as_deref() {
                if current.last_seen_face.is_none() && current.face.is_some() {
                    model.last_seen_face = Set(current.face.clone());
                }
                model.face = Set(Some(new_face.clone()));
                changed = true;
            }
        }
        if fresh_sign.as_deref() != current.sign.as_deref() {
            model.sign = Set(fresh_sign);
            changed = true;
        }
        if Some(fresh_level) != current.level {
            model.level = Set(Some(fresh_level));
            changed = true;
        }
        // 粉丝数变化时同步刷新（relation/stat 失败时 fans=0，保留旧值不覆盖）
        if info.fans > 0 && current.fans != Some(info.fans) {
            model.fans = Set(Some(info.fans));
            changed = true;
        }
        if changed {
            model.updated_at = Set(Some(Local::now()));
            if let Err(e) = model.update(&self.db).await {
                warn!("更新博主资料失败 {uid}: {e}");
            }
        }
    }
}
