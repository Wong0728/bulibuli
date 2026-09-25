//! 封面落盘：下载 B 站封面到本地目录，并提供「已有封面则跳过」的幂等入口。
//! api/cover.rs 与完成落库路径都经由 [`DownloadManager::ensure_cover_local_to`]。

use crate::models::history;
use crate::services::live_recorder::ffmpeg_session::redact_diagnostics;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::path::{Path, PathBuf};
use tracing::info;

use super::DownloadManager;

impl DownloadManager {
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
