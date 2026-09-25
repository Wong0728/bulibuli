//! 烧录入口：弹幕 / CC 字幕 / 直播互动 / 混合烧录的编排与素材预检。
//! FFmpeg 执行细节见 [`super::ffmpeg_exec`]，素材文件定位见 [`super::source_lookup`]。

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::ass_render::generate_ass;
use super::danmaku_source::load_danmaku_list;
use super::layout::set_position;
use super::subtitle_convert::{convert_subtitle_to_ass, merge_ass_files};
use super::{DanmakuItem, SubtitleBurner};

struct BurnInput {
    parent: PathBuf,
    stem: String,
}

impl SubtitleBurner {
    /// 烧录弹幕到视频（source=danmaku）。
    /// 优先查找 {bvid}_danmaku.xml，其次 .json，转换为 ASS 后烧录。
    pub async fn burn_danmaku(&self, video_path: &Path) -> Result<(bool, Option<PathBuf>, String)> {
        info!("[弹幕烧录] 开始处理: {}", video_path.display());

        let input = match Self::precheck_burn_input(video_path, "弹幕烧录") {
            Ok(input) => input,
            Err(message) => return Ok((false, None, message)),
        };
        let parent = &input.parent;
        let stem = input.stem;

        // 查找弹幕文件（xml 优先，json 兜底）
        let danmaku_path = self.find_danmaku_file(parent, &stem).await;
        let danmaku_path = match danmaku_path {
            Some(p) => p,
            None => {
                warn!("[弹幕烧录] 未找到 {stem} 对应的弹幕文件");
                return Ok((false, None, "未找到对应的弹幕文件（xml/json）".to_string()));
            }
        };
        info!("[弹幕烧录] 找到弹幕文件: {}", danmaku_path.display());

        let danmaku_list = match load_danmaku_list(&danmaku_path).await {
            Ok(list) if list.is_empty() => {
                warn!("[弹幕烧录] 弹幕列表为空: {}", danmaku_path.display());
                return Ok((false, None, "弹幕文件中没有找到弹幕数据".to_string()));
            }
            Ok(list) => {
                info!("[弹幕烧录] 解析到 {} 条弹幕", list.len());
                list
            }
            Err(e) => {
                warn!("[弹幕烧录] 解析弹幕失败 {danmaku_path:?}: {e}");
                return Ok((false, None, format!("解析弹幕失败: {e}")));
            }
        };

        let positioned = set_position(&danmaku_list, &self.burn_config);
        if positioned.is_empty() {
            warn!("[弹幕烧录] 弹幕位置计算失败");
            return Ok((false, None, "弹幕位置计算失败".to_string()));
        }
        info!("[弹幕烧录] 已计算 {} 条弹幕位置", positioned.len());

        let source_name = danmaku_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        self.burn_with_generated_ass(video_path, &positioned, &source_name, "弹幕烧录")
            .await
    }

    /// 烧录直播录制的互动条目（弹幕/SC）：调用方直接提供已解析的条目，
    /// 跳过按 BV 号查找 B 站弹幕文件的步骤；输出命名与下载烧录一致（_弹幕版）。
    pub async fn burn_live_interactions(
        &self,
        video_path: &Path,
        items: Vec<DanmakuItem>,
    ) -> Result<(bool, Option<PathBuf>, String)> {
        info!("[直播互动烧录] 开始处理: {}", video_path.display());
        let _input = match Self::precheck_burn_input(video_path, "直播互动烧录") {
            Ok(input) => input,
            Err(message) => return Ok((false, None, message)),
        };
        if items.is_empty() {
            return Ok((false, None, "没有可烧录的互动内容".to_string()));
        }
        let positioned = set_position(&items, &self.burn_config);
        if positioned.is_empty() {
            warn!("[直播互动烧录] 弹幕位置计算失败");
            return Ok((false, None, "弹幕位置计算失败".to_string()));
        }
        info!("[直播互动烧录] 已计算 {} 条互动位置", positioned.len());
        self.burn_with_generated_ass(
            video_path,
            &positioned,
            "live_interactions.jsonl",
            "直播互动烧录",
        )
        .await
    }

    /// 烧录 CC 字幕到视频（source=subtitle）。
    /// 优先查找 {bvid}.ass，其次 {bvid}.srt，转换为 ASS 后烧录。
    pub async fn burn_subtitle(
        &self,
        video_path: &Path,
    ) -> Result<(bool, Option<PathBuf>, String)> {
        info!("[CC字幕烧录] 开始处理: {}", video_path.display());

        let input = match Self::precheck_burn_input(video_path, "CC字幕烧录") {
            Ok(input) => input,
            Err(message) => return Ok((false, None, message)),
        };
        let parent = &input.parent;
        let stem = input.stem;

        // 查找字幕文件：固定路径优先，兜底扫描 subtitle/{bvid}*.srt/.ass
        let subtitle_path = Self::find_subtitle_file(parent, &stem).await;

        let subtitle_path = match subtitle_path {
            Some(p) => p,
            None => {
                warn!("[CC字幕烧录] 未找到 {stem} 对应的字幕文件");
                return Ok((false, None, "未找到对应的字幕文件（ass/srt）".to_string()));
            }
        };
        info!("[CC字幕烧录] 找到字幕文件: {}", subtitle_path.display());

        let subtitle_temp = tempfile::Builder::new()
            .prefix("subtitle_burn_")
            .tempdir()
            .context("创建字幕临时目录失败")?;
        let ass_path =
            match convert_subtitle_to_ass(&subtitle_path, subtitle_temp.path(), &self.burn_config)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    warn!("[CC字幕烧录] 字幕转换失败 {subtitle_path:?}: {e}");
                    return Ok((false, None, format!("字幕转换失败: {e}")));
                }
            };

        self.burn_with_ass_file(video_path, &ass_path, "CC字幕烧录")
            .await
    }

    /// 合并烧录弹幕 + CC 字幕（source=both）。
    pub async fn burn_mixed(&self, video_path: &Path) -> Result<(bool, Option<PathBuf>, String)> {
        info!("[混合烧录] 开始处理: {}", video_path.display());

        let input = match Self::precheck_burn_input(video_path, "混合烧录") {
            Ok(input) => input,
            Err(message) => return Ok((false, None, message)),
        };
        let parent = &input.parent;
        let stem = input.stem;

        // 查找弹幕
        let danmaku_path = self.find_danmaku_file(parent, &stem).await;
        // 查找字幕：固定路径优先，兜底扫描 subtitle/{bvid}*.srt/.ass
        let subtitle_path = Self::find_subtitle_file(parent, &stem).await;

        if danmaku_path.is_none() && subtitle_path.is_none() {
            warn!("[混合烧录] 未找到 {stem} 对应的弹幕或字幕文件");
            return Ok((false, None, "未找到对应的弹幕或字幕文件".to_string()));
        }

        let temp_guard = tempfile::Builder::new()
            .prefix("subtitle_burn_")
            .tempdir()
            .context("创建混合烧录临时目录失败")?;
        let temp_dir = temp_guard.path().to_path_buf();

        let mut danmaku_ass: Option<PathBuf> = None;
        let mut subtitle_ass: Option<PathBuf> = None;

        if let Some(ref dp) = danmaku_path {
            info!("[混合烧录] 找到弹幕文件: {}", dp.display());
            match load_danmaku_list(dp).await {
                Ok(list) if !list.is_empty() => {
                    let positioned = set_position(&list, &self.burn_config);
                    if !positioned.is_empty() {
                        let ass = temp_dir.join("danmaku.ass");
                        let source_name = dp
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        generate_ass(&positioned, &ass, &source_name, &stem, &self.burn_config)
                            .await?;
                        danmaku_ass = Some(ass);
                    }
                }
                _ => {}
            }
        }

        if let Some(ref sp) = subtitle_path {
            info!("[混合烧录] 找到字幕文件: {}", sp.display());
            match convert_subtitle_to_ass(sp, &temp_dir, &self.burn_config).await {
                Ok(ass) => {
                    subtitle_ass = Some(ass);
                }
                Err(e) => {
                    warn!("[混合烧录] 字幕转换失败 {sp:?}: {e}");
                }
            }
        }

        if danmaku_ass.is_none() && subtitle_ass.is_none() {
            return Ok((false, None, "弹幕或字幕文件均无法转换为 ASS".to_string()));
        }

        // 合并 ASS（字幕在上，弹幕在下分层显示；简单合并事件即可）
        let merged_ass = temp_dir.join("merged.ass");
        merge_ass_files(&merged_ass, danmaku_ass.as_deref(), subtitle_ass.as_deref()).await?;

        self.burn_with_ass_file(video_path, &merged_ass, "混合烧录")
            .await
    }

    fn precheck_burn_input(
        video_path: &Path,
        operation_name: &str,
    ) -> std::result::Result<BurnInput, String> {
        if !video_path.exists() {
            let message = format!("视频文件不存在: {}", video_path.display());
            warn!("[{operation_name}] {message}");
            return Err(message);
        }
        let parent = video_path
            .parent()
            .ok_or_else(|| "无法获取视频所在目录".to_string())?
            .to_path_buf();
        let stem = video_path
            .file_stem()
            .ok_or_else(|| "无法获取视频文件名".to_string())?
            .to_string_lossy()
            .to_string();
        Ok(BurnInput { parent, stem })
    }
}
