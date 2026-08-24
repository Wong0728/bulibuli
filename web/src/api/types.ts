export interface ApiError {
  code: number;
  message: string;
  status: number;
  retryable: boolean;
  data?: unknown;
  /** 网络层失败（fetch 抛错/响应无法解析）时为 true，对齐老框架 ApiError({offline: true}) 语义。 */
  offline?: boolean;
}

/** 后端统一响应信封 `{ code, message, data }`（client.ts 已校验其结构）。 */
export interface ApiResp<T> {
  code: number;
  message: string;
  data: T;
}

export interface AuthState {
  authenticated: boolean;
  pairing_open?: boolean;
  pairing_expires_at?: number | null;
  /** 服务器时间（Unix 秒）：用于校准倒计时，消除客户端时钟偏差。 */
  server_time?: number;
  role?: 'owner' | 'operator' | 'viewer' | null;
  user?: { mid?: number; name?: string; face?: string } | null;
}

export interface CookieStatus {
  /** 前端归一化字段（由 has_cookies 合成）；后端 status 响应不返回它。 */
  configured?: boolean;
  has_cookies?: boolean;
  valid: boolean;
  state?: 'authenticated' | 'unauthenticated' | 'risk_control' | 'unreachable' | 'malformed' | string;
  /** 后端在上游检查失败时附带的业务码与错误分类。 */
  business_code?: number;
  error_kind?: string;
  mid?: number;
  uname?: string;
  face?: string;
  level?: number;
  vip_status?: number;
  vip_label?: string;
  message?: string;
}

export interface QrcodeGenerate {
  qrcode_key: string;
  url: string;
}

export interface QrcodePoll {
  /** 与 src/api/cookies.rs 的状态机保持一致。 */
  status: 'pending' | 'authenticated' | 'expired' | 'partial';
  message?: string;
  /** B 站上游业务码与最终登录结果标记。 */
  code?: number;
  authenticated?: boolean;
}

export interface NetworkStatus {
  online: boolean;
  backendAvailable: boolean;
}

export interface FoundationStatus {
  configuration_status: string;
  access_mode: string;
  setup_access: string;
  ai_skill_enabled?: boolean;
  ai_skill_path?: string;
  restart_required?: boolean;
}

/**
 * /api/blogger/list 与 /api/blogger/update 的博主条目（src/models/blogger.rs::to_api
 * + manage.rs::blogger_api_value 叠加的调度快照字段）。老框架遗留的
 * running/filter_windows/burn_after_merge/pending_changes 等字段后端从不返回，已清除。
 */
export interface Blogger {
  id: number;
  uid: number;
  version?: number;
  name: string;
  face?: string;
  sign?: string;
  level?: number;
  fans?: number;
  // 监控配置
  monitor_enabled?: boolean;
  min_interval?: number;
  max_interval?: number;
  active_windows?: string[];
  // 增量选项
  download_video?: boolean;
  download_danmaku?: boolean;
  download_comments?: boolean;
  download_cover?: boolean;
  burn_danmaku?: boolean;
  burn_subtitle?: boolean;
  series_filter_regex?: string;
  // 运行状态（to_api + 调度快照）
  is_running?: boolean;
  next_check?: number;
  runtime_state?: string;
  pause_reason?: string | null;
  within_active_window?: boolean;
  next_action_at?: number | null;
  next_action_kind?: string | null;
  is_saved?: boolean;
  has_auto_task?: boolean;
  created_at?: string;
  updated_at?: string;
  // 改名/换头像通知：后端 src/models/blogger.rs::to_api 给出。
  notice_visible?: boolean;
  last_seen_name?: string;
  last_seen_face?: string;
  last_seen_at?: string;
}

export interface SavedBlogger {
  id?: number;
  version?: number;
  uid: number | string;
  name: string;
  face?: string;
  saved_at?: number;
  level?: number;
  fans?: number;
  sign?: string;
  is_saved?: boolean;
  has_auto_task?: boolean;
  last_seen_name?: string;
  last_seen_face?: string;
  last_seen_at?: string;
  notice_visible?: boolean;
}

/** 前端归一化后的搜索结果（store 从后端 SearchedUser 映射而来）。 */
export interface SearchBloggerResult {
  uid: number;
  name: string;
  face?: string;
  sign?: string;
  level?: number;
  fans?: number;
  videos_count?: number;
  uid_exact?: boolean;
}

/** 后端 /api/blogger/search 的原始元素（SearchedUser 序列化）。 */
export interface SearchedUser {
  mid: number;
  uname: string;
  upic: string;
  fans: number;
  level: number;
  sign: string;
  videos: number;
}

/** 后端 /api/blogger/validate-uid 的响应（UserProfile 序列化）。 */
export interface UserProfileResult {
  exists: boolean;
  uid: number;
  name: string;
  face: string;
  sign: string;
  level: number;
  fans: number;
}

export interface Series {
  series_id: number;
  title: string;
  name?: string;
  type?: 'season' | 'series' | string;
  count?: number;
}

export interface VideoItem {
  bvid: string;
  aid?: number;
  title: string;
  pic?: string;
  duration?: number;
  length?: string;
  pubdate?: number;
  created?: number;
  play?: number;
  is_charging_arc?: boolean;
  comment?: number;
  cid?: number;
  // manual 模式相关
  pages?: Array<{ cid: number; part: string; duration?: number }>;
}

export interface ManualResolveResult {
  media_type: 'pgc' | 'cheese' | 'video' | null;
  season_id?: number;
  season_title?: string;
  cover?: string;
  pay_blocked?: boolean;
  pay_reason?: string;
  message?: string;
  current_ep_id?: number;
  default_quality?: number;
  episodes?: Array<{
    ep_id?: number;
    bvid: string;
    aid?: number;
    cid?: number;
    title: string;
    long_title?: string;
    display_title?: string;
    duration?: number;
    section_title?: string;
    pic?: string;
    badge?: string;
  }>;
}

export interface VideoUrlsResult {
  cid: number;
  accept_quality: number[];
  available_qualities?: number[];
  qualities: Array<{ quality: number; quality_name: string; width: number; height: number; url: string; urls?: string[]; size?: number; format?: string; codec?: string }>;
  selected_quality?: VideoUrlsResult['qualities'][number];
  fallback_reason?: string | null;
  default_quality?: number;
}

export interface VideoInfo {
  bvid: string;
  title: string;
  desc?: string;
  pic?: string;
  duration?: number;
  pubdate?: number;
  tname?: string;
  up_name?: string;
  mid?: number;
  owner?: { mid?: number; name?: string; face?: string };
  stat?: { view?: number; danmaku?: number; reply?: number; favorite?: number; coin?: number; share?: number; like?: number };
  pub_timestamp?: number;
  pages?: Array<{ cid: number; page?: number; part: string; duration?: number }>;
  cid?: number;
}

export interface DownloadTask {
  id: number;
  version?: number;
  bvid: string;
  title: string;
  status: 'pending' | 'downloading' | 'completed' | 'failed' | 'paused' | 'burning' | 'retrying' | 'waiting_retry' | 'merge_failed' | 'cancelled' | 'removed' | 'merged' | string;
  progress?: number;
  total_size?: number;
  downloaded_size?: number;
  speed?: number;
  error?: string;
  priority?: number;
  video_url?: string;
  audio_url?: string;
  page?: number;
  history_id?: number;
  uid?: number;
  cid?: number;
  type?: string;
  part_title?: string;
  added_at?: number;
  started_at?: number;
  completed_at?: number;
  updated_at?: string;
  step?: number;
  total_steps?: number;
  step_label?: string;
}

/** 后端 /api/download/health 实际返回（src/services/download/status.rs）。 */
export interface DownloadHealth {
  aria2_connected: boolean;
  aria2_status?: string;
  aria2_diagnostics?: Record<string, unknown>;
}

/** GET /api/download/status 的活动快照：按任务键组织的条目表（下载 store 归一化）。 */
export interface DownloadStatusSnapshot {
  statuses: Record<string, Record<string, unknown>>;
  [key: string]: unknown;
}

/** WS `download:progress` 事件载荷（src/services/download/status.rs::broadcast_progress）。
 *  id 是 socketioxide 注入的消息去重 id（非任务 id）。 */
export interface DownloadProgressEvent {
  id?: string;
  task_id?: number | string;
  bvid?: string;
  cid?: number;
  page?: number;
  part_title?: string;
  type?: string;
  status?: string;
  progress_percent?: number;
  downloaded_size?: number;
  total_size?: number;
  speed?: number;
  step?: number;
  total_steps?: number;
  step_label?: string;
  error?: string;
  message?: string;
  title?: string;
  priority?: number;
}

/** GET /api/task/status 的全局聚合（enabled_tasks / active_tasks / waiting_window_tasks）。 */
export interface TaskStatusSummary {
  enabled_tasks?: number;
  active_tasks?: number;
  waiting_window_tasks?: number;
  [key: string]: unknown;
}

/** GET /api/task/next-check 的 bloggers map 单条调度快照（博主 store 映射 BloggerTaskStatus）。 */
export interface TaskSchedule {
  monitor_enabled?: boolean;
  is_running?: boolean;
  runtime_state?: string;
  pause_reason?: string | null;
  within_active_window?: boolean;
  next_action_at?: number;
  next_check?: number;
  [key: string]: unknown;
}

/** POST /api/setup/apply 的响应。 */
export interface SetupApplyResult {
  mode?: string;
  restart_required?: boolean;
  main_port?: number;
  setup_port?: number;
  main_url?: string | null;
  setup_url?: string | null;
  accessible_urls?: string[];
  [key: string]: unknown;
}

/** GET /api/logs/* 的原始日志条目（store 归一化成 LogEntry 前的后端形态）。 */
export interface RawLogEntry {
  id?: number | string;
  level?: string;
  msg?: string;
  message?: string;
  uid?: number | string;
  bvid?: string;
  time?: string;
  timestamp?: number;
}

export interface HistoryEntry {
  id: number;
  version?: number;
  bvid: string;
  title: string;
  uid?: number | string;
  uploader_name?: string;
  status: string;
  state?: string;
  is_completed?: boolean;
  has_danmaku?: boolean;
  has_comments?: boolean;
  has_video?: boolean;
  has_cover?: boolean;
  local_path?: string;
  relative_path?: string;
  downloaded_at?: number;
  download_time?: string;
  pub_date?: string;
  pub_timestamp?: number;
  view?: number;
  duration?: number;
  page?: number;
  cid?: number;
  part_title?: string;
  pic?: string;
  failure?: { message?: string; kind?: string; fallback_reason?: string } | null;
  reupload_of?: string;
  pay_note?: string;
  md5?: string;
  md5_last_checked_at?: string;
  sha256?: string;
  sha256_last_checked_at?: string;
  can_open_directory?: boolean;
  sidecar?: Record<string, unknown>;
  task?: { status?: string; progress_percent?: number; speed?: number; total_size?: number; downloaded_size?: number; task_id?: number; version?: number; priority?: number; type?: string; error?: string; error_kind?: string; fallback_reason?: string; step?: number; total_steps?: number; step_label?: string };
  burned?: { danmaku?: boolean; subtitle?: boolean };
}

export interface HistoryCounts {
  downloading?: number;
  completed?: number;
  failed?: number;
  removed?: number;
  pay_blocked?: number;
}

export interface HistoryGroup {
  uid: string | number;
  name?: string;
  face?: string;
  last_seen_name?: string;
  last_seen_face?: string;
  last_seen_at?: string;
  notice_visible?: boolean;
  counts?: HistoryCounts;
  videos: HistoryEntry[];
}

export interface HistoryBoardResponse {
  server_time?: number;
  tab?: 'downloading' | 'completed' | 'failed' | string;
  counts?: HistoryCounts;
  page?: number;
  page_size?: number;
  total?: number;
  /** 按博主分组的看板数据，failed/completed 都有。 */
  items: HistoryGroup[];
}

export interface HistoryBoard {
  /** 兼容旧类型的扁平视图（实际由 store 拍平后填充）。新代码应直接读 store 的 groups。 */
  downloading: HistoryEntry[];
  completed: HistoryEntry[];
  failed: HistoryEntry[];
  total: { downloading: number; completed: number; failed: number };
}

export interface LiveRoom {
  room_id: number;
  uid?: number;
  uname?: string;
  title?: string;
  cover?: string;
  area_name?: string;
  live_status?: 0 | 1 | 2;
  live_time?: string;
  // 扩展：来自 /api/live/room-info 实时信息
  short_id?: number;
  anchor_name?: string;
  face?: string;
  live_status_text?: string;
  online?: number;
  user_cover?: string;
  parent_area_name?: string;
  tags?: string;
  is_portrait?: boolean;
  encrypted?: boolean;
  is_recording?: boolean;
  recording_status?: string;
  can_start?: boolean;
  is_saved?: boolean;
}

export interface LiveSource {
  id: number;
  version?: number;
  room_id: number;
  uid?: number;
  uname?: string;
  title?: string;
  live_status?: 0 | 1 | 2;
  enabled?: boolean;
  auto_record?: boolean;
  quality?: number;
  danmaku_mode?: string;
  segment_seconds?: number;
  max_segments?: number;
  // 扩展：来自 /api/live/dashboard 实时源
  anchor_name?: string;
  face?: string;
  cover?: string;
  capture_mode?: 'off' | 'standard' | 'full' | string;
  schedule_all_day?: boolean;
  weekly_schedule?: Record<string, string[]>;
  runtime?: {
    live_status?: number;
    live_time?: number;
    last_seen_at?: string;
    online?: number;
    /** 后端 runtime 节点的检查状态字段（TabLive 告警条/详情面板直接消费）。 */
    last_checked_at?: string;
    next_retry_at?: string;
    error?: string;
    risk_limited?: boolean;
    stale?: boolean;
    [k: string]: unknown;
  };
}

export interface LiveRecording {
  recording_id: string;
  id?: number;
  room_id: number;
  uname?: string;
  title?: string;
  started_at?: number;
  segment_count?: number;
  duration?: number;
  size?: number;
  status?: 'starting' | 'recording' | 'stopping' | 'finalizing' | 'stopped' | 'completed' | 'failed' | 'cancelled';
  // 扩展：来自 RecordingInfo 序列化
  duration_secs?: number;
  file_size?: number;
  danmaku_count?: number;
  unique_user_count?: number;
  free_gift_count?: number;
  paid_gift_count?: number;
  sc_count?: number;
  guard_count?: number;
  peak_watched?: number;
  estimated_paid_value?: number;
  error_msg?: string;
  ended_at?: string;
  has_output?: boolean;
  can_open_directory?: boolean;
  has_events?: boolean;
  has_burned?: boolean;
  stream_quality?: number;
  stream_protocol?: string;
  stream_format?: string;
  stream_codec?: string;
  capture_mode?: string;
  trigger?: string;
  interaction_capture_status?: string;
  interaction_error?: string;
  danmu_unavailable?: boolean;
  is_recoverable?: boolean;
  segment_index?: number;
  restart_attempts?: number;
}

export interface LiveMergeJob {
  id: string;
  recording_id: number;
  status: string;
  progress: number;
  error?: string;
  source_segment_count?: number;
  cancel_requested?: boolean;
  created_at?: string;
  updated_at?: string;
}

export interface LiveDashboard {
  rooms: LiveSource[];
  recordings: LiveRecording[];
  disk_free?: string;
  monitor_running?: boolean;
  last_check_at?: number;
  // 扩展
  sources?: LiveSource[];
  sessions?: LiveRecording[];
  monitor?: {
    running?: boolean;
    last_heartbeat_at?: string;
    last_success_at?: string;
    last_error?: string;
    risk_backoff_until?: string;
    [k: string]: unknown;
  };
  risk_notice?: string;
  merge_jobs?: LiveMergeJob[];
  recovery?: LiveRecording[];
  disk?: { available_bytes?: number; total_bytes?: number; path_hidden?: boolean };
  synced_at?: string;
  server_now?: string;
  server_timezone?: string;
  poll_interval_secs?: number;
}

export interface Settings {
  // 查询
  manual_query_limit?: number;
  auto_query_limit?: number;
  // 视频
  video_max_quality?: number;
  video_min_quality?: number;
  video_format?: number;
  codec_preference?: string;
  video_download_video?: boolean;
  video_download_audio?: boolean;
  video_download_danmaku?: boolean;
  video_download_comments?: boolean;
  video_download_cover?: boolean;
  video_burn_after_merge?: boolean;
  video_burn_subtitle?: boolean;
  video_burn_danmaku?: boolean;
  // 弹幕/评论
  auto_download_danmaku?: boolean;
  auto_download_comments?: boolean;
  comments_main_limit?: number;
  comments_reply_mode?: string;
  comments_filter_regex?: string;
  sidecar_archive_mode?: string;
  sidecar_archive_limit?: number;
  // 智能下载
  enable_smart_download?: boolean;
  min_publish_hours?: number;
  time_points?: number[];
  // 并行
  max_parallel_downloads?: number;
  wait_slot_timeout?: number;
  // aria2
  aria2_host?: string;
  aria2_port?: number;
  aria2_secret?: string;
  aria2_dir?: string;
  aria2_max_concurrent?: number;
  aria2_mode?: string;
  aria2_max_conn_per_server?: number;
  aria2_split?: number;
  aria2_min_split_size?: string;
  aria2_max_tries?: number;
  aria2_retry_wait?: number;
  aria2_max_concurrent_downloads?: number;
  // ffmpeg
  ffmpeg_mode?: 'auto' | 'system' | 'embedded' | 'custom' | string;
  ffmpeg_custom_path?: string;
  // 模板
  file_naming_template?: string;
  folder_naming_template?: string;
  // 烧录
  burn_font_size?: number;
  burn_opacity?: number;
  burn_font_size_scale?: number;
  burn_scroll_time?: number;
  burn_fix_time?: number;
  burn_bottom_reserve?: number;
  burn_font_family?: string;
  burn_color_mode?: string;
  burn_color?: string;
  burn_bottom?: boolean;
  burn_top?: boolean;
  // CC 字幕
  subtitle_enabled?: boolean;
  subtitle_accept_ai?: boolean;
  subtitle_languages?: string;
  // 看板显示
  path_display_mode?: 'hidden' | 'relative' | 'absolute' | string;
  show_relative_path?: boolean;
  // 下载目录整理
  auto_organize?: boolean;
  conflict_strategy?: 'suffix' | 'skip' | 'overwrite' | string;
  // MD5 完整性校验（后端合法值：off / on_completion / periodic，见 src/services/settings.rs）
  verify_mode?: 'off' | 'on_completion' | 'periodic' | string;
  verify_periodic_days?: number;
  verify_periodic_batch?: number;
  verify_concurrency?: number;
  // 存储 / 保留
  history_limit?: number;
  log_limit?: number;
  per_blogger_retain_default?: number;
  // 监控行为
  detect_reupload?: boolean;
  scan_page_limit?: number;
  multi_page_mode?: 'first' | 'all' | string;
  // 数据刷新
  l1_interval_minutes?: number;
  // 直播
  live_max_concurrent?: number;
  live_min_free_space_gib?: number;
  live_max_duration_hours?: number;
  live_file_name_template?: string;
  // 外观
  theme?: 'system' | 'light' | 'dark' | string;
  // 浏览器下载
  browser_download_enabled?: boolean;
  // 更新策略
  update_policy?: 'auto' | 'manual' | 'off' | string;
  // 安全（前端仅占位；真实开关在 security.toml）
  enable_auth?: boolean;
  bind_localhost?: boolean;
  // 其它
  revision?: number;
  [key: string]: unknown;
}

export interface UpdateStatus {
  current_version?: string;
  latest_version?: string;
  has_update?: boolean;
  last_checked_at?: number;
  policy?: string;
  updatable?: boolean;
}

export interface LogEntry {
  /** 毫秒时间戳（store 已从后端 timestamp 秒归一化）。 */
  ts: number;
  level: string;
  message: string;
  uid?: string | null;
  bvid?: string | null;
}

/** 后端 GET /api/setup/status（src/api/setup.rs::SetupStatus）。 */
export interface SetupStatus {
  completed: boolean;
  mode: 'local' | 'lan' | 'proxy' | string;
  configured_mode: string;
  restart_required: boolean;
  ai_skill_enabled: boolean;
  ai_skill_path: string;
  detected_ips: string[];
  main_port: number;
  setup_port: number;
  main_url: string | null;
  accessible_urls: string[];
}

/** 后端 GET /api/setup/detect。 */
export interface DetectResult {
  ipv4: string[];
  ipv6: string[];
}

/** GET /api/settings 的套壳响应。 */
export interface SettingsPayload {
  current: Record<string, unknown>;
  defaults?: Record<string, unknown>;
  constraints?: Record<string, unknown>;
  secret_configured?: boolean;
}

/** GET/POST /api/settings/ffmpeg-path|ffmpeg-test 的响应。 */
export interface FfmpegStatus {
  available: boolean;
  path: string | null;
  source: string;
  version: string | null;
}

/** POST /api/update/apply 的响应。 */
export interface UpdateApplyResult {
  applied: boolean;
  version?: string;
  current_version?: string;
}
