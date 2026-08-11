//! 弹幕/字幕烧录服务（us-danmaku 算法移植）。
//!
//! 子模块划分：
//! - `burn`：烧录入口与 FFmpeg 执行（burn_danmaku / burn_subtitle / burn_mixed）
//! - `danmaku_source`：弹幕文件（xml/json）加载解析
//! - `subtitle_convert`：SRT → ASS 转换与 ASS 合并
//! - `layout`：弹幕轨道布局（R2L / TOP / BOTTOM 位置计算）
//! - `ass_render`：ASS 文本生成与格式化工具
//!
//! 烧录参数通过 `BurnConfig` 注入，未配置时使用 us-danmaku 默认值（行为与迭代前一致）。

mod ass_render;
mod burn;
mod danmaku_source;
mod layout;
mod subtitle_convert;

use crate::services::video_processor::VideoProcessor;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// us-danmaku 的内部参考画布尺寸（不可配置的算法常量）。
const PLAY_RES_X: f64 = 560.0;
const PLAY_RES_Y: f64 = 420.0;
// 弹幕密度/丢弃策略调参常量（不暴露给用户，避免误调导致布局错乱）。
const SPACE: f64 = 0.0;
const MAX_DELAY: f64 = 6.0;

/// 烧录参数配置：用户可在设置页调整，未设置时使用 us-danmaku 默认值。
///
/// 字段含义参见 `功能迭代计划.md` 迭代项6：
/// - `opacity`：弹幕透明度（0.1~1.0），默认 0.6。
/// - `scroll_time`：滚动弹幕（R2L）时长（秒），默认 8.0。
/// - `fix_time`：固定弹幕（TOP/BOTTOM）时长（秒），默认 4.0。
/// - `font_size_scale`：字号缩放比例（0.5~2.0），默认 1.0（保持原大小）。
/// - `bottom_reserve`：底部保留高度（像素），默认 50.0，避免弹幕遮挡字幕。
/// - `font_family`：ASS 字体，auto 使用当前平台默认字体。
/// - `color_mode` / `color`：保留源颜色或使用统一颜色。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct BurnConfig {
    pub opacity: f64,
    pub scroll_time: f64,
    pub fix_time: f64,
    pub font_size_scale: f64,
    pub bottom_reserve: f64,
    pub font_family: String,
    pub color_mode: String,
    pub color: String,
}

impl Default for BurnConfig {
    fn default() -> Self {
        Self {
            opacity: 0.6,
            scroll_time: 8.0,
            fix_time: 4.0,
            font_size_scale: 1.0,
            bottom_reserve: 50.0,
            font_family: "auto".to_string(),
            color_mode: "source".to_string(),
            color: "FFFFFF".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct SubtitleBurner {
    video_processor: Arc<VideoProcessor>,
    custom_ffmpeg_path: Option<String>,
    burn_config: BurnConfig,
}

impl SubtitleBurner {
    /// 构造带烧录参数的实例；`config` 来自 settings，调用方需自行做范围校验。
    pub fn with_burn_config(
        video_processor: Arc<VideoProcessor>,
        custom_path: Option<String>,
        burn_config: BurnConfig,
    ) -> Self {
        Self {
            video_processor,
            custom_ffmpeg_path: custom_path,
            burn_config,
        }
    }
}

/// 单条待烧录弹幕；直播互动烧录入口也复用该结构（弹幕/SC 转换而来）。
#[derive(Clone)]
pub struct DanmakuItem {
    pub text: String,
    pub time: f64,
    pub mode: String,
    pub size: i32,
    pub color: String,
    pub bottom: bool,
}

#[derive(Clone)]
struct PositionedDanmaku {
    text: String,
    mode: String,
    color: String,
    stime: f64,
    dtime: f64,
    poss_x: f64,
    poss_y: f64,
    posd_x: f64,
    posd_y: f64,
    font_size: i32,
}
