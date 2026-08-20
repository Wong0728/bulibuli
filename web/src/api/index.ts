/**
 * 后端 API 收敛层：按 Rust 模块分组，所有请求都走 client.ts 的统一 envelope 解析。
 * - GET 用 query
 * - POST 用 body
 * - 二进制流/下载直接用 fetch 返回的 Response
 */
import { get, post } from './client';
import type {
  AuthState, CookieStatus, QrcodeGenerate, QrcodePoll, FoundationStatus, SetupStatus, DetectResult,
  Blogger, SavedBlogger, SearchBloggerResult, Series, VideoItem, ManualResolveResult, VideoUrlsResult, VideoInfo,
  DownloadTask, DownloadHealth, HistoryBoard, HistoryBoardResponse, HistoryEntry, LiveRoom, LiveSource, LiveRecording, LiveDashboard, Settings,
  UpdateStatus, LogEntry,
} from './types';

/** ---------- auth ---------- */
export const auth = {
  state: () => get<AuthState>('/api/auth/state'),
  logout: () => post<{ ok: boolean }>('/api/auth/logout'),
};

/** ---------- foundation ---------- */
export const foundation = {
  status: () => get<FoundationStatus>('/api/foundation/status'),
};

/** ---------- cookies ---------- */
export const cookies = {
  status: () => get<CookieStatus>('/api/cookies/status'),
  // 后端期望的字段是 `cookies`（src/api/cookies.rs::CookiesRequest），
  // 旧前端发 `content` 所以保存永远失败；这里在 client 侧直接对齐。
  save: (content: string) => post<{ ok: boolean }>('/api/cookies/save', { cookies: content }),
  qrcodeGenerate: () => get<QrcodeGenerate>('/api/cookies/qrcode/generate'),
  qrcodePoll: (qrcode_key: string) => get<QrcodePoll>('/api/cookies/qrcode/poll', { qrcode_key }),
};

/** ---------- blogger ----------
 * 后端约定（src/api/blogger/manage.rs + actions.rs + discover.rs）：
 *   - saved/delete 用 { id: i32 }（不是 uid）
 *   - saved/list 返回 { bloggers: [...] }（不是裸数组）
 *   - add 字段大量是字符串/枚举，详见 AddBloggerRequest
 *   - acknowledge 不存在 "all" 路由，批量走 acknowledge-batch 传 uids: Vec<String>
 *   - search 实际是搜索接口，按 keyword GET
 *   - series 返回合集列表
 */
export const blogger = {
  list: () => get<Blogger[]>('/api/blogger/list'),
  savedList: () => get<{ bloggers: SavedBlogger[] }>('/api/blogger/saved/list'),
  savedAdd: (uid: number | string) => post<{ ok: boolean }>('/api/blogger/saved/add', { uid: String(uid) }),
  savedDelete: (id: number) => post<{ ok: boolean }>('/api/blogger/saved/delete', { id }),
  add: (uid: number, config: Partial<Blogger>) => post<Blogger>('/api/blogger/add', { uid: String(uid), ...config }),
  update: (id: number, patch: Partial<Blogger>) => post<Blogger>('/api/blogger/update', { id, ...patch }),
  remove: (id: number) => post<{ ok: boolean }>('/api/blogger/delete', { id }),
  search: (keyword: string) => get<SearchBloggerResult[]>('/api/blogger/search', { keyword }),
  validateUid: (uid: number | string) => get<Blogger>('/api/blogger/validate-uid', { uid: String(uid) }),
  series: (uid: number | string) => get<Series[]>('/api/blogger/series', { uid: String(uid) }),
  seriesVideos: (uid: number | string, seriesId: number, opts: { collection_type?: 'series' | 'season'; offset?: number; limit?: number } = {}) =>
    get<{ items: any[]; has_more?: boolean }>('/api/blogger/series-videos', {
      uid: String(uid),
      series_id: seriesId,
      collection_type: opts.collection_type ?? 'series',
      offset: opts.offset ?? 0,
      limit: opts.limit ?? 30,
    }),
  cleanupNow: (uid: number | string) => post<{ ok: boolean }>('/api/blogger/cleanup-now', { uid: String(uid) }),
  acknowledgeChange: (uid: number | string) => post<{ ok: boolean }>('/api/blogger/acknowledge', { uid: String(uid) }),
  acknowledgeBatch: (uids: Array<number | string>) => post<{ affected: number }>('/api/blogger/acknowledge-batch', { uids: uids.map(String) }),
};

/** ---------- video ----------
 * 后端约定（src/api/video/stream.rs）：
 *   - POST /api/video/resolve  body: { input }    ← 不叫 bvid
 *   - POST /api/video/get-videos body: { uid: String, limit?, offset? }
 *   - POST /api/video/get-video-urls body: { bvid, fnval?, cid? }   ← 没有 qn
 *   - POST /api/video/get-audio-url body: { bvid }                    ← 没有 cid
 *   - GET  /api/video/info  ?bvid=...
 *   - 没有 /api/video/resolve-link —— 用 /api/video/resolve + { input } 替代
 */
export const video = {
  resolve: (bvid: string) => post<any>('/api/video/resolve', { input: bvid }),
  getVideos: (uid: number | string, opts: { limit?: number; offset?: number } = {}) =>
    post<{ items: VideoItem[]; has_more?: boolean; total?: number }>('/api/video/get-videos', { uid: String(uid), ...opts }),
  getVideoUrls: (bvid: string, cid?: number, _qn?: number) =>
    post<VideoUrlsResult>('/api/video/get-video-urls', { bvid, cid, fnval: 4048 }),
  getAudioUrl: (bvid: string) => post<{ url: string; expires_at?: number }>('/api/video/get-audio-url', { bvid }),
  info: (bvid: string) => get<VideoInfo>('/api/video/info', { bvid }),
  gateDownload: (bvid: string) => post<{ ok: boolean; reason?: string }>('/api/video/gate-download', { bvid }),
  /** 解析番剧/课程/单视频链接：后端没有独立路由，复用 /api/video/resolve。 */
  resolveLink: (link: string) => post<any>('/api/video/resolve', { input: link }),
  comments: (bvid: string) => get<{ count: number }>('/api/video/comments', { bvid }),
  danmaku: (bvid: string) => get<{ count: number }>('/api/video/danmaku', { bvid }),
  proxyImage: (url: string) => `/api/video/proxy-image?url=${encodeURIComponent(url)}`,
  downloadCover: (bvid: string) => post<{ ok: boolean; path?: string }>('/api/video/download-cover', { bvid }),
};

/** ---------- download ----------
 * 后端约定（src/api/download/queue_ops.rs）：
 *   POST /api/download/add  body: { bvid, title, url, quality?, type? }
 *   type 默认 "video"，合法值：video / audio / danmaku / comments / cover
 *   start / retry / remove / pause / resume / priority 走 task_id (i32)
 *   没有 pause-all / resume-all 路由 → 前端在 store 里循环
 *   /api/download/status 是按 uid 查的（不是单 task）
 */
export const download = {
  add: (bvid: string, opts: { title: string; url: string; quality?: number; type?: 'video' | 'audio' | 'danmaku' | 'comments' | 'cover' } = { title: '', url: '' }) =>
    post<DownloadTask>('/api/download/add', { bvid, ...opts }),
  start: (task_id: number) => post<DownloadTask>('/api/download/start', { task_id }),
  retry: (task_id: number) => post<DownloadTask>('/api/download/retry', { task_id }),
  retryAll: () => post<{ count: number }>('/api/download/retry-all', {}),
  remove: (task_id: number) => post<{ ok: boolean }>('/api/download/remove', { task_id }),
  pause: (task_id: number) => post<{ ok: boolean }>('/api/download/pause', { task_id }),
  resume: (task_id: number) => post<{ ok: boolean }>('/api/download/resume', { task_id }),
  priority: (task_id: number, priority: number) => post<{ ok: boolean }>('/api/download/priority', { task_id, priority }),
  status: (uid?: string, limit?: number) => get<any>('/api/download/status', { uid, limit }),
  health: () => get<DownloadHealth>('/api/download/health'),
  metrics: () => get<{ statuses: Record<string, number>; error_kinds: Record<string, number>; waiting_retry: number }>('/api/download/metrics'),
  burn: (bvid: string, source: 'danmaku' | 'subtitle' | 'all', history_id?: number | null) =>
    post<{ ok: boolean; task_id?: string }>('/api/download/burn', { bvid, source, history_id: history_id ?? null }),
  proxy: (task_id: number) => `/api/download/proxy?task_id=${task_id}`,
};

/** ---------- history ----------
 * 后端约定（src/api/history/board.rs + file_download.rs）：
 *   - GET  /api/history/list?tab=...&page=...&page_size=...
 *       返回按博主分组的 { items, total, counts, page, page_size, server_time, tab }
 *   - GET  /api/history/list?bvid=...&history_id=...
 *       返回 { server_time, video: { ...files, sidecar, task, blogger, ... } } 单视频详情
 *   - GET  /api/history/file-download?bvid=...&path=...
 *       浏览器下载产物文件（流式响应，非 envelope）
 */
export const history = {
  list: (tab: 'downloading' | 'completed' | 'failed', page = 1, page_size = 50) =>
    get<HistoryBoardResponse>('/api/history/list', { tab, page, page_size }),
  detail: (bvid: string, history_id?: number) =>
    get<{ server_time: number; video: any }>('/api/history/list', { bvid, history_id }),
  byUid: (uid: number, page = 1, page_size = 50) => get<HistoryBoardResponse>('/api/history/by-uid', { uid, page, page_size }),
  search: (keyword: string, tab: 'completed' | 'failed' = 'completed', page = 1) =>
    get<{ items: HistoryEntry[]; total: number }>('/api/history/search', { keyword, tab, page }),
  delete: (id: number) => post<{ ok: boolean }>('/api/history/delete', { id }),
  openDirectory: (id: number) => post<{ ok: boolean }>('/api/history/open-directory', { id }),
  fileDownloadUrl: (bvid: string, path: string) =>
    `/api/history/file-download?bvid=${encodeURIComponent(bvid)}&path=${encodeURIComponent(path)}`,
};

/** ---------- task ----------
 * 后端约定（src/api/task.rs）：
 *   - POST /api/task/start  body: { uid: String }    （uid 是字符串，不是数字）
 *   - POST /api/task/stop   body: { uid: String }
 *   - GET  /api/task/status 无参，返回全局聚合；不要按 uid 查
 *   - GET  /api/task/next-check 无参，返回 { bloggers: { uid: {schedule} }, ... }
 */
export const task = {
  start: (uid: number | string) => post<{ ok: boolean }>('/api/task/start', { uid: String(uid) }),
  stop: (uid: number | string) => post<{ ok: boolean }>('/api/task/stop', { uid: String(uid) }),
  status: () => get<any>('/api/task/status'),
  nextCheck: () => get<{ bloggers: Record<string, any>; server_timestamp: number; server_utc_offset: string }>('/api/task/next-check'),
};

/** ---------- live ---------- */
export const live = {
  roomInfo: (room_id: number) => get<LiveRoom>('/api/live/room-info', { room_id }),
  start: (source_id: number) => post<LiveRecording>('/api/live/start', { source_id }),
  stop: (recording_id: string) => post<{ ok: boolean }>('/api/live/stop', { recording_id }),
  status: (recording_id: string) => get<LiveRecording>('/api/live/status', { recording_id }),
  dashboard: () => get<LiveDashboard>('/api/live/dashboard'),
  sourceAdd: (room_id: number, config: Partial<LiveSource> = {}) => post<LiveSource>('/api/live/source/add', { room_id, ...config }),
  sourceUpdate: (id: number, patch: Partial<LiveSource>) => post<LiveSource>('/api/live/source/update', { id, ...patch }),
  sourceDelete: (id: number) => post<{ ok: boolean }>('/api/live/source/delete', { id }),
  history: (page = 1) => get<{ items: LiveRecording[]; total: number }>('/api/live/history', { page }),
  recovery: () => get<{ recordings: LiveRecording[] }>('/api/live/recovery'),
  events: (recording_id: string) => get<{ events: Array<{ ts: number; category: string; label: string }> }>('/api/live/events', { recording_id }),
  startMerge: (recording_id: string) => post<{ job_id: string }>(`/api/live/history/${recording_id}/merge`, {}),
  mergeJob: (job_id: string) => get<{ status: string; progress?: number; error?: string; output_path?: string }>(`/api/live/merge/${job_id}`),
  cancelMerge: (job_id: string) => post<{ ok: boolean }>(`/api/live/merge/${job_id}/cancel`, {}),
};

/** ---------- settings ---------- */
export const settings = {
  get: () => get<Settings>('/api/settings'),
  save: (s: Settings) => post<Settings>('/api/settings', s),
  reset: () => post<Settings>('/api/settings/reset', {}),
  aria2Restart: () => post<{ ok: boolean }>('/api/settings/aria2-restart'),
  ffmpegPath: () => get<{ path: string; bundled: boolean; mode: string }>('/api/settings/ffmpeg-path'),
  ffmpegTest: (opts: { mode?: string; custom_path?: string }) =>
    post<{ ok: boolean; version?: string; probe?: string; message?: string }>('/api/settings/ffmpeg-test', opts),
  pathPreview: (opts: { template: string; vars: Record<string, string> }) =>
    post<{ preview: string }>('/api/settings/path-preview', opts),
};

/** ---------- logs ----------
 * 后端 3 个日志接口都返回 { logs: [...] } 包装对象
 * （src/api/logs.rs），老前端当数组遍历会拿到 undefined 链。
 */
export const logs = {
  get: (limit = 500, level?: string) => get<{ logs: LogEntry[] }>('/api/logs/get', { limit, level }),
  blogger: (uid: number | string, limit = 500) =>
    get<{ logs: LogEntry[] }>('/api/logs/blogger', { uid: String(uid), limit }),
  bvid: (bvid: string, limit = 500) =>
    get<{ logs: LogEntry[] }>('/api/logs/bvid', { bvid, limit }),
};

/** ---------- refresh ----------
 * 后端 /api/refresh 是 GET + query(kind=board|blogger|video|verify)（src/api/refresh.rs）
 * 旧前端发 {} 空 body，会返回 400。
 */
export const refresh = {
  trigger: (kind: 'board' | 'blogger' | 'video' | 'verify' = 'board', bvid?: string) =>
    get<{ refreshed?: number; bvid?: string; verified?: number }>('/api/refresh', { kind, bvid }),
};

/** ---------- update ---------- */
export const update = {
  status: () => get<UpdateStatus>('/api/update/status'),
  check: () => post<UpdateStatus>('/api/update/check'),
  apply: () => post<{ ok: boolean; need_restart?: boolean }>('/api/update/apply'),
};

/** ---------- setup ----------
 * 后端约定（src/api/setup.rs）：
 *   - GET  /api/setup/status  返回详细 SetupStatus 字段（completed, mode, ...）
 *   - POST /api/setup/apply   body: { mode, access_default?, proxy_domain?, mark_completed? }
 *   - POST /api/setup/ai-skill body: { enabled: bool }   ← 字段名是 enabled，不是 content
 *   - GET  /api/setup/detect  返回 { ipv4, ipv6 }
 *   - GET  /api/setup/ports   返回端口 / URL 信息
 */
export const setup = {
  status: () => get<any>('/api/setup/status'),
  apply: (config: { mode: 'local' | 'lan' | 'proxy'; access_default?: 'allow' | 'deny'; proxy_domain?: string; mark_completed?: boolean }) =>
    post<any>('/api/setup/apply', config),
  aiSkill: (enabled: boolean) => post<{ ai_skill_enabled: boolean }>('/api/setup/ai-skill', { enabled }),
  detect: () => get<{ ipv4: string[]; ipv6: string[] }>('/api/setup/detect'),
  ports: () => get<{ main_port: number; setup_port: number; main_url: string; setup_url: string; accessible_urls: string[] }>('/api/setup/ports'),
};

/** ---------- cover (static) ---------- */
export const cover = {
  url: (bvid: string) => `/api/cover/${bvid}`,
};

/** ---------- health ---------- */
export const health = {
  health: () => get<{ ok: boolean }>('/api/health'),
  ready: () => get<{ ok: boolean }>('/api/ready'),
};

/** ---------- default export for convenience ---------- */
export default {
  auth, foundation, cookies, blogger, video, download, history,
  task, live, settings, logs, refresh, update, setup, cover, health,
};