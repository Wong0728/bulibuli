//! 运行时设置的 DTO：顶层 `RuntimeSettings` 与各设置组结构体、serde 默认值宏，
//! 以及烧录参数到 `BurnConfig` 的转换。

use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: u32 = 1;
pub const SECRET_MASK: &str = "********";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct RuntimeSettings {
    pub revision: u64,
    pub config_version: u32,
    pub query: QuerySettings,
    pub danmaku_comment: DanmakuCommentSettings,
    pub parallel_download: ParallelDownloadSettings,
    pub aria2_rpc: Aria2RpcSettings,
    pub download_mode: DownloadModeSettings,
    pub aria2c_basic: Aria2BasicSettings,
    pub storage: StorageSettings,
    pub download_path: DownloadPathSettings,
    pub ffmpeg: FfmpegSettings,
    pub download: DownloadSettings,
    pub board: BoardSettings,
    pub monitor: MonitorSettings,
    pub refresh: RefreshSettings,
    pub appearance: AppearanceSettings,
    pub burn: BurnSettings,
    pub subtitle: SubtitleSettings,
    pub live: LiveRecordingSettings,
    pub update: UpdateSettings,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            revision: 0,
            config_version: CONFIG_VERSION,
            query: QuerySettings::default(),
            danmaku_comment: DanmakuCommentSettings::default(),
            parallel_download: ParallelDownloadSettings::default(),
            aria2_rpc: Aria2RpcSettings::default(),
            download_mode: DownloadModeSettings::default(),
            aria2c_basic: Aria2BasicSettings::default(),
            storage: StorageSettings::default(),
            download_path: DownloadPathSettings::default(),
            ffmpeg: FfmpegSettings::default(),
            download: DownloadSettings::default(),
            board: BoardSettings::default(),
            monitor: MonitorSettings::default(),
            refresh: RefreshSettings::default(),
            appearance: AppearanceSettings::default(),
            burn: BurnSettings::default(),
            subtitle: SubtitleSettings::default(),
            live: LiveRecordingSettings::default(),
            update: UpdateSettings::default(),
        }
    }
}

/// 声明带 serde 默认值的设置组，并将默认值放在字段定义旁边。
/// 宏只用于运行时设置，避免为这些 DTO 重复编写相同的 `Default` 实现。
macro_rules! default_struct {
    ($name:ident { $($field:ident : $ty:ty = $value:expr),+ $(,)? }) => {
        #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
        #[serde(default)]
        pub struct $name { $(pub $field: $ty),+ }
        impl Default for $name {
            fn default() -> Self { Self { $($field: $value),+ } }
        }
    };
}

default_struct!(QuerySettings {
    manual_query_limit: i32 = 10,
    auto_query_limit: i32 = 3,
    video_quality: i32 = 80,
    video_format: i32 = 4048,
    skip_charge_videos: bool = true,
    min_video_quality: i32 = 64,
    prefer_codecs: Vec<String> = vec!["av1".to_string(), "hevc".to_string(), "avc".to_string()],
    allow_quality_fallback: bool = true,
    // 音轨偏好：m4a（默认最高码率）/ dolby（杜比全景声优先）/ flac（Hi-Res 无损优先）。
    // 命中时 ext 切换为 ec3/flac，合并容器自动切换为 mkv；未命中回退 m4a+mp4（行为不变）。
    audio_quality_preference: String = "m4a".to_string(),
});
default_struct!(DanmakuCommentSettings {
    auto_download_danmaku: bool = true,
    auto_download_comments: bool = true,
    comments_main_limit: i32 = 30,
    comments_reply_mode: String = "hot3".to_string(),
    comments_filter_regex: String = String::new(),
    enable_smart_download: bool = true,
    min_publish_hours: i32 = 1,
    download_time_points: Vec<i32> = vec![1, 5, 24],
    sidecar_archive_mode: String = "overwrite".to_string(),
    sidecar_archive_limit: i32 = 3,
});
default_struct!(ParallelDownloadSettings {
    max_parallel: usize = 3,
    wait_slot_timeout: u64 = 300,
});
default_struct!(Aria2RpcSettings {
    host: String = "localhost".to_string(),
    port: u16 = 6800,
    secret: String = String::new(),
});
default_struct!(DownloadModeSettings {
    mode: String = "embedded".to_string(),
});
default_struct!(Aria2BasicSettings {
    max_connection_per_server: i32 = 16,
    split: i32 = 16,
    min_split_size: String = "10M".to_string(),
    max_tries: i32 = 5,
    retry_wait: i32 = 5,
    max_concurrent_downloads: usize = 3,
    max_overall_download_limit: String = "0".to_string(),
});
default_struct!(StorageSettings {
    history_limit: i64 = 1000,
    log_limit: i64 = 100,
    per_blogger_retain_default: i32 = 0,
});
default_struct!(DownloadPathSettings {
    auto_organize: bool = true,
    path_template: String = "{uid}/{title}".to_string(),
    conflict_strategy: String = "suffix".to_string(),
});
default_struct!(FfmpegSettings {
    mode: String = "auto".to_string(),
    custom_path: String = String::new(),
});
default_struct!(VerifySettings {
    mode: String = "off".to_string(),
    periodic_days: i32 = 7,
    periodic_batch: i32 = 20,
    concurrency: i32 = 4,
});
default_struct!(DownloadSettings {
    verify: VerifySettings = VerifySettings::default(),
});
default_struct!(BoardSettings {
    path_display_mode: String = "hidden".to_string(),
    // 旧版兼容字段；新代码以 path_display_mode 为准。
    show_relative_path: bool = false,
    // 下载管理抽屉：允许把服务器上的视频产物通过浏览器保存到本机（默认开启）。
    browser_download_enabled: bool = true,
});
default_struct!(MonitorSettings {
    detect_reupload: bool = true,
    scan_page_limit: i32 = 5,
    // 多P投稿自动入队策略："first" 仅下 P1（默认，保持存量行为），"all" 下全部分P。
    multi_page_mode: String = "first".to_string(),
});
default_struct!(AppearanceSettings {
    theme: String = "system".to_string(),
});
default_struct!(RefreshSettings {
    l1_interval_minutes: u64 = 5,
});
default_struct!(BurnSettings {
    // 弹幕透明度（0.1~1.0），默认 0.6 与 us-danmaku 一致。
    opacity: f64 = 0.6,
    // 滚动弹幕 R2L 时长（秒），默认 8.0。
    scroll_time: f64 = 8.0,
    // 固定弹幕 TOP/BOTTOM 时长（秒），默认 4.0。
    fix_time: f64 = 4.0,
    // 字号缩放比例（0.5~2.0），默认 1.0（保持原大小）。
    font_size_scale: f64 = 1.0,
    // 底部保留高度（像素，0~200），默认 50.0，避免弹幕遮挡字幕。
    bottom_reserve: f64 = 50.0,
    // ASS 字体：auto 使用当前平台默认字体。
    font_family: String = "auto".to_string(),
    // source 保留原始颜色，uniform 使用 color。
    color_mode: String = "source".to_string(),
    color: String = "FFFFFF".to_string(),
});

default_struct!(SubtitleSettings {
    // 是否启用 CC 字幕自动下载（默认 true）
    enabled: bool = true,
    // 是否接受 AI 自动生成字幕（lan 以 ai- 开头），默认 false
    accept_ai: bool = false,
    // 语言过滤：空数组表示下载全部；非空时只下载 lan 匹配的语言（如 ["zh-CN", "zh-Hans"]）
    languages: Vec<String> = vec![],
});

default_struct!(LiveRecordingSettings {
    // 并发录制上限（多房间同时开播时的资源保护）
    max_concurrent: usize = 2,
    // 磁盘余量安全阈值（GiB）：启动与运行中低于该值时安全停录
    min_free_space_gib: u64 = 10,
    // 单场录制时长上限（小时），防止忘关的异常直播永久占用磁盘
    max_duration_hours: u64 = 12,
    // 录制文件名模板，可用占位符：{room_id} {title} {date} {time}
    file_name_template: String = "{room_id}_{title}_{date}".to_string(),
});

default_struct!(UpdateSettings {
    // 更新策略：auto 检测到新版本自动下载并暂存（不自动重启）/ manual 仅提示（默认）/ off 关闭检测
    policy: String = "manual".to_string(),
    // 上次检查时间（unix 秒），供设置页展示
    last_checked_at: Option<i64> = None,
    // 缓存的最新版本号（如 v2.0.0-alpha.9）；仅提示用途
    latest_version: Option<String> = None,
});

impl BurnSettings {
    /// 转换为 `BurnConfig`，供 `SubtitleBurner::with_burn_config` 使用。
    /// 已通过 `RuntimeSettings::validate` 保证范围合法，此处不再校验。
    pub fn to_burn_config(&self) -> crate::services::subtitle_burner::BurnConfig {
        use crate::services::subtitle_burner::BurnConfig;
        BurnConfig {
            opacity: self.opacity,
            scroll_time: self.scroll_time,
            fix_time: self.fix_time,
            font_size_scale: self.font_size_scale,
            bottom_reserve: self.bottom_reserve,
            font_family: self.font_family.clone(),
            color_mode: self.color_mode.clone(),
            color: self.color.clone(),
        }
    }
}
