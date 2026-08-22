/**
 * 设置 store：与后端 RuntimeSettings 双向适配。
 *
 * 后端约定（见 src/api/settings.rs 和 src/services/settings.rs）：
 * - GET /api/settings → { current: RuntimeSettings, defaults, constraints, secret_configured }
 *   RuntimeSettings 是嵌套结构（query / danmaku_comment / parallel_download /
 *   aria2_rpc / download_mode / ffmpeg / board / appearance / update / ...）。
 * - PUT /api/settings → SettingsUpdateRequest，body 直接是 RuntimeSettings 展平
 *   （后端 SettingsUpdateRequest 用 #[serde(flatten)] settings: RuntimeSettings）。
 *   服务端会回包 settings_for_response(...)（扁平 RuntimeSettings），所以保存后
 *   响应里 data 就是扁平的 RuntimeSettings（不是套壳）。
 * - GET /api/settings/ffmpeg-path?mode=... → { available, path, source, version }。
 * - POST /api/settings/ffmpeg-test (POST) → { available, path, source, version }
 *   （这里前端用 `ok: boolean` 是旧约定，做兼容）。
 *
 * 迁移前设置行为对齐：
 * - 保存/reset/更新应用后的 toast 优先展示后端 result.message（Aria2 重载失败警告、
 *   Windows 更新暂存提示等），因此写操作统一用 client 的 Full 变体拿 message。
 * - reset/aria2-restart/update check/apply 一律空 body，不传 expected_revision。
 * - ffmpeg-path 检测必须带 mode 参数，切模式重新检测，失败显示"检测失败"。
 *
 * 旧前端模板大量使用扁平字段名（settings.settings.video_max_quality 等），
 * 我们在 store 内部维护"扁平 view"，update(patch) 自动把扁平字段翻译回嵌套
 * 的 RuntimeSettings，save() 时 PUT 整个 RuntimeSettings。
 *
 * 所有 action 内部自带 try/catch，**不向调用者抛 promise reject**。
 */
import { defineStore } from 'pinia';
import { ref } from 'vue';
import { settings as settingsApi, logs as logsApi } from '@/api';
import { getFull, postFull, putFull } from '@/api/client';
import type { Settings, UpdateStatus, LogEntry, FfmpegStatus } from '@/api/types';

/** 后端 RuntimeSettings 嵌套结构。字段命名严格对齐 Rust 端 default_struct! 宏产物。 */
export interface RuntimeSettingsNested {
  revision: number;
  config_version: number;
  query: {
    manual_query_limit: number;
    auto_query_limit: number;
    video_quality: number;
    video_format: number;
    skip_charge_videos: boolean;
    min_video_quality: number;
    prefer_codecs: string[];
    allow_quality_fallback: boolean;
    audio_quality_preference: string;
  };
  danmaku_comment: {
    auto_download_danmaku: boolean;
    auto_download_comments: boolean;
    comments_main_limit: number;
    comments_reply_mode: 'hot3' | 'all' | string;
    comments_filter_regex: string;
    enable_smart_download: boolean;
    min_publish_hours: number;
    download_time_points: number[];
    sidecar_archive_mode: string;
    sidecar_archive_limit: number;
  };
  parallel_download: {
    max_parallel: number;
    wait_slot_timeout: number;
  };
  aria2_rpc: {
    host: string;
    port: number;
    secret: string;
  };
  download_mode: { mode: 'embedded' | 'system' | 'external' | string };
  aria2c_basic: {
    max_connection_per_server: number;
    split: number;
    min_split_size: string;
    max_tries: number;
    retry_wait: number;
    max_concurrent_downloads: number;
    max_overall_download_limit: string;
  };
  storage: {
    history_limit: number;
    log_limit: number;
    per_blogger_retain_default: number;
  };
  download_path: {
    auto_organize: boolean;
    path_template: string;
    conflict_strategy: string;
  };
  ffmpeg: {
    mode: 'auto' | 'system' | 'embedded' | 'custom' | string;
    custom_path: string;
  };
  download: { verify: { mode: string; periodic_days: number; periodic_batch: number; concurrency: number } };
  board: {
    path_display_mode: string;
    show_relative_path: boolean;
    browser_download_enabled: boolean;
  };
  monitor: {
    detect_reupload: boolean;
    scan_page_limit: number;
    multi_page_mode: 'first' | 'all' | string;
  };
  refresh: { l1_interval_minutes: number };
  appearance: { theme: 'system' | 'light' | 'dark' | string };
  burn: {
    opacity: number;
    scroll_time: number;
    fix_time: number;
    font_size_scale: number;
    bottom_reserve: number;
    font_family: string;
    color_mode: string;
    color: string;
  };
  subtitle: { enabled: boolean; accept_ai: boolean; languages: string[] };
  live: {
    max_concurrent: number;
    min_free_space_gib: number;
    max_duration_hours: number;
    file_name_template: string;
  };
  update: { policy: 'auto' | 'manual' | 'off' | string; last_checked_at?: number | null; latest_version?: string | null };
}

const DEFAULT_NESTED: RuntimeSettingsNested = {
  revision: 0,
  config_version: 1,
  query: {
    manual_query_limit: 10,
    auto_query_limit: 3,
    video_quality: 80,
    video_format: 4048,
    skip_charge_videos: true,
    min_video_quality: 64,
    prefer_codecs: ['av1', 'hevc', 'avc'],
    allow_quality_fallback: true,
    audio_quality_preference: 'm4a',
  },
  danmaku_comment: {
    auto_download_danmaku: true,
    auto_download_comments: true,
    comments_main_limit: 30,
    comments_reply_mode: 'hot3',
    comments_filter_regex: '',
    enable_smart_download: true,
    min_publish_hours: 1,
    download_time_points: [1, 5, 24],
    sidecar_archive_mode: 'overwrite',
    sidecar_archive_limit: 3,
  },
  parallel_download: { max_parallel: 3, wait_slot_timeout: 300 },
  aria2_rpc: { host: 'localhost', port: 6800, secret: '' },
  download_mode: { mode: 'embedded' },
  aria2c_basic: {
    max_connection_per_server: 16,
    split: 16,
    min_split_size: '10M',
    max_tries: 5,
    retry_wait: 5,
    max_concurrent_downloads: 3,
    max_overall_download_limit: '0',
  },
  storage: { history_limit: 1000, log_limit: 100, per_blogger_retain_default: 0 },
  download_path: { auto_organize: true, path_template: '{uid}/{title}', conflict_strategy: 'suffix' },
  ffmpeg: { mode: 'auto', custom_path: '' },
  download: { verify: { mode: 'off', periodic_days: 7, periodic_batch: 20, concurrency: 4 } },
  board: { path_display_mode: 'hidden', show_relative_path: false, browser_download_enabled: true },
  monitor: { detect_reupload: true, scan_page_limit: 5, multi_page_mode: 'first' },
  refresh: { l1_interval_minutes: 5 },
  appearance: { theme: 'system' },
  burn: {
    opacity: 0.6,
    scroll_time: 8.0,
    fix_time: 4.0,
    font_size_scale: 1.0,
    bottom_reserve: 50.0,
    font_family: 'auto',
    color_mode: 'source',
    color: 'FFFFFF',
  },
  subtitle: { enabled: true, accept_ai: false, languages: [] },
  live: { max_concurrent: 2, min_free_space_gib: 10, max_duration_hours: 12, file_name_template: '{room_id}_{title}_{date}' },
  update: { policy: 'manual', last_checked_at: null, latest_version: null },
};

/**
 * 把后端 RuntimeSettings 摊平成前端模板用的扁平 view。
 * 字段名与老 JS 模板保持一致（video_max_quality、aria2_host、aria2_secret、…），
 * 烧录参数保留后端原单位，避免设置页显示值与实际配置不一致。
 */
function flatten(n: RuntimeSettingsNested): Settings {
  return {
    // 视频质量
    video_max_quality: n.query.video_quality,
    video_min_quality: n.query.min_video_quality,
    manual_query_limit: n.query.manual_query_limit,
    auto_query_limit: n.query.auto_query_limit,
    skip_charge_videos: n.query.skip_charge_videos,
    allow_quality_fallback: n.query.allow_quality_fallback,
    video_format: n.query.video_format,
    codec_preference: n.query.prefer_codecs.join(','),
    // 弹幕/评论
    auto_download_danmaku: n.danmaku_comment.auto_download_danmaku,
    auto_download_comments: n.danmaku_comment.auto_download_comments,
    comments_main_limit: n.danmaku_comment.comments_main_limit,
    comments_reply_mode: n.danmaku_comment.comments_reply_mode,
    comments_filter_regex: n.danmaku_comment.comments_filter_regex,
    sidecar_archive_mode: n.danmaku_comment.sidecar_archive_mode,
    sidecar_archive_limit: n.danmaku_comment.sidecar_archive_limit,
    // 智能下载
    enable_smart_download: n.danmaku_comment.enable_smart_download,
    min_publish_hours: n.danmaku_comment.min_publish_hours,
    time_points: [...n.danmaku_comment.download_time_points],
    // 并行
    max_parallel_downloads: n.parallel_download.max_parallel,
    wait_slot_timeout: n.parallel_download.wait_slot_timeout,
    // aria2
    aria2_host: n.aria2_rpc.host,
    aria2_port: n.aria2_rpc.port,
    aria2_secret: n.aria2_rpc.secret,
    aria2_mode: n.download_mode.mode,
    aria2_max_conn_per_server: n.aria2c_basic.max_connection_per_server,
    aria2_split: n.aria2c_basic.split,
    aria2_min_split_size: n.aria2c_basic.min_split_size,
    aria2_max_tries: n.aria2c_basic.max_tries,
    aria2_retry_wait: n.aria2c_basic.retry_wait,
    aria2_max_concurrent_downloads: n.aria2c_basic.max_concurrent_downloads,
    // ffmpeg
    ffmpeg_mode: n.ffmpeg.mode,
    ffmpeg_custom_path: n.ffmpeg.custom_path,
    // 模板
    file_naming_template: n.download_path.path_template,
    // 烧录
    burn_opacity: n.burn.opacity,
    burn_font_size_scale: n.burn.font_size_scale,
    burn_scroll_time: n.burn.scroll_time,
    burn_fix_time: n.burn.fix_time,
    burn_bottom_reserve: n.burn.bottom_reserve,
    burn_font_family: n.burn.font_family,
    burn_color_mode: n.burn.color_mode,
    burn_color: n.burn.color,
    // CC 字幕
    subtitle_enabled: n.subtitle.enabled,
    subtitle_accept_ai: n.subtitle.accept_ai,
    subtitle_languages: (n.subtitle.languages || []).join(','),
    // 看板显示
    path_display_mode: n.board.path_display_mode,
    show_relative_path: n.board.show_relative_path,
    // 下载目录整理
    auto_organize: n.download_path.auto_organize,
    conflict_strategy: n.download_path.conflict_strategy,
    // MD5 完整性校验
    verify_mode: n.download.verify.mode,
    verify_periodic_days: n.download.verify.periodic_days,
    verify_periodic_batch: n.download.verify.periodic_batch,
    verify_concurrency: n.download.verify.concurrency,
    // 存储 / 保留
    history_limit: n.storage.history_limit,
    log_limit: n.storage.log_limit,
    per_blogger_retain_default: n.storage.per_blogger_retain_default,
    // 监控行为
    detect_reupload: n.monitor.detect_reupload,
    scan_page_limit: n.monitor.scan_page_limit,
    multi_page_mode: n.monitor.multi_page_mode,
    // 数据刷新
    l1_interval_minutes: n.refresh.l1_interval_minutes,
    // 直播
    live_max_concurrent: n.live.max_concurrent,
    live_min_free_space_gib: n.live.min_free_space_gib,
    live_max_duration_hours: n.live.max_duration_hours,
    live_file_name_template: n.live.file_name_template,
    // 外观
    theme: n.appearance.theme,
    // 浏览器下载
    browser_download_enabled: n.board.browser_download_enabled,
    // 更新
    update_policy: n.update.policy,
    // revision
    revision: n.revision,
  };
}

/** 已知扁平字段名 → 写入路径。集中维护避免散落。 */
const FLAT_TO_NESTED: Record<string, (value: any, n: RuntimeSettingsNested) => void> = {
  video_max_quality: (v, n) => { n.query.video_quality = Number(v); },
  video_min_quality: (v, n) => { n.query.min_video_quality = Number(v); },
  manual_query_limit: (v, n) => { n.query.manual_query_limit = Number(v); },
  auto_query_limit: (v, n) => { n.query.auto_query_limit = Number(v); },
  skip_charge_videos: (v, n) => { n.query.skip_charge_videos = !!v; },
  allow_quality_fallback: (v, n) => { n.query.allow_quality_fallback = !!v; },
  video_format: (v, n) => { n.query.video_format = Number(v); },
  codec_preference: (v, n) => {
    n.query.prefer_codecs = String(v ?? 'av1,hevc,avc').split(',').map(s => s.trim()).filter(Boolean);
  },
  auto_download_danmaku: (v, n) => { n.danmaku_comment.auto_download_danmaku = !!v; },
  auto_download_comments: (v, n) => { n.danmaku_comment.auto_download_comments = !!v; },
  comments_main_limit: (v, n) => { n.danmaku_comment.comments_main_limit = Number(v); },
  comments_reply_mode: (v, n) => { n.danmaku_comment.comments_reply_mode = String(v); },
  comments_filter_regex: (v, n) => { n.danmaku_comment.comments_filter_regex = String(v ?? ''); },
  sidecar_archive_mode: (v, n) => { n.danmaku_comment.sidecar_archive_mode = String(v ?? 'overwrite'); },
  sidecar_archive_limit: (v, n) => { n.danmaku_comment.sidecar_archive_limit = Number(v); },
  enable_smart_download: (v, n) => { n.danmaku_comment.enable_smart_download = !!v; },
  min_publish_hours: (v, n) => { n.danmaku_comment.min_publish_hours = Number(v); },
  time_points: (v, n) => { n.danmaku_comment.download_time_points = Array.isArray(v) ? v.map(Number) : []; },
  max_parallel_downloads: (v, n) => { n.parallel_download.max_parallel = Number(v); },
  wait_slot_timeout: (v, n) => { n.parallel_download.wait_slot_timeout = Number(v); },
  aria2_host: (v, n) => { n.aria2_rpc.host = String(v ?? ''); },
  aria2_port: (v, n) => { n.aria2_rpc.port = Number(v); },
  aria2_secret: (v, n) => { n.aria2_rpc.secret = String(v ?? ''); },
  aria2_mode: (v, n) => { n.download_mode.mode = String(v); },
  aria2_max_conn_per_server: (v, n) => { n.aria2c_basic.max_connection_per_server = Number(v); },
  aria2_split: (v, n) => { n.aria2c_basic.split = Number(v); },
  aria2_min_split_size: (v, n) => { n.aria2c_basic.min_split_size = String(v ?? '10M'); },
  aria2_max_tries: (v, n) => { n.aria2c_basic.max_tries = Number(v); },
  aria2_retry_wait: (v, n) => { n.aria2c_basic.retry_wait = Number(v); },
  aria2_max_concurrent_downloads: (v, n) => { n.aria2c_basic.max_concurrent_downloads = Number(v); },
  ffmpeg_mode: (v, n) => { n.ffmpeg.mode = String(v); },
  ffmpeg_custom_path: (v, n) => { n.ffmpeg.custom_path = String(v ?? ''); },
  file_naming_template: (v, n) => { n.download_path.path_template = String(v ?? ''); },
  burn_opacity: (v, n) => { n.burn.opacity = Math.max(0.1, Math.min(1.0, Number(v))); },
  burn_font_size_scale: (v, n) => { n.burn.font_size_scale = Number(v); },
  burn_scroll_time: (v, n) => { n.burn.scroll_time = Number(v); },
  burn_fix_time: (v, n) => { n.burn.fix_time = Number(v); },
  burn_bottom_reserve: (v, n) => { n.burn.bottom_reserve = Number(v); },
  burn_font_family: (v, n) => { n.burn.font_family = String(v ?? 'auto'); },
  burn_color_mode: (v, n) => { n.burn.color_mode = String(v ?? 'source'); },
  burn_color: (v, n) => { n.burn.color = String(v ?? 'FFFFFF').replace(/^#/, '').toUpperCase(); },
  theme: (v, n) => { n.appearance.theme = String(v) as any; },
  browser_download_enabled: (v, n) => { n.board.browser_download_enabled = !!v; },
  update_policy: (v, n) => { n.update.policy = String(v) as any; },
  live_max_concurrent: (v, n) => { n.live.max_concurrent = Number(v); },
  live_file_name_template: (v, n) => { n.live.file_name_template = String(v ?? ''); },
  subtitle_enabled: (v, n) => { n.subtitle.enabled = !!v; },
  subtitle_accept_ai: (v, n) => { n.subtitle.accept_ai = !!v; },
  subtitle_languages: (v, n) => { n.subtitle.languages = String(v ?? '').split(',').map(s => s.trim()).filter(Boolean); },
  path_display_mode: (v, n) => { n.board.path_display_mode = String(v); },
  show_relative_path: (v, n) => { n.board.show_relative_path = !!v; },
  auto_organize: (v, n) => { n.download_path.auto_organize = !!v; },
  conflict_strategy: (v, n) => { n.download_path.conflict_strategy = String(v); },
  verify_mode: (v, n) => { n.download.verify.mode = String(v); },
  verify_periodic_days: (v, n) => { n.download.verify.periodic_days = Number(v); },
  verify_periodic_batch: (v, n) => { n.download.verify.periodic_batch = Number(v); },
  verify_concurrency: (v, n) => { n.download.verify.concurrency = Number(v); },
  history_limit: (v, n) => { n.storage.history_limit = Number(v); },
  log_limit: (v, n) => { n.storage.log_limit = Number(v); },
  per_blogger_retain_default: (v, n) => { n.storage.per_blogger_retain_default = Number(v); },
  detect_reupload: (v, n) => { n.monitor.detect_reupload = !!v; },
  scan_page_limit: (v, n) => { n.monitor.scan_page_limit = Number(v); },
  multi_page_mode: (v, n) => { n.monitor.multi_page_mode = String(v) as any; },
  l1_interval_minutes: (v, n) => { n.refresh.l1_interval_minutes = Number(v); },
  live_min_free_space_gib: (v, n) => { n.live.min_free_space_gib = Number(v); },
  live_max_duration_hours: (v, n) => { n.live.max_duration_hours = Number(v); },
};

function applyFlatPatch(n: RuntimeSettingsNested, patch: Partial<Settings>) {
  for (const [k, v] of Object.entries(patch)) {
    const fn = FLAT_TO_NESTED[k];
    if (fn) fn(v, n);
  }
}

export const useSettingsStore = defineStore('settings', () => {
  const settings = ref<Settings>(flatten(DEFAULT_NESTED));
  const nested = ref<RuntimeSettingsNested>(JSON.parse(JSON.stringify(DEFAULT_NESTED)));
  const originalNested = ref<RuntimeSettingsNested>(JSON.parse(JSON.stringify(DEFAULT_NESTED)));
  const updateStatus = ref<UpdateStatus>({});
  const logs = ref<LogEntry[]>([]);
  const ffmpegInfo = ref<FfmpegStatus | null>(null);
  /** ffmpeg-path 检测失败标记（老框架：'检测失败' 状态，而不是恒'检测中...'）。 */
  const ffmpegDetectError = ref(false);
  let ffmpegGeneration = 0;
  const loading = ref(false);
  const loadError = ref<string | null>(null);
  const saving = ref(false);
  const dirty = ref(false);

  async function load() {
    loading.value = true;
    loadError.value = null;
    try {
      const payload: any = await settingsApi.get();
      // 响应可能是 { current, defaults, ... } 套壳，也可能是直接 RuntimeSettings
      // （保存接口的响应就是裸的 RuntimeSettings）。
      const raw: RuntimeSettingsNested = payload?.current ?? payload ?? DEFAULT_NESTED;
      const merged: RuntimeSettingsNested = JSON.parse(JSON.stringify(DEFAULT_NESTED));
      Object.assign(merged, raw);
      for (const k of Object.keys(merged.query ?? {})) (merged.query as any)[k] = (raw.query as any)?.[k] ?? (DEFAULT_NESTED.query as any)[k];
      for (const g of Object.keys(DEFAULT_NESTED)) {
        if (typeof (DEFAULT_NESTED as any)[g] === 'object' && !Array.isArray((DEFAULT_NESTED as any)[g])) {
          (merged as any)[g] = { ...(DEFAULT_NESTED as any)[g], ...((raw as any)[g] ?? {}) };
        }
      }
      nested.value = merged;
      originalNested.value = JSON.parse(JSON.stringify(merged));
      settings.value = flatten(merged);
      dirty.value = false;
    } catch (error) {
      loadError.value = error instanceof Error ? error.message : '设置加载失败';
    }
    loading.value = false;
  }

  /** 兼容旧 API：patch 是扁平字段。 */
  function update(patch: Partial<Settings>) {
    applyFlatPatch(nested.value, patch);
    settings.value = flatten(nested.value);
    dirty.value = JSON.stringify(nested.value) !== JSON.stringify(originalNested.value);
  }

  /** 对齐老框架 saveSettings：PUT 带 expected_revision，返回后端 message
   *  （'设置已保存' / '设置已保存，但 Aria2 未能完全重载：...'）供调用方展示。 */
  async function save(): Promise<string> {
    saving.value = true;
    try {
      // 后端 save_settings 返回的就是 settings_for_response(...)（裸 RuntimeSettings）。
      const { data, message } = await putFull<RuntimeSettingsNested>('/api/settings', {
        ...(nested.value as any),
        expected_revision: nested.value.revision,
      });
      const raw: RuntimeSettingsNested = data ?? nested.value;
      nested.value = raw;
      originalNested.value = JSON.parse(JSON.stringify(raw));
      settings.value = flatten(raw);
      dirty.value = false;
      return message || '';
    } finally { saving.value = false; }
  }

  /** 对齐老框架 resetSettings：POST /api/settings/reset 空 body（一律不传版本号）。 */
  async function reset(): Promise<string> {
    const { data, message } = await postFull<RuntimeSettingsNested>('/api/settings/reset', {});
    const raw: RuntimeSettingsNested = data ?? DEFAULT_NESTED;
    nested.value = JSON.parse(JSON.stringify(raw));
    originalNested.value = JSON.parse(JSON.stringify(raw));
    settings.value = flatten(raw);
    dirty.value = false;
    return message || '';
  }

  /** 对齐老框架 restartAria2：POST {}，返回 data { restarted, error } + 后端 message。 */
  async function restartAria2() {
    const { data, message } = await postFull<{ restarted?: boolean; error?: string | null }>('/api/settings/aria2-restart', {});
    return { restarted: !!data?.restarted, error: data?.error, message };
  }

  /** 对齐老框架 testFFmpeg：POST { mode, custom_path }，失败抛错（ApiError 携带后端 message）。 */
  async function testFFmpeg(opts: { mode?: string; custom_path?: string } = {}) {
    return postFull<FfmpegStatus>('/api/settings/ffmpeg-test', opts);
  }

  /** 对齐老框架 refreshFFmpegDetectedPath：GET 带 mode 参数；失败置 ffmpegDetectError
   *  （'检测失败'）；无 path 时 2s 补一次延迟重试；generation 防竞态。
 *  （路径预览是老框架纯前端 replaceAll，见 TabSettings 的 pathPreviewText。） */
  async function refreshFfmpegPath(mode?: string) {
    const generation = ++ffmpegGeneration;
    ffmpegDetectError.value = false;
    try {
      const { data } = await getFull<FfmpegStatus>('/api/settings/ffmpeg-path', {
        mode: mode ?? nested.value.ffmpeg.mode,
      });
      if (generation !== ffmpegGeneration) return;
      if (data) {
        ffmpegInfo.value = {
          available: !!data.available,
          path: data.path ?? '',
          source: data.source ?? '',
          version: data.version ?? '',
        };
        if (!data.path) {
          setTimeout(() => {
            if (generation === ffmpegGeneration) void refreshFfmpegPath(mode);
          }, 2000);
        }
      }
    } catch {
      if (generation !== ffmpegGeneration) return;
      ffmpegInfo.value = null;
      ffmpegDetectError.value = true;
    }
  }

  /** 后端日志条目是 { id, level, msg, uid, bvid, time, timestamp }（秒），
   *  归一化成 LogEntry（毫秒 ts + message + 老框架展示用的 time 字符串）；
   *  limit 上限 100，level 前端过滤；失败静默（老框架轮询静默重试）。 */
  async function loadLogs(limit = 100, level?: string) {
    try {
      const r: any = await logsApi.get(limit);
      const raw: any[] = r && Array.isArray(r.logs) ? r.logs : (Array.isArray(r) ? r : []);
      let list: any[] = raw.map((l: any) => ({
        ts: Number(l.timestamp ?? l.ts ?? 0) * (Number(l.timestamp ?? 0) > 0 ? 1000 : 1),
        time: l.time || '--:--:--',
        level: l.level ?? 'info',
        message: l.msg ?? l.message ?? '',
        uid: l.uid ?? null,
        bvid: l.bvid ?? null,
      }));
      if (level) list = list.filter(l => l.level === level);
      logs.value = list;
    } catch { /* 静默 */ }
  }

  async function loadUpdateStatus() {
    try {
      const { data } = await getFull<UpdateStatus>('/api/update/status');
      if (data) updateStatus.value = data;
      return true;
    } catch { return false; }
  }

  /** 对齐老框架 checkUpdate：不吞错，失败抛出由调用方渲染 resultEl。 */
  async function checkUpdate() {
    const { data } = await postFull<UpdateStatus>('/api/update/check', {});
    if (data) updateStatus.value = data;
    return data;
  }

  /** 对齐老框架 applyUpdate：POST {}，返回后端 message（Windows 暂存提示等）。 */
  async function applyUpdate() {
    return postFull<any>('/api/update/apply', {});
  }

  return {
    settings, updateStatus, logs, ffmpegInfo, ffmpegDetectError, loading, loadError, saving, dirty,
    nested,
    load, update, save, reset, restartAria2, testFFmpeg, refreshFfmpegPath,
    loadLogs, loadUpdateStatus, checkUpdate, applyUpdate,
  };
});
