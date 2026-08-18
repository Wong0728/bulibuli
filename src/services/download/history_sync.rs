//! 历史记录同步：完成任务写入 history、SHA-256 记录、封面落盘与历史清理。

use crate::models::history;
use crate::services::live_recorder::ffmpeg_session::redact_diagnostics;
use anyhow::Result;
use chrono::{DateTime, Local};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::DownloadManager;

impl DownloadManager {
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

        if let Ok(info) = self
            .bili_api
            .get_video_info(bvid, cookies.unwrap_or(""))
            .await
        {
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
                warn!("[add_to_history] 下载封面失败 {bvid}: {e}");
            }
        }

        if let Some(existing) = existing {
            let mut model: history::ActiveModel = existing.into();
            model.uid = Set(uid_to_save);
            model.cid = Set(cid);
            model.page = Set(page);
            model.part_title = Set(part_title.map(str::to_string));
            model.source = Set(source.to_string());
            model.title = Set(title);
            model.pub_date = Set(pub_date);
            model.pub_timestamp = Set(pub_ts);
            model.download_time = Set(Some(Local::now()));
            model.file_path = Set(file_path.map(|p| p.to_string_lossy().to_string()));
            model.pic = Set(pic);
            model.duration = Set(duration);
            model.view = Set(view);
            model.state = Set(Some("completed".to_string()));
            model.cover_local_path = Set(cover_local_path);
            model.view_source = Set(Some("snapshot".to_string()));
            model.owner_name = Set(owner_name);
            model.owner_face = Set(owner_face);
            // next_download_index / next_sidecar_at / sidecar_attempts 保持占位值。
            model.update(&self.db).await?;
        } else {
            let new_history = history::ActiveModel {
                uid: Set(uid_to_save),
                bvid: Set(bvid.to_string()),
                cid: Set(cid),
                page: Set(page),
                part_title: Set(part_title.map(str::to_string)),
                source: Set(source.to_string()),
                title: Set(title),
                pub_date: Set(pub_date),
                pub_timestamp: Set(pub_ts),
                download_time: Set(Some(Local::now())),
                file_path: Set(file_path.map(|p| p.to_string_lossy().to_string())),
                next_download_index: Set(0),
                pic: Set(pic),
                duration: Set(duration),
                view: Set(view),
                state: Set(Some("completed".to_string())),
                cover_local_path: Set(cover_local_path),
                view_source: Set(Some("snapshot".to_string())),
                owner_name: Set(owner_name),
                owner_face: Set(owner_face),
                ..Default::default()
            };
            new_history.insert(&self.db).await?;
        }

        // on_completion 模式下立即计算 SHA-256
        if let Some(path) = file_path {
            if path.exists() {
                if let Err(e) = self.compute_and_store_sha256(bvid, path).await {
                    warn!("[add_to_history] 计算 SHA-256 失败 {bvid}: {e}");
                }
            }
        }

        if let Err(e) = self.cleanup_history().await {
            warn!("清理历史记录失败: {e}");
        }

        Ok(())
    }

    /// 计算文件 SHA-256（流式分块，避免整文件读入内存）并写入 history.sha256。
    /// 由 add_to_history（on_completion）与 verify worker（periodic）共用。
    pub async fn compute_and_store_sha256(&self, bvid: &str, path: &Path) -> Result<String> {
        let digest = crate::services::file_safety::stream_file_sha256(path).await?;
        let h = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .filter(history::Column::FilePath.eq(path.to_string_lossy().to_string()))
            .one(&self.db)
            .await?;
        if let Some(h) = h {
            let mut model: history::ActiveModel = h.into();
            model.sha256 = Set(Some(digest.clone()));
            model.sha256_last_checked_at = Set(Some(Local::now()));
            model.update(&self.db).await?;
        }
        Ok(digest)
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
            let to_delete: Vec<i32> = history::Entity::find()
                .order_by_desc(history::Column::DownloadTime)
                .offset(limit as u64)
                .limit(10000)
                .all(&self.db)
                .await?
                .into_iter()
                .map(|h| h.id)
                .collect();
            if !to_delete.is_empty() {
                history::Entity::delete_many()
                    .filter(history::Column::Id.is_in(to_delete))
                    .exec(&self.db)
                    .await?;
            }
        }
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
