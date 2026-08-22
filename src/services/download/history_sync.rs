//! 历史记录同步：完成任务写入 history、SHA-256 记录、封面落盘与历史清理。

use crate::models::{download_task, history};
use crate::services::file_safety::ensure_existing_within_root;
use crate::services::live_recorder::ffmpeg_session::redact_diagnostics;
use anyhow::Result;
use chrono::{DateTime, Local};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::DownloadManager;

/// 终态任务状态全集：history 清理时级联回收对应 download_tasks 行，
/// 以及终态任务的保留上限判定都引用此处。
const TERMINAL_TASK_STATUSES: [&str; 3] = ["completed", "failed", "cancelled"];

/// 手动入队写 history 占位记录所需的信息（见 [`DownloadManager::ensure_history_placeholder`]）。
pub(super) struct HistoryPlaceholder<'a> {
    pub bvid: &'a str,
    pub title: &'a str,
    pub uid: Option<&'a str>,
    pub cid: Option<i64>,
    pub page: Option<i32>,
    pub part_title: Option<&'a str>,
    /// 任务下载目录：封面直接落到该目录，避免完成后二次下载到兜底目录。
    pub cover_dir: Option<&'a Path>,
}

/// 占位记录的落库部分：查询同 (bvid, cid) 既有记录，不存在则插入 state=pending 行。
/// `owner` 为 (owner_name, owner_face, pic) 快照。返回 `Ok(None)` 表示已有记录，
/// 无需再写；插入撞唯一索引（与 video/audio 并发完成或并发入队竞态）时返回 Err。
/// 独立于 DownloadManager 抽出，便于直接对内存 SQLite 做单元测试。
async fn insert_history_placeholder(
    db: &DatabaseConnection,
    info: &HistoryPlaceholder<'_>,
    owner: (Option<String>, Option<String>, Option<String>),
) -> Result<Option<history::Model>> {
    let HistoryPlaceholder {
        bvid,
        title,
        uid,
        cid,
        page,
        part_title,
        ..
    } = *info;
    let mut query = history::Entity::find().filter(history::Column::Bvid.eq(bvid));
    query = match cid {
        Some(cid) => query.filter(history::Column::Cid.eq(cid)),
        None => query.filter(history::Column::Cid.is_null()),
    };
    if query.one(db).await?.is_some() {
        return Ok(None);
    }
    let placeholder = history::ActiveModel {
        uid: Set(uid.map(str::to_string)),
        bvid: Set(bvid.to_string()),
        cid: Set(cid),
        page: Set(page),
        part_title: Set(part_title.map(str::to_string)),
        title: Set(Some(title.to_string())),
        source: Set("manual".to_string()),
        state: Set(Some("pending".to_string())),
        owner_name: Set(owner.0),
        owner_face: Set(owner.1),
        pic: Set(owner.2),
        ..Default::default()
    };
    Ok(Some(placeholder.insert(db).await?))
}

impl DownloadManager {
    /// 手动下载入队时写入占位 history 记录（state=pending）。
    /// 看板「下载中」Tab 由 history 表驱动（board_page 按 bvid ∈ 活跃任务过滤
    /// history 行），入队时不落行则整个传输过程看板无卡片可显，直到首个
    /// 子任务完成经 add_to_history 建行才"突然出现"。完成后由 add_to_history
    /// 的 UPSERT 补齐元数据（uid/pic/download_time 等被 excluded 覆盖）。
    ///
    /// `cover_dir` 为任务下载目录：占位记录建立后立即把封面下载到该目录并回写
    /// cover_local_path。否则前端看板卡片请求 /api/cover 时（uid 可能为空）
    /// 会把封面落到 manual/{日期} 兜底目录，完成后 add_to_history 再下一份
    /// 到任务目录，同一封面存两份且路径不一致。
    pub(super) async fn ensure_history_placeholder(&self, info: HistoryPlaceholder<'_>) {
        let bvid = info.bvid;
        // 未监控博主没有 bloggers 行可兜底，看板分组名只能靠 owner 快照；
        // get_video_info 有会话级缓存，手动入队前 start_download 刚查过，通常零开销。
        let owner = match self.bili_api.get_video_info(bvid, "").await {
            Ok(info) => (
                Some(info.owner.name).filter(|s| !s.is_empty()),
                Some(info.owner.face).filter(|s| !s.is_empty()).map(|s| {
                    if s.starts_with("http") {
                        s
                    } else {
                        format!("https:{s}")
                    }
                }),
                Some(info.pic).filter(|s| !s.is_empty()),
            ),
            Err(e) => {
                warn!("[ensure_history_placeholder] 获取 {bvid} 视频信息失败: {e}");
                (None, None, None)
            }
        };
        let inserted = match insert_history_placeholder(&self.db, &info, owner).await {
            Ok(Some(model)) => model,
            Ok(None) => return,
            Err(e) => {
                warn!("[ensure_history_placeholder] {bvid} 写入占位记录失败: {e}");
                return;
            }
        };
        let Some(dir) = info.cover_dir else { return };
        match self.ensure_cover_local_to(bvid, info.uid, Some(dir)).await {
            Ok(Some(path)) => {
                let mut model: history::ActiveModel = inserted.into();
                model.cover_local_path = Set(Some(path.to_string_lossy().to_string()));
                if let Err(e) = model.update(&self.db).await {
                    warn!("[ensure_history_placeholder] {bvid} 回写封面路径失败: {e}");
                }
            }
            // 目录内已有封面时返回该路径；None 理论上不出现（下载失败走 Err）
            Ok(None) => {}
            Err(e) => {
                // 封面落盘失败不阻塞下载：留空即"可重试标记"，完成路径会再补
                warn!("[ensure_history_placeholder] {bvid} 下载封面失败: {e}");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_to_history(
        &self,
        bvid: &str,
        file_path: Option<&Path>,
        uid: Option<&str>,
        pub_timestamp: Option<i64>,
        cookies: Option<&str>,
        source: &str,
        cid: Option<i64>,
        page: Option<i32>,
        part_title: Option<&str>,
    ) -> Result<()> {
        let mut existing_query = history::Entity::find().filter(history::Column::Bvid.eq(bvid));
        existing_query = match cid {
            Some(cid) => existing_query.filter(history::Column::Cid.eq(cid)),
            None => existing_query.filter(history::Column::Cid.is_null()),
        };
        let existing = existing_query.one(&self.db).await?;
        let requested_source = if source == "manual" { "manual" } else { "auto" };
        // 同一 BV 同时存在自动与手动产物时，history.source 保持 auto，
        // 防止一次手动补下载把自动侧车调度永久排除。
        let source = if requested_source == "auto"
            || existing
                .as_ref()
                .is_some_and(|history| history.source == "auto")
        {
            "auto"
        } else {
            "manual"
        };
        if let Some(existing) = existing.as_ref() {
            // 已完成记录只同步本次任务来源；其余完成字段仍由首次完成事件维护。
            if existing.download_time.is_some() {
                if existing.source != source {
                    let mut model: history::ActiveModel = existing.clone().into();
                    model.source = Set(source.to_string());
                    model.update(&self.db).await?;
                }
                return Ok(());
            }
        }

        // 新记录或未完成占位记录统一补齐视频元数据；调度游标字段由占位记录保留。
        let mut uid_to_save = existing
            .as_ref()
            .and_then(|h| h.uid.clone())
            .or_else(|| uid.map(str::to_string));
        let mut title = existing.as_ref().and_then(|h| h.title.clone());
        let mut pub_ts = existing
            .as_ref()
            .and_then(|h| h.pub_timestamp)
            .or(pub_timestamp);
        let mut pub_date = existing
            .as_ref()
            .and_then(|h| h.pub_date.clone())
            .or_else(|| {
                pub_ts.and_then(|ts| {
                    DateTime::from_timestamp(ts, 0).map(|d| d.format("%Y-%m-%d").to_string())
                })
            });
        let mut pic = existing.as_ref().and_then(|h| h.pic.clone());
        let mut duration = existing.as_ref().and_then(|h| h.duration);
        let mut view = existing.as_ref().and_then(|h| h.view);
        let mut owner_name = existing.as_ref().and_then(|h| h.owner_name.clone());
        let mut owner_face = existing.as_ref().and_then(|h| h.owner_face.clone());

        let info_result = self
            .bili_api
            .get_video_info(bvid, cookies.unwrap_or(""))
            .await;
        if let Err(e) = &info_result {
            // 静默失败会让记录以 uid/owner 为空落库，看板归入 "unknown" 分组，必须留痕。
            warn!("[add_to_history] 获取视频信息失败 {bvid}: {e}");
        }
        if let Ok(info) = info_result {
            title = Some(info.title.clone());
            if info.owner.mid != 0 {
                uid_to_save = Some(info.owner.mid.to_string());
            }
            // UP 主名字/头像快照：未监控博主的看板分组靠它显示（避免只剩纯数字 UID）
            owner_name = Some(info.owner.name.clone()).filter(|s| !s.is_empty());
            owner_face = Some(info.owner.face.clone())
                .filter(|s| !s.is_empty())
                .map(|s| {
                    // 协议相对地址补全（与 get_user_info 的 face 处理保持一致）
                    if s.starts_with("http") {
                        s
                    } else {
                        format!("https:{s}")
                    }
                });
            if let Some(ts) = Some(info.created).filter(|&ts| ts > 0).or(pub_timestamp) {
                pub_ts = Some(ts);
                pub_date = Some(
                    DateTime::from_timestamp(ts, 0)
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_default(),
                );
            }
            pic = Some(info.pic.clone());
            duration = Some(info.duration);
            view = Some(info.stat.view);
        }

        // 封面优先保存到视频实际目录，兼容 manual 与自定义路径模板。
        let cover_dir = file_path.and_then(Path::parent);
        let mut cover_local_path = existing.as_ref().and_then(|h| h.cover_local_path.clone());
        match self
            .ensure_cover_local_to(bvid, uid_to_save.as_deref(), cover_dir)
            .await
        {
            Ok(path) => {
                if let Some(p) = path {
                    cover_local_path = Some(p.to_string_lossy().to_string());
                }
            }
            Err(e) => {
                // 封面落盘失败不阻塞主记录写入：cover_local_path 留空即为可重试标记，
                // 后续 ensure_cover_local / api/cover 按需重新下载封面。
                warn!("[add_to_history] 下载封面失败 {bvid}: {e}");
            }
        }

        // on_completion 模式下立即计算 SHA-256。放在事务外执行：流式读全文件是
        // 耗时 IO，避免长时间持有 SQLite 写事务阻塞其他写入方。
        let mut sha256_digest: Option<String> = None;
        if let Some(path) = file_path {
            if path.exists() {
                match crate::services::file_safety::stream_file_sha256(path).await {
                    Ok(digest) => sha256_digest = Some(digest),
                    Err(e) => warn!("[add_to_history] 计算 SHA-256 失败 {bvid}: {e}"),
                }
            }
        }

        // 使用 UPSERT 原子写入 history：
        // - 解决 video/audio 并发完成时的插入竞态；
        // - 新唯一索引 uix_history_bvid_cid 已去掉 source，DO UPDATE 中按晋升规则处理 source；
        // - next_download_index / next_sidecar_at / sidecar_attempts 等调度游标在冲突时保留原值。
        let now = Local::now();
        let sha256_last_checked_at = sha256_digest.as_ref().map(|_| now);
        self.db
            .execute_raw(Statement::from_sql_and_values(
                self.db.get_database_backend(),
                "INSERT INTO history (
                    uid, bvid, cid, page, part_title, source, title, pub_date, pub_timestamp,
                    download_time, file_path, next_download_index, pic, duration, view, state,
                    cover_local_path, view_source, owner_name, owner_face, sha256,
                    sha256_last_checked_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(bvid, COALESCE(cid, -1)) DO UPDATE SET
                    uid = excluded.uid,
                    cid = excluded.cid,
                    page = excluded.page,
                    part_title = excluded.part_title,
                    source = CASE WHEN history.source = 'auto' OR excluded.source = 'auto' THEN 'auto' ELSE 'manual' END,
                    title = excluded.title,
                    pub_date = excluded.pub_date,
                    pub_timestamp = excluded.pub_timestamp,
                    download_time = excluded.download_time,
                    file_path = excluded.file_path,
                    next_download_index = MAX(history.next_download_index, excluded.next_download_index),
                    pic = excluded.pic,
                    duration = excluded.duration,
                    view = excluded.view,
                    state = excluded.state,
                    cover_local_path = excluded.cover_local_path,
                    view_source = excluded.view_source,
                    owner_name = excluded.owner_name,
                    owner_face = excluded.owner_face,
                    sha256 = excluded.sha256,
                    sha256_last_checked_at = excluded.sha256_last_checked_at",
                [
                    uid_to_save.into(),
                    bvid.to_string().into(),
                    cid.into(),
                    page.into(),
                    part_title.map(str::to_string).into(),
                    source.to_string().into(),
                    title.into(),
                    pub_date.into(),
                    pub_ts.into(),
                    Some(now).into(),
                    file_path.map(|p| p.to_string_lossy().to_string()).into(),
                    0i32.into(),
                    pic.into(),
                    duration.into(),
                    view.into(),
                    "completed".to_string().into(),
                    cover_local_path.into(),
                    Some("snapshot".to_string()).into(),
                    owner_name.into(),
                    owner_face.into(),
                    sha256_digest.into(),
                    sha256_last_checked_at.into(),
                ],
            ))
            .await?;

        if let Err(e) = self.cleanup_history().await {
            warn!("清理历史记录失败: {e}");
        }

        Ok(())
    }

    async fn cleanup_history(&self) -> Result<()> {
        // 设置页 storage.history_limit 优先，环境变量 config.history_limit 兜底
        //（审计 B11：此前设置页改了不生效，只有环境变量能改）。
        let settings_limit = self.settings_service.current().storage.history_limit;
        let limit = if settings_limit > 0 {
            settings_limit
        } else {
            self.config.history_limit
        };
        let count = history::Entity::find().count(&self.db).await?;
        if count > (limit as u64 * 11 / 10) {
            let to_delete: Vec<history::Model> = history::Entity::find()
                // NULL download_time（pending 占位）按 SQLite 规则视为最小值，
                // DESC 排序会落到最旧一端、最先进入删除窗口；显式让 NULL 最后。
                .order_by_asc(history::Column::DownloadTime.is_null())
                .order_by_desc(history::Column::DownloadTime)
                .offset(limit as u64)
                .limit(10000)
                .all(&self.db)
                .await?;
            if !to_delete.is_empty() {
                // 审计修复：删除记录时同步清理本地封面文件，避免孤儿封面累积。
                // 设计意图：只删封面（派生缓存，可随时重新下载），媒体文件保留——
                // history 是展示/索引层，清理记录不代表删除用户已下载的视频内容。
                for h in &to_delete {
                    if let Some(cover) = h.cover_local_path.as_deref() {
                        let path = PathBuf::from(cover);
                        // 审计 DB3：cover_local_path 来自历史库，旧数据可能残留越界
                        // 路径；删除前必须确认仍位于下载根目录内，防止误删根外用户文件。
                        if let Err(e) =
                            ensure_existing_within_root(&self.paths.download_dir, &path).await
                        {
                            warn!(
                                "[cleanup_history] 封面路径越界，跳过删除 {} ({}): {e}",
                                h.bvid,
                                path.display()
                            );
                            continue;
                        }
                        if path.is_file() {
                            if let Err(e) = tokio::fs::remove_file(&path).await {
                                warn!(
                                    "[cleanup_history] 删除封面失败 {} ({}): {e}",
                                    h.bvid,
                                    path.display()
                                );
                            }
                        }
                    }
                }
                let ids: Vec<i32> = to_delete.iter().map(|h| h.id).collect();
                let pairs: Vec<(String, Option<i64>)> =
                    to_delete.iter().map(|h| (h.bvid.clone(), h.cid)).collect();
                history::Entity::delete_many()
                    .filter(history::Column::Id.is_in(ids))
                    .exec(&self.db)
                    .await?;
                // 审计 DB2：history 行被清理后，对应任务的终态 download_tasks 行
                // 随之级联回收，否则自动监控长期运行会每视频累积两行且永不释放。
                self.delete_terminal_tasks_for(&pairs).await?;
            }
        }
        // 审计 DB2：终态任务总量同样受 history_limit 约束（与 history 相同的
        // 10% 余量），超限时按更新时间最旧优先删除，避免任务表随运行时长无限增长、
        // 看板/统计查询持续变慢。
        let terminal_count = download_task::Entity::find()
            .filter(download_task::Column::Status.is_in(TERMINAL_TASK_STATUSES))
            .count(&self.db)
            .await?;
        if terminal_count > (limit as u64 * 11 / 10) {
            let stale_ids: Vec<i32> = download_task::Entity::find()
                .filter(download_task::Column::Status.is_in(TERMINAL_TASK_STATUSES))
                .order_by_asc(download_task::Column::UpdatedAt)
                .offset(limit as u64)
                .limit(10000)
                .all(&self.db)
                .await?
                .into_iter()
                .map(|t| t.id)
                .collect();
            if !stale_ids.is_empty() {
                download_task::Entity::delete_many()
                    .filter(download_task::Column::Id.is_in(stale_ids))
                    .exec(&self.db)
                    .await?;
            }
        }
        Ok(())
    }

    /// 删除与指定 history 记录 (bvid, cid) 对应、处于终态的 download_tasks 行。
    /// 仅删终态行：活跃/暂停中的任务（如用户手动补下载）不受历史清理影响。
    async fn delete_terminal_tasks_for(&self, pairs: &[(String, Option<i64>)]) -> Result<()> {
        if pairs.is_empty() {
            return Ok(());
        }
        let mut any_pair = Condition::any();
        for (bvid, cid) in pairs {
            let mut pair_condition =
                Condition::all().add(download_task::Column::Bvid.eq(bvid.clone()));
            pair_condition = match cid {
                Some(cid) => pair_condition.add(download_task::Column::Cid.eq(*cid)),
                None => pair_condition.add(download_task::Column::Cid.is_null()),
            };
            any_pair = any_pair.add(pair_condition);
        }
        download_task::Entity::delete_many()
            .filter(any_pair)
            .filter(download_task::Column::Status.is_in(TERMINAL_TASK_STATUSES))
            .exec(&self.db)
            .await?;
        Ok(())
    }

    pub(super) async fn get_blogger_uid_from_history(&self, bvid: &str) -> Option<String> {
        history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await
            .ok()
            .flatten()
            .and_then(|h| h.uid)
    }

    /// 内部方法：下载视频封面到服务器，返回保存到本地的文件路径。
    /// 若 UID 已知则保存到 `downloads/{uid}/{bvid}_cover.{ext}`，否则保存到 `downloads/{bvid}_cover.{ext}`。
    pub(super) async fn download_cover_internal(
        &self,
        bvid: &str,
        uid: Option<&str>,
        save_dir_override: Option<&Path>,
    ) -> anyhow::Result<Option<PathBuf>> {
        use reqwest::header::HeaderValue;
        use tokio::io::AsyncWriteExt;

        // 获取视频信息（包含封面URL）
        let info = self.bili_api.get_video_info(bvid, "").await?;
        let pic_url = info.pic.as_str();
        let safe_pic_url = redact_diagnostics(pic_url);
        if pic_url.is_empty() {
            return Err(anyhow::anyhow!("未找到封面URL"));
        }

        // 构建请求头
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(&self.config.user_agent)?,
        );
        headers.insert("Referer", HeaderValue::from_str(&self.config.referer)?);
        headers.insert(
            "Origin",
            HeaderValue::from_static("https://www.bilibili.com"),
        );
        headers.insert("Accept", HeaderValue::from_static("image/*,*/*;q=0.8"));

        // 下载封面图片（按域名选择 TLS 策略）
        let resp = self
            .bili_api
            .client_for(pic_url)
            .get(pic_url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| {
                let diagnostics = redact_diagnostics(&e.to_string());
                anyhow::anyhow!("请求封面失败 {bvid} url={safe_pic_url}: {diagnostics}")
            })?;

        let status_code = resp.status();
        let content_type = resp
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        if !status_code.is_success() {
            return Err(anyhow::anyhow!(
                "下载封面失败 {bvid}: HTTP {} content-type={content_type} url={safe_pic_url}",
                status_code
            ));
        }
        info!(
            "[封面] {bvid} 下载响应 status={} content-type={content_type}",
            status_code
        );

        // 根据已读取的 Content-Type 选择文件扩展名。
        let ext = match content_type.as_str() {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "jpg",
        };

        // 获取下载目录
        let download_dir = match save_dir_override {
            Some(directory) => directory.to_path_buf(),
            None => self.download_dir(uid).await,
        };
        tokio::fs::create_dir_all(&download_dir).await?;

        // 构建文件路径
        let filename = format!("{}_cover.{}", bvid, ext);
        let filepath = download_dir.join(&filename);

        // 保存封面
        let bytes = resp.bytes().await?;
        let mut file = tokio::fs::File::create(&filepath).await?;
        file.write_all(&bytes).await?;
        file.flush().await?;

        info!("[封面] 已下载: {} -> {}", bvid, filepath.display());
        Ok(Some(filepath))
    }

    /// 公开接口：供 api/cover.rs 调用，下载封面并返回本地路径。
    /// 已存在本地文件时跳过下载。
    pub async fn ensure_cover_local(
        &self,
        bvid: &str,
        uid: Option<&str>,
    ) -> anyhow::Result<Option<PathBuf>> {
        self.ensure_cover_local_to(bvid, uid, None).await
    }

    /// 公开接口（带目录偏好）：api/cover 在记录尚无封面时传入下载任务目录，
    /// 保证封面与视频落在同一处，而不是按 uid/日期推导出的其他目录。
    pub async fn ensure_cover_local_in(
        &self,
        bvid: &str,
        uid: Option<&str>,
        dir: Option<&Path>,
    ) -> anyhow::Result<Option<PathBuf>> {
        self.ensure_cover_local_to(bvid, uid, dir).await
    }

    pub(super) async fn ensure_cover_local_to(
        &self,
        bvid: &str,
        uid: Option<&str>,
        save_dir_override: Option<&Path>,
    ) -> anyhow::Result<Option<PathBuf>> {
        // 先查 history 是否已有 cover_local_path 且文件存在
        if let Some(h) = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await?
        {
            if let Some(p) = h.cover_local_path.as_deref() {
                let path = PathBuf::from(p);
                let matches_override = save_dir_override
                    .map(|directory| path.parent() == Some(directory))
                    .unwrap_or(true);
                if path.exists() && matches_override {
                    return Ok(Some(path));
                }
            }
        }
        // 否则在 download_dir(uid) 下扫描所有 {bvid}_cover.* 文件。
        let dir = match save_dir_override {
            Some(directory) => directory.to_path_buf(),
            None => self.download_dir(uid).await,
        };
        if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
            while let Some(entry) = entries.next_entry().await? {
                let name = match entry.file_name().to_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if name.starts_with(&format!("{bvid}_cover.")) && !name.ends_with(".downloading") {
                    return Ok(Some(entry.path()));
                }
            }
        }
        // 无 UID 的记录可能位于 downloads 根目录。
        if uid.is_some() && save_dir_override.is_none() {
            let root_dir = self.paths.download_dir.clone();
            if let Ok(mut entries) = tokio::fs::read_dir(&root_dir).await {
                while let Some(entry) = entries.next_entry().await? {
                    let name = match entry.file_name().to_str() {
                        Some(s) => s.to_string(),
                        None => continue,
                    };
                    if name.starts_with(&format!("{bvid}_cover."))
                        && !name.ends_with(".downloading")
                    {
                        return Ok(Some(entry.path()));
                    }
                }
            }
        }
        // 本地都没有，触发下载
        self.download_cover_internal(bvid, uid, save_dir_override)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::insert_history_placeholder;
    use crate::models::history;
    use sea_orm::{Database, EntityTrait};
    use sea_orm_migration::MigratorTrait;

    fn placeholder<'a>(
        bvid: &'a str,
        cid: Option<i64>,
        page: Option<i32>,
    ) -> super::HistoryPlaceholder<'a> {
        super::HistoryPlaceholder {
            bvid,
            title: "测试视频",
            uid: Some("9469"),
            cid,
            page,
            part_title: Some("第二P"),
            cover_dir: None,
        }
    }

    #[tokio::test]
    async fn history_placeholder_creates_pending_row_once_per_bvid_cid() {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        crate::migration::Migrator::up(&db, None)
            .await
            .expect("run migrations");

        let info = placeholder("BV1xx411c7mD", Some(42), Some(2));
        // 首次插入：创建 state=pending 的手动占位记录，owner 快照落库
        let first = insert_history_placeholder(&db, &info, (Some("UP主".into()), None, None))
            .await
            .expect("首次插入成功")
            .expect("应创建占位记录");
        assert_eq!(first.state.as_deref(), Some("pending"));
        assert_eq!(first.source, "manual");
        assert_eq!(first.owner_name.as_deref(), Some("UP主"));

        // 同 (bvid, cid) 重复调用：已有记录，不重复建行
        let second = insert_history_placeholder(&db, &info, (None, None, None))
            .await
            .expect("二次查询成功");
        assert!(second.is_none());

        // 不同 cid（其他分P）：各自独立建行
        let other = placeholder("BV1xx411c7mD", Some(43), Some(3));
        assert!(insert_history_placeholder(&db, &other, (None, None, None))
            .await
            .expect("分P插入成功")
            .is_some());

        let rows = history::Entity::find().all(&db).await.expect("query rows");
        assert_eq!(rows.len(), 2);
    }
}
