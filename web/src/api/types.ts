export interface ApiError {
  code: number;
  message: string;
  status: number;
  retryable: boolean;
  data?: unknown;
}

export interface Envelope<T> {
  code: number;
  message: string;
  data: T;
}

export interface AuthState {
  authenticated: boolean;
  role?: 'owner' | 'guest' | null;
  user?: { mid?: number; name?: string; face?: string } | null;
}

export interface CookieStatus {
  configured: boolean;
  valid: boolean;
  message?: string;
}

export interface QrcodeGenerate {
  qrcode_key: string;
  url: string;
  image_data?: string;
}

export interface QrcodePoll {
  status: 'pending' | 'scanned' | 'confirmed' | 'expired' | 'failed';
  message?: string;
  cookies?: string;
}

export interface NetworkStatus {
  online: boolean;
  backendAvailable: boolean;
}

export interface FoundationStatus {
  configuration_status: string;
  access_mode: string;
  setup_access: string;
  message?: string;
}

export interface Blogger {
  id: number;
  uid: number;
  name: string;
  face?: string;
  sign?: string;
  videos_count?: number;
  // 监控配置
  enabled?: boolean;
  filter_window_enabled?: boolean;
  filter_windows?: Array<{ start: string; end: string }>;
  // 增量选项
  download_video?: boolean;
  download_danmaku?: boolean;
  download_comments?: boolean;
  download_cover?: boolean;
  burn_after_merge?: boolean;
  // 其它
  next_check_at?: number;
  last_check_at?: number;
  running?: boolean;
  pending_changes?: { name?: string; face?: string };
  // 改名/换头像通知：后端 src/models/blogger.rs::to_api 给出。
  notice_visible?: boolean;
  last_seen_name?: string;
  last_seen_face?: string;
  last_seen_at?: string;
}

export interface SavedBlogger {
  id?: number;
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

export interface SearchBloggerResult {
  uid: number;
  name: string;
  face?: string;
  sign?: string;
  fans?: number;
  videos_count?: number;
}

export interface Series {
  series_id: number;
  title: string;
  count?: number;
}

export interface VideoItem {
  bvid: string;
  aid?: number;
  title: string;
  pic?: string;
  duration?: number;
  pubdate?: number;
  play?: number;
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
  accept_quality: Array<{ qn: number; name: string }>;
  durl?: Array<{ id: number; url: string; size?: number; backup_urls?: string[] }>;
  dash?: {
    video?: Array<{ id: number; url: string; bandwidth?: number; codecs?: string }>;
    audio?: Array<{ id: number; url: string; bandwidth?: number; codecs?: string }>;
  };
  expires_at?: number;
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
  pages?: Array<{ cid: number; part: string; duration?: number }>;
  cid?: number;
}

export interface DownloadTask {
  id: number;
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
}

export interface DownloadHealth {
  aria2_ok: boolean;
  queue_running: number;
  queue_pending: number;
  queue_paused: number;
}

export interface HistoryEntry {
  id: number;
  bvid: string;
  title: string;
  uid?: number | string;
  uploader_name?: string;
  status: string;
  is_completed?: boolean;
  has_danmaku?: boolean;
  has_comments?: boolean;
  has_video?: boolean;
  has_audio?: boolean;
  has_cover?: boolean;
  local_path?: string;
  relative_path?: string;
  downloaded_at?: number;
  duration?: number;
  page?: number;
  cid?: number;
  part_title?: string;
  pic?: string;
  failure?: { message?: string; kind?: string; fallback_reason?: string } | null;
  task?: { status?: string; progress_percent?: number; speed?: number; total_size?: number; downloaded_size?: number; task_id?: number; priority?: number };
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
  runtime?: {
    live_status?: number;
    live_time?: number;
    last_seen_at?: string;
    online?: number;
    [k: string]: any;
  };
}

export interface LiveRecording {
  recording_id: string;
  room_id: number;
  uname?: string;
  title?: string;
  started_at?: number;
  segment_count?: number;
  duration?: number;
  size?: number;
  status?: 'recording' | 'stopped' | 'merging' | 'merged' | 'failed';
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
  stream_quality?: number;
  stream_protocol?: string;
  stream_format?: string;
  stream_codec?: string;
  capture_mode?: string;
  trigger?: string;
  interaction_capture_status?: string;
  interaction_error?: string;
  danmu_unavailable?: boolean;
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
  monitor?: { running?: boolean; last_check_at?: string; [k: string]: any };
  risk_notice?: string;
  merge_jobs?: LiveMergeJob[];
  recovery?: any[];
  disk?: { available_bytes?: number; total_bytes?: number; path_hidden?: boolean };
  synced_at?: string;
  server_now?: string;
  server_timezone?: string;
  poll_interval_secs?: number;
}

export interface Settings {
  // 视频
  video_max_quality?: number;
  video_min_quality?: number;
  video_format?: number;
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
  // ffmpeg
  ffmpeg_mode?: 'auto' | 'system' | 'embedded' | 'custom' | string;
  ffmpeg_custom_path?: string;
  // 模板
  file_naming_template?: string;
  folder_naming_template?: string;
  // 烧录
  burn_font_size?: number;
  burn_opacity?: number;
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
  // MD5 完整性校验
  verify_mode?: 'off' | 'manual' | 'periodic' | string;
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
  [key: string]: any;
}

export interface UpdateStatus {
  current_version?: string;
  latest_version?: string;
  has_update?: boolean;
  update_available?: boolean;
  release_url?: string;
  release_notes?: string;
  last_checked_at?: number;
  policy?: string;
}

export interface LogEntry {
  ts: number;
  level: string;
  message: string;
}

export interface SetupStatus {
  configured: boolean;
  mode?: 'standalone' | 'lan' | 'public';
  access?: string;
  bind_host?: string;
  bind_port?: number;
  ai_skill?: { content?: string };
}

export interface DetectResult {
  network_ok: boolean;
  bilibili_ok?: boolean;
  message?: string;
}