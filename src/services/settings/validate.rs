//! 运行时设置的合法性校验：各设置组取值范围、格式与组合约束，
//! 违规一律返回 `BadRequest`/`Config`，避免非法值进入运行路径。

use super::model::{RuntimeSettings, CONFIG_VERSION};
use crate::error::{AppError, AppResult};
use std::path::Path;

impl RuntimeSettings {
    pub fn validate(&self) -> AppResult<()> {
        if self.config_version > CONFIG_VERSION {
            return Err(AppError::Config(format!(
                "设置版本 {} 高于程序支持版本 {CONFIG_VERSION}",
                self.config_version
            )));
        }
        if !matches!(
            self.download_mode.mode.as_str(),
            "embedded" | "system" | "external"
        ) {
            return Err(AppError::BadRequest(
                "下载模式必须是 embedded、system 或 external".to_string(),
            ));
        }
        if !(1..=16).contains(&self.aria2c_basic.max_connection_per_server)
            || !(1..=16).contains(&self.aria2c_basic.split)
            || !(3..=10).contains(&self.aria2c_basic.max_tries)
            || !(1..=30).contains(&self.aria2c_basic.retry_wait)
            || !(1..=32).contains(&self.aria2c_basic.max_concurrent_downloads)
        {
            return Err(AppError::BadRequest(
                "aria2 并发、分片或重试设置超出允许范围".to_string(),
            ));
        }
        if !(1..=100).contains(&self.danmaku_comment.comments_main_limit)
            || !(0..=72).contains(&self.danmaku_comment.min_publish_hours)
            || !matches!(
                self.danmaku_comment.comments_reply_mode.as_str(),
                "hot3" | "all"
            )
            || !matches!(
                self.danmaku_comment.sidecar_archive_mode.as_str(),
                "overwrite" | "keep_latest_n" | "keep_all"
            )
            || !(1..=50).contains(&self.danmaku_comment.sidecar_archive_limit)
            || self
                .danmaku_comment
                .download_time_points
                .iter()
                .any(|point| !(0..=72).contains(point))
        {
            return Err(AppError::BadRequest(
                "弹幕/评论设置超出允许范围".to_string(),
            ));
        }
        if !matches!(
            self.ffmpeg.mode.as_str(),
            "auto" | "system" | "embedded" | "custom"
        ) || self.ffmpeg.mode == "custom" && self.ffmpeg.custom_path.trim().is_empty()
        {
            return Err(AppError::BadRequest("FFmpeg 设置无效".to_string()));
        }
        if !matches!(
            self.download.verify.mode.as_str(),
            "off" | "on_completion" | "periodic"
        ) || !(1..=90).contains(&self.download.verify.periodic_days)
            || !(1..=200).contains(&self.download.verify.periodic_batch)
            || !(1..=16).contains(&self.download.verify.concurrency)
        {
            return Err(AppError::BadRequest("文件校验设置无效".to_string()));
        }
        let template = Path::new(&self.download_path.path_template);
        if template.is_absolute()
            || template.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(AppError::BadRequest(
                "下载路径模板不能是绝对路径或包含路径穿越".to_string(),
            ));
        }

        if !(1..=100).contains(&self.query.manual_query_limit)
            || !(1..=100).contains(&self.query.auto_query_limit)
        {
            return Err(AppError::BadRequest(
                "查询条数必须在 1..=100 范围内".to_string(),
            ));
        }
        if !(16..=127).contains(&self.query.min_video_quality)
            || !matches!(
                self.query.audio_quality_preference.as_str(),
                "m4a" | "dolby" | "flac"
            )
            || self.query.prefer_codecs.is_empty()
            || self
                .query
                .prefer_codecs
                .iter()
                .any(|codec| !matches!(codec.as_str(), "av1" | "hevc" | "avc"))
        {
            return Err(AppError::BadRequest("画质、音频或编码偏好无效".to_string()));
        }
        if !matches!(
            self.download_path.conflict_strategy.as_str(),
            "suffix" | "skip" | "overwrite"
        ) || !(1..=20).contains(&self.monitor.scan_page_limit)
            || !matches!(self.monitor.multi_page_mode.as_str(), "first" | "all")
            || !matches!(self.appearance.theme.as_str(), "system" | "light" | "dark")
            || !matches!(
                self.board.path_display_mode.as_str(),
                "hidden" | "relative" | "absolute"
            )
        {
            return Err(AppError::BadRequest(
                "文件冲突策略、扫描页数、多P监控模式或主题设置无效".to_string(),
            ));
        }
        if self.parallel_download.max_parallel == 0 || self.parallel_download.max_parallel > 32 {
            return Err(AppError::BadRequest(
                "并行下载数必须在 1..=32 范围内".to_string(),
            ));
        }
        if !(60..=3600).contains(&self.parallel_download.wait_slot_timeout) {
            return Err(AppError::BadRequest(
                "等待下载槽超时必须在 60..=3600 秒范围内".to_string(),
            ));
        }
        if self.aria2_rpc.port == 0 {
            return Err(AppError::BadRequest("aria2 RPC 端口不能为 0".to_string()));
        }
        if self.storage.history_limit <= 0 || self.storage.log_limit <= 0 {
            return Err(AppError::BadRequest(
                "历史记录和日志上限必须大于 0".to_string(),
            ));
        }
        if self.refresh.l1_interval_minutes == 0 {
            return Err(AppError::BadRequest("刷新间隔必须大于 0".to_string()));
        }
        let regex = self.danmaku_comment.comments_filter_regex.trim();
        if !regex.is_empty() {
            regex::RegexBuilder::new(regex)
                .size_limit(10_240)
                .dfa_size_limit(10_240)
                .build()
                .map_err(|error| AppError::BadRequest(format!("评论过滤正则无效: {error}")))?;
        }
        // 弹幕烧录参数范围校验：越界返回 BadRequest，避免前端传入非法值导致布局错乱
        if !(0.1..=1.0).contains(&self.burn.opacity)
            || !(1.0..=60.0).contains(&self.burn.scroll_time)
            || !(1.0..=60.0).contains(&self.burn.fix_time)
            || !(0.5..=2.0).contains(&self.burn.font_size_scale)
            || !(0.0..=200.0).contains(&self.burn.bottom_reserve)
            || !matches!(
                self.burn.font_family.as_str(),
                "auto" | "Microsoft YaHei UI" | "Noto Sans CJK SC" | "Arial"
            )
            || !matches!(self.burn.color_mode.as_str(), "source" | "uniform")
            || self.burn.color.len() != 6
            || !self.burn.color.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(AppError::BadRequest("弹幕烧录参数超出允许范围".to_string()));
        }
        if !(1..=8).contains(&self.live.max_concurrent)
            || !(1..=1024).contains(&self.live.min_free_space_gib)
            || !(1..=72).contains(&self.live.max_duration_hours)
            || self.live.file_name_template.trim().is_empty()
        {
            return Err(AppError::BadRequest("直播录制设置超出允许范围".to_string()));
        }
        if !matches!(self.update.policy.as_str(), "auto" | "manual" | "off") {
            return Err(AppError::BadRequest(
                "更新策略必须是 auto、manual 或 off".to_string(),
            ));
        }
        Ok(())
    }
}
