//! 烧录入口：弹幕/CC字幕/混合烧录与 FFmpeg 执行、素材文件定位。

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::fs;
use tokio::process::Command;
use tracing::{error, info, warn};

use super::ass_render::generate_ass;
use super::danmaku_source::load_danmaku_list;
use super::layout::set_position;
use super::subtitle_convert::{convert_subtitle_to_ass, merge_ass_files};
use super::{DanmakuItem, PositionedDanmaku, SubtitleBurner};

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

    /// 通用：根据已生成的弹幕 ASS 内容烧录进视频。
    async fn burn_with_generated_ass(
        &self,
        video_path: &Path,
        positioned: &[PositionedDanmaku],
        source_name: &str,
        operation_name: &str,
    ) -> Result<(bool, Option<PathBuf>, String)> {
        let temp_guard = tempfile::Builder::new()
            .prefix("subtitle_burn_")
            .tempdir()
            .context("创建弹幕烧录临时目录失败")?;
        let temp_dir = temp_guard.path().to_path_buf();

        let stem = video_path
            .file_stem()
            .context("无法获取视频文件名")?
            .to_string_lossy();
        let temp_ass = temp_dir.join("subtitles.ass");
        generate_ass(positioned, &temp_ass, source_name, &stem, &self.burn_config).await?;

        self.burn_with_ass_file(video_path, &temp_ass, operation_name)
            .await
    }

    /// 通用：使用指定 ASS 文件烧录进视频。
    async fn burn_with_ass_file(
        &self,
        video_path: &Path,
        ass_path: &Path,
        operation_name: &str,
    ) -> Result<(bool, Option<PathBuf>, String)> {
        let parent = video_path.parent().context("无法获取视频所在目录")?;
        let stem = video_path
            .file_stem()
            .context("无法获取视频文件名")?
            .to_string_lossy();

        // 确定输出路径
        let output = if stem.contains("_弹幕版") {
            video_path.to_path_buf()
        } else {
            parent.join(format!("{}_弹幕版.mp4", stem))
        };
        info!("[{operation_name}] 输出路径: {}", output.display());

        // 生成临时目录并复制视频/ASS
        let temp_guard = tempfile::Builder::new()
            .prefix("subtitle_burn_")
            .tempdir()
            .context("创建 FFmpeg 临时目录失败")?;
        let temp_dir = temp_guard.path().to_path_buf();

        let temp_video = temp_dir.join("video.mp4");
        let temp_ass = temp_dir.join("subtitles.ass");
        let temp_output = temp_dir.join("output.mp4");

        info!("[{operation_name}] 复制视频到临时目录...");
        fs::copy(video_path, &temp_video)
            .await
            .context("复制视频到临时目录失败")?;
        fs::copy(ass_path, &temp_ass)
            .await
            .context("复制 ASS 到临时目录失败")?;

        // 检查 FFmpeg
        let (ffmpeg, _) = self
            .video_processor
            .detect_ffmpeg("auto", self.custom_ffmpeg_path.as_deref())
            .await;
        let ffmpeg = match ffmpeg {
            Some(p) => {
                info!("[{operation_name}] 使用 FFmpeg: {}", p.display());
                p
            }
            None => {
                warn!("[{operation_name}] 未找到 FFmpeg");
                return Ok((
                    false,
                    None,
                    "未找到 FFmpeg，请安装 FFmpeg 并添加到系统 PATH，或将其放置到 resources 目录"
                        .to_string(),
                ));
            }
        };

        // 烧录字幕
        info!("[{operation_name}] 启动 FFmpeg 烧录进程...");
        let mut command = Command::new(&ffmpeg);
        command
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("warning")
            .arg("-stats")
            .arg("-i")
            .arg(&temp_video)
            .arg("-vf")
            .arg("ass=subtitles.ass")
            .arg("-c:a")
            .arg("copy")
            .arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg("fast")
            .arg("-crf")
            .arg("23")
            .arg("-y")
            .arg(&temp_output)
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let result = match command.spawn() {
            Ok(mut child) => {
                // 并发读取 stderr，避免管道缓冲写满导致进程阻塞
                let stderr = child.stderr.take();
                let stderr_task = tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut buf = Vec::new();
                    if let Some(mut pipe) = stderr {
                        pipe.read_to_end(&mut buf).await.ok();
                    }
                    buf
                });
                match tokio::time::timeout(
                    std::time::Duration::from_secs(6 * 60 * 60),
                    child.wait(),
                )
                .await
                {
                    Ok(status) => {
                        let stderr_buf = stderr_task.await.unwrap_or_default();
                        status.map(|s| (s, stderr_buf))
                    }
                    Err(_) => {
                        // 超时：显式 kill + wait 回收子进程，释放临时目录文件句柄，
                        // 否则 Windows 上 TempDir 删除会失败并静默泄漏 GB 级临时文件
                        if let Err(e) = child.kill().await {
                            warn!("[{operation_name}] 终止超时 FFmpeg 进程失败: {e}");
                        }
                        child.wait().await.ok();
                        stderr_task.await.ok();
                        return Ok((false, None, "FFmpeg 烧录超过 6 小时，已终止".to_string()));
                    }
                }
            }
            Err(e) => Err(e),
        };

        match result {
            Ok((status, _)) if status.success() => {
                info!("[{operation_name}] FFmpeg 进程结束，开始复制输出文件");
                // 先复制到与最终文件同目录的临时文件，再原子 rename，避免复制途中崩溃
                // 在最终路径残留半成品（跨盘无法 rename，故复制到目标同目录）
                let staging = output.with_extension("mp4.burning.tmp");
                fs::copy(&temp_output, &staging)
                    .await
                    .context("复制输出文件失败")?;
                if let Err(e) = fs::rename(&staging, &output).await {
                    fs::remove_file(&staging).await.ok();
                    return Err(anyhow!("原子替换烧录输出失败: {e}"));
                }
                info!("[{operation_name}] 完成: {}", output.display());
                Ok((true, Some(output), format!("{operation_name}成功")))
            }
            Ok((status, stderr_buf)) => {
                let err = String::from_utf8_lossy(&stderr_buf);
                error!(
                    "[{operation_name}] FFmpeg 失败 exit_code={:?} temp_dir={} temp_video={} temp_ass={} temp_output={}\nstderr: {err}",
                    status.code(),
                    temp_dir.display(),
                    temp_video.display(),
                    temp_ass.display(),
                    temp_output.display()
                );
                Ok((false, None, format!("FFmpeg 错误: {err}")))
            }
            Err(e) => {
                error!("[{operation_name}] 启动 FFmpeg 失败: {e}");
                Err(anyhow::anyhow!("执行 ffmpeg 失败: {e}"))
            }
        }
    }

    async fn find_danmaku_file(&self, parent: &Path, stem: &str) -> Option<PathBuf> {
        let bvid = extract_bvid(stem)?;

        // 如果视频文件名包含 _弹幕版，还原原始 stem（BV 号不变，但用于同名匹配）
        let original_stem = if stem.contains("_弹幕版") {
            stem.replace("_弹幕版", "")
        } else {
            stem.to_string()
        };

        let danmaku_dir = parent.join("danmaku");
        let candidates = vec![
            danmaku_dir.join(format!("{bvid}_danmaku.xml")),
            danmaku_dir.join(format!("{bvid}_danmaku.json")),
            danmaku_dir.join(format!("{bvid}.xml")),
            danmaku_dir.join(format!("{bvid}.json")),
            parent.join(format!("{original_stem}_danmaku.xml")),
            parent.join(format!("{original_stem}_danmaku.json")),
            parent.join(format!("{original_stem}.xml")),
            parent.join(format!("{original_stem}.json")),
            parent.join(format!("{bvid}_danmaku.xml")),
            parent.join(format!("{bvid}_danmaku.json")),
            parent.join(format!("{bvid}.xml")),
            parent.join(format!("{bvid}.json")),
        ];

        if let Some(p) = Self::find_first_existing(&candidates).await {
            return Some(p);
        }

        // 兜底：扫描 parent 及其 danmaku 子目录中名称包含 bvid 的 xml/json
        for dir in [parent, &danmaku_dir] {
            let mut entries = match fs::read_dir(dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_lowercase());
                if matches!(ext.as_deref(), Some("xml") | Some("json")) && name.contains(&bvid) {
                    return Some(path);
                }
            }
        }

        None
    }

    async fn find_first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
        for p in paths {
            if p.exists() {
                return Some(p.clone());
            }
        }
        None
    }

    /// 查找字幕文件：优先固定路径（{stem}.ass/.srt），兜底扫描 subtitle/ 目录下 {bvid}*.srt/.ass。
    /// 兜底匹配使用 bvid 前缀 + 边界字符（`_` / `.`），避免前缀误匹配。
    async fn find_subtitle_file(parent: &Path, stem: &str) -> Option<PathBuf> {
        let subtitle_dir = parent.join("subtitle");
        let candidates = [
            subtitle_dir.join(format!("{stem}.ass")),
            subtitle_dir.join(format!("{stem}.srt")),
            parent.join(format!("{stem}.ass")),
            parent.join(format!("{stem}.srt")),
        ];
        if let Some(p) = Self::find_first_existing(&candidates).await {
            return Some(p);
        }
        // 兜底：扫描 subtitle/ 目录下以 {bvid} 开头、扩展名为 .srt 或 .ass 的文件
        let bvid = extract_bvid(stem)?;
        let mut entries = match fs::read_dir(&subtitle_dir).await {
            Ok(e) => e,
            Err(_) => return None,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase());
            if matches!(ext.as_deref(), Some("srt") | Some("ass")) && name.starts_with(&bvid) {
                // 边界检查：bvid 后必须紧跟 `_` 或 `.`（如 {bvid}_zh-CN.srt / {bvid}.srt）
                let rest = &name[bvid.len()..];
                if rest.is_empty() || rest.starts_with('_') || rest.starts_with('.') {
                    return Some(path);
                }
            }
        }
        None
    }
}

fn extract_bvid(text: &str) -> Option<String> {
    use regex::Regex;
    let re = Regex::new(r"BV[0-9a-zA-Z]+").ok()?;
    re.find(text).map(|m| m.as_str().to_string())
}
