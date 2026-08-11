use crate::error::{AppError, AppResult};
use crate::models::setting;
use crate::services::secret_store::SecretStore;
use arc_swap::ArcSwap;
use chrono::Local;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, EntityTrait, Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

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
        }
    }
}

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
                self.query.prefer_audio_quality.as_str(),
                "best" | "high" | "standard"
            )
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
        Ok(())
    }
}

/// Declare a serde-default settings group while keeping its defaults beside
/// the field definitions. This is intentionally local to runtime settings;
/// it avoids repeating identical `Default` implementations for these DTOs.
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
    video_quality: i32 = 112,
    video_format: i32 = 4048,
    skip_charge_videos: bool = true,
    min_video_quality: i32 = 64,
    prefer_codecs: Vec<String> = vec!["av1".to_string(), "hevc".to_string(), "avc".to_string()],
    prefer_audio_quality: String = "best".to_string(),
    allow_quality_fallback: bool = true,
    // 音轨偏好：m4a（默认最高码率）/ dolby（杜比全景声优先）/ flac（Hi-Res 无损优先）。
    // 命中时 ext 切换为 ec3/flac，合并容器自动切换为 mkv；未命中回退 m4a+mp4（行为不变）。
    audio_quality_preference: String = "m4a".to_string(),
});
default_struct!(DanmakuCommentSettings {
    auto_download_danmaku: bool = true,
    auto_download_comments: bool = true,
    danmaku_download_all: bool = true,
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
    uid_history_limit: i64 = 10,
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

#[derive(Clone)]
pub struct SettingsService {
    db: DatabaseConnection,
    current: Arc<ArcSwap<RuntimeSettings>>,
    secret_store: Arc<SecretStore>,
    save_lock: Arc<Mutex<()>>,
}

impl SettingsService {
    pub async fn new(db: DatabaseConnection, secret_store: Arc<SecretStore>) -> AppResult<Self> {
        let mut migrated_plaintext = migrate_legacy_cookie(&db, &secret_store).await?;
        let mut current = load_runtime_settings(&db).await?;
        if !current.aria2_rpc.secret.is_empty() {
            secret_store
                .set("aria2_rpc_secret", &current.aria2_rpc.secret)
                .await?;
            current.aria2_rpc.secret.clear();
            persist_runtime_settings(&db, &current).await?;
            migrated_plaintext = true;
        }
        current.aria2_rpc.secret = secret_store
            .get("aria2_rpc_secret")
            .await?
            .unwrap_or_default();
        current.validate()?;
        if migrated_plaintext {
            secret_store.cleanup_legacy_plaintext().await?;
        }
        Ok(Self {
            db,
            current: Arc::new(ArcSwap::from_pointee(current)),
            secret_store,
            save_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn current(&self) -> Arc<RuntimeSettings> {
        self.current.load_full()
    }

    pub async fn save(&self, settings: RuntimeSettings) -> AppResult<Arc<RuntimeSettings>> {
        self.save_inner(settings, true).await
    }

    async fn save_inner(
        &self,
        mut settings: RuntimeSettings,
        check_revision: bool,
    ) -> AppResult<Arc<RuntimeSettings>> {
        let _save_guard = self.save_lock.lock().await;
        let before = self.current.load_full();
        if check_revision && settings.revision != before.revision {
            return Err(AppError::Conflict(format!(
                "settings revision conflict: expected {}, current {}",
                settings.revision, before.revision
            )));
        }
        let requested_secret = settings.aria2_rpc.secret.clone();
        settings.aria2_rpc.secret = if requested_secret == SECRET_MASK {
            before.aria2_rpc.secret.clone()
        } else {
            requested_secret
        };
        settings.validate()?;
        settings.revision = before.revision.saturating_add(1);
        let previous_secret = before.aria2_rpc.secret.clone();
        self.secret_store
            .set("aria2_rpc_secret", &settings.aria2_rpc.secret)
            .await?;
        if let Err(error) = persist_runtime_settings(&self.db, &settings).await {
            if let Err(rollback) = self
                .secret_store
                .set("aria2_rpc_secret", &previous_secret)
                .await
            {
                return Err(AppError::Internal(format!(
                    "settings save failed and secret rollback failed: {rollback}"
                )));
            }
            return Err(error);
        }
        let settings = Arc::new(settings);
        self.current.store(settings.clone());
        Ok(settings)
    }

    pub async fn reset(&self) -> AppResult<Arc<RuntimeSettings>> {
        self.save_inner(RuntimeSettings::default(), false).await
    }

    pub async fn cookie_header(&self) -> AppResult<String> {
        Ok(self
            .secret_store
            .get("bili_cookie")
            .await?
            .unwrap_or_default())
    }

    pub async fn save_cookie_header(&self, cookies: &str) -> AppResult<()> {
        self.secret_store.set("bili_cookie", cookies).await
    }
}

pub async fn all_settings(db: &DatabaseConnection) -> AppResult<Value> {
    Ok(serde_json::to_value(load_runtime_settings(db).await?)?)
}

async fn load_runtime_settings(db: &DatabaseConnection) -> AppResult<RuntimeSettings> {
    if let Some(row) = setting::Entity::find_by_id("runtime_config")
        .one(db)
        .await?
    {
        if let Some(value) = row.value {
            let mut raw: Value = serde_json::from_str(&value)?;
            migrate_legacy_path_display_mode(&mut raw);
            let mut settings: RuntimeSettings = serde_json::from_value(raw)?;
            settings.config_version = CONFIG_VERSION;
            return Ok(settings);
        }
    }
    let mut merged = serde_json::to_value(RuntimeSettings::default())?;
    let rows = setting::Entity::find().all(db).await?;
    for row in rows {
        if matches!(
            row.key.as_str(),
            "cookies" | "device_cookies" | "runtime_config"
        ) {
            continue;
        }
        let Some(value) = row.value else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(&value) else {
            tracing::warn!(key = %row.key, "忽略无法解析的旧设置");
            continue;
        };
        if let Some(object) = merged.as_object_mut() {
            object.insert(row.key, parsed);
        }
    }
    migrate_legacy_path_display_mode(&mut merged);
    let mut settings: RuntimeSettings = serde_json::from_value(merged)
        .map_err(|error| AppError::Config(format!("迁移旧设置失败: {error}")))?;
    settings.config_version = CONFIG_VERSION;
    settings.validate()?;
    persist_runtime_settings(db, &settings).await?;
    Ok(settings)
}

fn migrate_legacy_path_display_mode(value: &mut Value) {
    let Some(board) = value.get_mut("board").and_then(Value::as_object_mut) else {
        return;
    };
    if board.get("path_display_mode").is_none()
        && board
            .get("show_relative_path")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        board.insert(
            "path_display_mode".to_string(),
            Value::String("relative".to_string()),
        );
    }
}

async fn persist_runtime_settings(
    db: &DatabaseConnection,
    settings: &RuntimeSettings,
) -> AppResult<()> {
    let mut protected = settings.clone();
    protected.aria2_rpc.secret.clear();
    let value = serde_json::to_value(protected)?;
    save_setting_value(db, "runtime_config", value).await
}

async fn migrate_legacy_cookie(
    db: &DatabaseConnection,
    secret_store: &SecretStore,
) -> AppResult<bool> {
    let mut migrated = false;
    for (legacy_key, protected_key) in [
        ("cookies", "bili_cookie"),
        ("device_cookies", "bili_device_cookies"),
    ] {
        let row = setting::Entity::find_by_id(legacy_key).one(db).await?;
        let Some(value) = row.and_then(|row| row.value) else {
            continue;
        };
        if !value.is_empty() {
            secret_store.set(protected_key, &value).await?;
        }
        db.execute_raw(sea_orm::Statement::from_sql_and_values(
            db.get_database_backend(),
            "DELETE FROM settings WHERE key = ?".to_string(),
            [sea_orm::Value::from(legacy_key)],
        ))
        .await?;
        migrated = true;
    }
    Ok(migrated)
}

async fn save_setting_value(db: &DatabaseConnection, key: &str, value: Value) -> AppResult<()> {
    let transaction = db.begin().await?;
    let serialized = match value {
        Value::String(value) if key == "cookies" => value,
        other => serde_json::to_string(&other)?,
    };
    if let Some(existing) = setting::Entity::find_by_id(key).one(&transaction).await? {
        let mut model: setting::ActiveModel = existing.into();
        model.value = Set(Some(serialized));
        model.updated_at = Set(Some(Local::now()));
        model.update(&transaction).await?;
    } else {
        setting::ActiveModel {
            key: Set(key.to_string()),
            value: Set(Some(serialized)),
            updated_at: Set(Some(Local::now())),
        }
        .insert(&transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
