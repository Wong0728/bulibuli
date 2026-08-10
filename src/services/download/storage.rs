//! 下载目录派生与文件归位：目录模板渲染、任务目录回退与 MD5 去重。

use crate::models::{download_task, history};
use crate::services::file_safety::{atomic_replace, render_path_template, sanitize_filename};
use anyhow::{anyhow, Result};
use chrono::Local;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::{is_valid_bvid, DedupeResult, DownloadManager, PageInfo};

impl DownloadManager {
    /// 返回指定来源视频任务的实际产物目录，供弹幕/评论与视频保持同目录。
    pub(crate) async fn artifact_dir_for_bvid(&self, bvid: &str, source: &str) -> Option<PathBuf> {
        let task = download_task::Entity::find()
            .filter(download_task::Column::Bvid.eq(bvid))
            .filter(download_task::Column::TaskType.eq("video"))
            .filter(download_task::Column::Source.eq(source))
            .one(&self.db)
            .await
            .ok()
            .flatten()?;
        task.download_dir
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.starts_with(&self.paths.download_dir) && path.exists())
    }

    pub async fn download_dir(&self, uid: Option<&str>) -> PathBuf {
        match uid.filter(|u| !u.is_empty() && u.chars().all(|c| c.is_ascii_digit())) {
            Some(u) => {
                let dir = self.paths.download_dir.join(u);
                if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                    warn!("创建博主下载目录失败 {}: {e}", dir.display());
                }
                dir
            }
            None => {
                if let Some(invalid_uid) = uid {
                    warn!("拒绝将非法 UID 用作目录名: {invalid_uid:?}");
                }
                // 手动下载无关联博主时，使用按日期分类的 manual 目录
                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                let dir = self.paths.download_dir.join("manual").join(&today);
                if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                    warn!("创建手动下载目录失败 {}: {e}", dir.display());
                }
                dir
            }
        }
    }

    pub(super) async fn templated_download_dir(
        &self,
        uid: Option<&str>,
        title: &str,
        bvid: &str,
        quality: i32,
        task_type: &str,
        page: Option<&PageInfo>,
    ) -> PathBuf {
        let settings = self.settings_service.current();
        if !settings.download_path.auto_organize {
            return self.download_dir(uid).await;
        }
        // 多P时 {page} 用实际分P序号、{part} 用分P标题；单P保持 page=1、part 回退为标题。
        let page_str = page
            .map(|p| p.page.to_string())
            .unwrap_or_else(|| "1".to_string());
        let part_str = match page {
            Some(p) if !p.part_title.is_empty() => sanitize_filename(&p.part_title),
            _ => sanitize_filename(title),
        };
        let variables = HashMap::from([
            ("title", sanitize_filename(title)),
            ("bvid", sanitize_filename(bvid)),
            ("uid", sanitize_filename(uid.unwrap_or("manual"))),
            // 没有博主名称时用 UID 作稳定回退，避免模板因未知变量失效。
            ("up", sanitize_filename(uid.unwrap_or("manual"))),
            ("date", chrono::Local::now().format("%Y-%m-%d").to_string()),
            ("page", page_str),
            ("part", part_str),
            ("quality", quality.to_string()),
            ("codec", "auto".to_string()),
            ("type", sanitize_filename(task_type)),
        ]);
        match render_path_template(
            &self.paths.download_dir,
            &settings.download_path.path_template,
            &variables,
        ) {
            Ok(directory) => {
                if let Err(error) = tokio::fs::create_dir_all(&directory).await {
                    warn!("创建模板下载目录失败 {}: {error}", directory.display());
                    self.download_dir(uid).await
                } else {
                    directory
                }
            }
            Err(error) => {
                warn!("下载路径模板无效，使用安全回退目录: {error}");
                self.download_dir(uid).await
            }
        }
    }

    /// 从 download_task 记录中获取实际下载目录（优先使用存储的 download_dir，回退到派生逻辑）
    pub(super) async fn task_download_dir(&self, task: &download_task::Model) -> PathBuf {
        // 优先使用存储的下载目录（避免跨天日期变化）
        if let Some(ref dir_str) = task.download_dir {
            let dir = PathBuf::from(dir_str);
            if dir.exists() {
                let canonical_root = std::fs::canonicalize(&self.paths.download_dir);
                let canonical_dir = std::fs::canonicalize(&dir);
                if matches!(
                    (&canonical_root, &canonical_dir),
                    (Ok(root), Ok(candidate)) if candidate.starts_with(root)
                ) {
                    return dir;
                }
                warn!(
                    "忽略越过下载根目录的任务路径: bvid={}, path={}",
                    task.bvid,
                    dir.display()
                );
            }
        }
        // 回退：从 history 获取 uid 派生目录
        let uid = self.get_blogger_uid_from_history(&task.bvid).await;
        self.download_dir(uid.as_deref()).await
    }

    /// MD5 去重 + 文件归位：
    /// - 计算临时文件 `temp_path` 的 MD5
    /// - 扫描同目录下所有 `{stem}*.{ext}` 文件（排除 .downloading）
    /// - 若无匹配文件：重命名为 `original_name`（首次下载）
    /// - 若有匹配文件且任一 MD5 相同：删除临时文件，返回"内容未变更"
    /// - 若有匹配文件但 MD5 都不同：重命名为 `{stem}_{YYYYMMDD_HHMMSS}.{ext}`
    ///
    /// `stem` 单P为 bvid（存量行为不变），多P为 `{bvid}_p{page}`，用于隔离同 bvid 不同分P的文件。
    pub(super) async fn dedupe_and_finalize_file(
        &self,
        temp_path: &Path,
        original_name: &str,
        bvid: &str,
        stem: &str,
        task_type: &str,
    ) -> Result<DedupeResult> {
        // 防御性校验 bvid 格式：必须是合法 BV 号（BV + 10 位 base58 字符），
        // 防止恶意 bvid（如 "../../etc/passwd"）触发的路径穿越或误匹配。
        // 注：add_task_inner 已做入口校验，此处为 defense-in-depth。
        if !is_valid_bvid(bvid) {
            return Err(anyhow!("非法 bvid 格式: {bvid}（期望 BV + 10 位字符）"));
        }

        let dir = temp_path
            .parent()
            .ok_or_else(|| anyhow!("无法获取临时文件父目录"))?;

        // 计算临时文件 MD5（流式分块，避免大文件打爆内存）
        let temp_size = tokio::fs::metadata(temp_path).await?.len();
        let temp_md5 = crate::services::file_safety::stream_file_md5(temp_path).await?;
        info!("[MD5去重] {} 临时文件 MD5: {}", bvid, temp_md5);

        // 提取扩展名（task_type 决定：video→m4s, audio→m4a）
        let ext = if task_type == "audio" { "m4a" } else { "m4s" };

        // 扫描同目录下所有匹配 {bvid}*.{ext} 的文件（排除 .downloading）
        // 使用 bvid + "_" / bvid + "." 边界判断，避免前缀误匹配（如 BV1 与 BV1XX）
        let mut existing_files: Vec<(String, PathBuf)> = Vec::new();
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            // 排除 .downloading 临时文件，只匹配 .{ext} 结尾
            if name.ends_with(".downloading") {
                continue;
            }
            if !name.ends_with(&format!(".{ext}")) {
                continue;
            }
            // 严格匹配：必须以 {stem} 开头，且其后字符必须为 "_" 或 "."（避免前缀误匹配）
            if !name.starts_with(stem) {
                continue;
            }
            let rest = &name[stem.len()..];
            if !rest.is_empty() && !rest.starts_with('_') && !rest.starts_with('.') {
                continue;
            }
            existing_files.push((name.to_string(), path));
        }

        if existing_files.is_empty() {
            // 首次下载或全部已删：直接重命名为 original_name
            let final_path = dir.join(original_name);
            atomic_replace(temp_path, &final_path).await?;
            info!(
                "[MD5去重] {} 无已存在文件，重命名为 {}",
                bvid, original_name
            );
            return Ok(DedupeResult {
                final_filename: original_name.to_string(),
                message: "下载完成".to_string(),
            });
        }

        let cached_history = history::Entity::find()
            .filter(history::Column::Bvid.eq(bvid))
            .one(&self.db)
            .await
            .ok()
            .flatten();

        // 比对已存在文件的 MD5
        for (name, path) in &existing_files {
            match tokio::fs::metadata(path).await {
                Ok(metadata) if metadata.len() != temp_size => continue,
                Ok(_) => {}
                Err(error) => {
                    warn!("[MD5去重] 读取已存在文件大小失败 {}: {error}", name);
                    continue;
                }
            }
            let cached_md5 = cached_history.as_ref().and_then(|history| {
                let same_file = history
                    .file_path
                    .as_deref()
                    .is_some_and(|cached_path| Path::new(cached_path) == path);
                if same_file {
                    history.md5.clone()
                } else {
                    None
                }
            });
            let existing_md5 = if let Some(cached_md5) = cached_md5 {
                cached_md5
            } else {
                match crate::services::file_safety::stream_file_md5(path).await {
                    Ok(digest) => digest,
                    Err(e) => {
                        warn!("[MD5去重] 读取已存在文件失败 {}: {e}", name);
                        continue;
                    }
                }
            };
            if existing_md5 == temp_md5 {
                // 内容相同：删除临时文件，保留原文件
                if let Err(error) = tokio::fs::remove_file(temp_path).await {
                    warn!("清理去重临时文件失败: {error}");
                }
                info!("[MD5去重] {} 内容未变更（MD5 一致），删除临时文件", bvid);
                return Ok(DedupeResult {
                    final_filename: name.clone(),
                    message: "内容未变更，跳过保存".to_string(),
                });
            }
        }

        // 内容不同的同名冲突：默认追加时间戳，用户也可选择跳过或覆盖。
        let conflict_strategy = self
            .settings_service
            .current()
            .download_path
            .conflict_strategy
            .clone();
        if conflict_strategy == "skip" {
            tokio::fs::remove_file(temp_path).await?;
            let retained = existing_files
                .first()
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| original_name.to_string());
            return Ok(DedupeResult {
                final_filename: retained,
                message: "检测到同名不同内容，按策略跳过保存".to_string(),
            });
        }
        if conflict_strategy == "overwrite" {
            let final_path = dir.join(original_name);
            atomic_replace(temp_path, &final_path).await?;
            return Ok(DedupeResult {
                final_filename: original_name.to_string(),
                message: "检测到同名不同内容，已按策略覆盖".to_string(),
            });
        }
        // 默认 suffix：保留历史版本，重命名为带时间戳的新文件。
        let timestamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
        let new_name = format!("{stem}_{timestamp}.{ext}");
        let new_path = dir.join(&new_name);
        atomic_replace(temp_path, &new_path).await?;
        info!(
            "[MD5去重] {} 检测到内容变化，保留为新版本: {}",
            bvid, new_name
        );
        Ok(DedupeResult {
            final_filename: new_name,
            message: "检测到内容变化，已保留为新版本".to_string(),
        })
    }
}
