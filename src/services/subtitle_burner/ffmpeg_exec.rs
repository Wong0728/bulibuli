//! FFmpeg 烧录执行：能力探测、进程编排（临时目录、超时与 stderr 回收）、
//! 输出原子替换，以及无烧录能力时导出 ASS 侧车字幕的降级路径。

use crate::services::spawn_util::child_lifetime;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;
use tracing::{error, info, warn};

use super::ass_render::generate_ass;
use super::{PositionedDanmaku, SubtitleBurner};

/// 解析 `ffmpeg -filters` / `-encoders` 列表：每行格式为“标志 名称 描述”，
/// 取第二列精确匹配名称，避免子串误判（如 ass 命中 subtitles 等）。
fn listing_has_named_entry(text: &str, name: &str) -> bool {
    text.lines()
        .any(|line| line.split_whitespace().nth(1) == Some(name))
}

/// 能力探测的挂起上限：与 `ffmpeg_detect::CAPABILITY_TIMEOUT` 同量级。
const LISTING_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn ffmpeg_listing_contains(ffmpeg: &Path, flag: &str, name: &str) -> bool {
    let mut command = Command::new(ffmpeg);
    command
        .arg("-hide_banner")
        .arg(flag)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    child_lifetime::configure(&mut command);
    let output = match tokio::time::timeout(LISTING_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => return false,
        Err(_) => {
            warn!("FFmpeg 能力探测超时（{flag} 查询 {name}），按不具备该能力处理");
            return false;
        }
    };
    listing_has_named_entry(&String::from_utf8_lossy(&output.stdout), name)
}

/// 烧录能力不足时的统一指引：当前 FFmpeg 缺少 ass 滤镜或视频编码器。
const FULL_FFMPEG_HINT: &str = "当前使用的 FFmpeg 不具备烧录能力（需要 ass 滤镜与 libx264/mpeg4 编码器）；请在“设置 → FFmpeg”中改为完整版 FFmpeg（例如 gyan.dev 的 full 构建或系统包管理器安装的版本）";

impl SubtitleBurner {
    /// 通用：根据已生成的弹幕 ASS 内容烧录进视频。
    pub(super) async fn burn_with_generated_ass(
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
    pub(super) async fn burn_with_ass_file(
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

        // 检查 FFmpeg（与 ffmpeg_supports_burn 用同一套 mode/自定义路径解析）
        let (ffmpeg, _) = self
            .video_processor
            .detect_ffmpeg(&self.ffmpeg_mode, self.custom_ffmpeg_path.as_deref())
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

        // 能力探测放在复制视频之前：精简版 FFmpeg 直接走 ASS 侧车导出，
        // 避免先整卷复制视频再失败，也避免 "Unrecognized option 'preset'" 这类晦涩报错。
        let has_ass_filter = ffmpeg_listing_contains(&ffmpeg, "-filters", "ass").await;
        let has_libx264 = ffmpeg_listing_contains(&ffmpeg, "-encoders", "libx264").await;
        let has_mpeg4 =
            !has_libx264 && ffmpeg_listing_contains(&ffmpeg, "-encoders", "mpeg4").await;
        if !has_ass_filter || (!has_libx264 && !has_mpeg4) {
            warn!(
                "[{operation_name}] FFmpeg 不支持烧录（ass 滤镜={}，libx264={}，mpeg4={}），降级导出 ASS 字幕: {}",
                has_ass_filter,
                has_libx264,
                has_mpeg4,
                ffmpeg.display()
            );
            return self
                .export_ass_sidecar(&stem, parent, ass_path, operation_name)
                .await;
        }

        // 磁盘预检：临时目录（系统盘 %TEMP%）需容纳源视频副本 + 编码输出
        // （约 2 倍源体积），目标盘需容纳最终输出；不足时提前报可读错误，
        // 避免整卷复制/编码数小时后才因磁盘写满失败。
        // ensure_disk_space 内部走 spawn_blocking，不在 async 线程同步调用 fs2。
        let source_bytes = tokio::fs::metadata(video_path)
            .await
            .with_context(|| format!("读取视频文件大小失败: {}", video_path.display()))?
            .len();
        crate::services::file_safety::ensure_disk_space(
            &std::env::temp_dir(),
            Some(source_bytes.saturating_mul(2)),
        )
        .await
        .with_context(|| {
            format!(
                "烧录临时目录（{}）磁盘空间不足",
                std::env::temp_dir().display()
            )
        })?;
        crate::services::file_safety::ensure_disk_space(parent, Some(source_bytes))
            .await
            .with_context(|| format!("输出目录（{}）磁盘空间不足", parent.display()))?;

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
            .arg("-c:v");
        if has_libx264 {
            // x264：preset/crf 是其编码器私有选项，仅在确认存在时传入。
            command
                .arg("libx264")
                .arg("-preset")
                .arg("fast")
                .arg("-crf")
                .arg("23");
        } else {
            // 兜底编码器 mpeg4 不支持 preset/crf，用 -q:v 控制质量。
            command.arg("mpeg4").arg("-q:v").arg("3");
        }
        command
            .arg("-y")
            .arg(&temp_output)
            .current_dir(&temp_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let result = match child_lifetime::spawn(&mut command) {
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

    /// FFmpeg 无法烧录时的降级方案（与 Bili23 Downloader 同思路）：
    /// 导出与视频同名的 ASS 字幕文件，PotPlayer / mpv 等播放器会自动加载同名 .ass 显示弹幕。
    /// 返回 success=false：烧录确实未发生（历史记录不标记已烧录），但产物可用且给出清晰指引。
    async fn export_ass_sidecar(
        &self,
        stem: &str,
        parent: &Path,
        ass_path: &Path,
        operation_name: &str,
    ) -> Result<(bool, Option<PathBuf>, String)> {
        let ass_output = parent.join(format!("{stem}.ass"));
        fs::copy(ass_path, &ass_output)
            .await
            .with_context(|| format!("导出 ASS 字幕文件失败: {}", ass_output.display()))?;
        info!(
            "[{operation_name}] FFmpeg 不支持烧录，已导出 ASS 字幕: {}",
            ass_output.display()
        );
        let name = ass_output
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok((
            false,
            Some(ass_output),
            format!(
                "当前 FFmpeg 缺少烧录能力，未烧录进视频；已导出同名 ASS 字幕文件（{name}），用 PotPlayer / mpv 等播放器打开视频即可自动加载弹幕。如需真正烧录：{FULL_FFMPEG_HINT}"
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::listing_has_named_entry;

    #[test]
    fn ffmpeg_listing_matches_second_column_exactly() {
        let filters = "Filters:\n T..C. subtitles V->V Render text subtitles...\n T.S.C. ass S->V Render ASS subtitles...";
        assert!(listing_has_named_entry(filters, "ass"));
        assert!(listing_has_named_entry(filters, "subtitles"));
        // 名称必须精确匹配，不能子串命中。
        assert!(!listing_has_named_entry(filters, "subtitle"));
        let encoders = "Encoders:\n V....D libx264 libx264 H.264...\n A..... aac AAC encoder";
        assert!(listing_has_named_entry(encoders, "libx264"));
        assert!(listing_has_named_entry(encoders, "aac"));
        assert!(!listing_has_named_entry(encoders, "x264"));
    }
}
