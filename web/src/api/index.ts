/**
 * 后端 API 收敛层：按 Rust 模块分组，所有请求都走 client.ts 的统一 envelope 解析。
 * - GET 用 query
 * - POST 用 body
 * - 二进制流/下载直接用 fetch 返回的 Response
 */
import { get, post, put, request } from './client';
import type {
  AuthState, CookieStatus, QrcodeGenerate, QrcodePoll, FoundationStatus, SetupStatus, DetectResult,
  Blogger, SavedBlogger, SearchedUser, UserProfileResult, Series, VideoItem, VideoUrlsResult, VideoInfo,
  DownloadHealth, DownloadStatusSnapshot, HistoryBoardResponse, HistoryEntry, LiveRoom, LiveRecording, LiveDashboard, LiveMergeJob,
  SettingsPayload, Settings, FfmpegStatus,
  UpdateStatus, UpdateApplyResult,
  ManualResolveResult, TaskStatusSummary, TaskSchedule, SetupApplyResult, RawLogEntry,
} from './types';

/** ---------- auth ---------- */
export const auth = {
  // 配对刚写入会话 Cookie，入口状态不能读到旧的 GET 缓存。
  state: () => request<AuthState>('/api/auth/state', { method: 'GET', cache: 'no-store' }),
  pair: (code: string, device_name?: string) =>
    post<{ paired: boolean }>('/api/auth/pair', { code, device_name }),
  // 后端响应是 { logged_out: true }（src/api/auth.rs::logout）。
  logout: () => post<{ logged_out: boolean }>('/api/auth/logout'),
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
  // 响应是 { configured: boolean }。
  save: (content: string) => post<{ configured: boolean }>('/api/cookies/save', { cookies: content }),
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
  list: () => get<{ bloggers: Blogger[] }>('/api/blogger/list'),
  savedList: () => get<{ bloggers: SavedBlogger[] }>('/api/blogger/saved/list'),
  savedAdd: (uid: number | string, details: Partial<Pick<SavedBlogger, 'name' | 'face' | 'sign' | 'level' | 'fans'>> = {}) =>
    post<{ blogger: Blogger }>('/api/blogger/saved/add', { uid: String(uid), ...details }),
  // 后端删除/清理/确认类操作 data 恒为空对象（src/api/blogger/manage.rs + actions.rs）。
  savedDelete: (id: number) => post<Record<string, never>>('/api/blogger/saved/delete', { id }),
  add: (uid: number, config: Partial<Blogger>) =>
    post<{ blogger: Blogger }>('/api/blogger/add', { uid: String(uid), ...config }),
  update: (id: number, patch: Partial<Blogger>) =>
    post<{ blogger: Blogger }>('/api/blogger/update', { id, ...patch }),
  remove: (id: number) => post<Record<string, never>>('/api/blogger/delete', { id }),
  search: (q: string) => get<{ users: SearchedUser[]; total: number }>('/api/blogger/search', { q }),
  validateUid: (uid: number | string) => get<UserProfileResult>('/api/blogger/validate-uid', { uid: String(uid) }),
  series: (uid: number | string) => get<{ series: Series[] }>('/api/blogger/series', { uid: String(uid) }),
  seriesVideos: (uid: number | string, seriesId: number, opts: { collection_type?: 'series' | 'season'; offset?: number; limit?: number } = {}) =>
    get<{ videos: VideoItem[]; has_more?: boolean }>('/api/blogger/series-videos', {
      uid: String(uid),
      series_id: seriesId,
      collection_type: opts.collection_type ?? 'series',
      offset: opts.offset ?? 0,
      limit: opts.limit ?? 30,
    }),
  cleanupNow: (uid: number | string) => post<Record<string, never>>('/api/blogger/cleanup-now', { uid: String(uid) }),
  acknowledgeChange: (uid: number | string) => post<Record<string, never>>('/api/blogger/acknowledge', { uid: String(uid) }),
  acknowledgeBatch: (uids: Array<number | string>) => post<{ affected: number }>('/api/blogger/acknowledge-batch', { uids: uids.map(String) }),
};

/** ---------- video ----------
 * 后端约定（src/api/video/stream.rs）：
 *   - POST /api/video/resolve  body: { input }    ← 不叫 bvid
 *   - POST /api/video/get-videos body: { uid: String, limit?, offset? }
 *   - POST /api/video/get-video-urls body: { bvid, qn?, cid?, fnval? }
 *       fnval 不传时后端用 settings.query.video_format，前端不要硬编码覆盖用户设置
 *   - POST /api/video/get-audio-url body: { bvid }                    ← 没有 cid
 *   - GET  /api/video/info  ?bvid=...
 *   - 没有 /api/video/resolve-link —— 用 /api/video/resolve + { input } 替代
 */
export const video = {
  // resolve 同时承接单视频/番剧/课程链接解析，返回 ManualResolveResult（见 types.ts）。
  resolve: (bvid: string) => post<ManualResolveResult>('/api/video/resolve', { input: bvid }),
  getVideos: (uid: number | string, opts: { limit?: number; offset?: number } = {}) =>
    post<{ videos: VideoItem[]; has_more?: boolean; total?: number }>('/api/video/get-videos', { uid: String(uid), ...opts }),
  getVideoUrls: (bvid: string, cid?: number, qn?: number) =>
    post<VideoUrlsResult>('/api/video/get-video-urls', { bvid, cid, qn }),
  getAudioUrl: (bvid: string) => post<{ audio_url: string; qualities?: Array<{ id: number; bandwidth: number; url: string }>; ext?: string }>('/api/video/get-audio-url', { bvid }),
  info: (bvid: string) => get<VideoInfo>('/api/video/info', { bvid }),
  // 后端响应 { allow, state, pay_note, message }（src/api/video/stream.rs::gate_download）。
  gateDownload: (bvid: string) => post<{ allow: boolean; state?: string; pay_note?: string; message?: string }>('/api/video/gate-download', { bvid }),
  /** 解析番剧/课程/单视频链接：后端没有独立路由，复用 /api/video/resolve。 */
  resolveLink: (link: string) => post<ManualResolveResult>('/api/video/resolve', { input: link }),
  // comments / danmaku 返回旁挂内容聚合结构，消费方（VideoDrawer）原样展示，不在此处强约定字段。
  comments: (bvid: string, opts: { path?: string; history_id?: number; uid?: number | string } = {}) =>
    get<unknown>('/api/video/comments', { bvid, ...opts, uid: opts.uid == null ? undefined : String(opts.uid) }),
  danmaku: (bvid: string, path: string, history_id?: number) =>
    get<unknown>('/api/video/danmaku', { bvid, path, history_id }),
  downloadDanmaku: (bvid: string, opts: { source?: string; page?: number; history_id?: number } = {}) =>
    post<{ count: number }>('/api/video/download-danmaku', { bvid, source: opts.source, page: opts.page, history_id: opts.history_id }),
  downloadComments: (bvid: string, opts: { source?: string; history_id?: number } = {}) =>
    post<{ count: number }>('/api/video/download-comments', { bvid, source: opts.source, history_id: opts.history_id }),
  proxyImage: (url: string) => `/api/video/proxy-image?url=${encodeURIComponent(url)}`,
  // 后端响应 { filename, size }（src/api/video/cover.rs）。
  downloadCover: (bvid: string) => post<{ filename: string; size: number }>('/api/video/download-cover', { bvid }),
};

/** ---------- download ----------
 * 后端约定（src/api/download/queue_ops.rs）：
 *   POST /api/download/add  body: { bvid, title, url, quality?, type? }
 *   type 默认 "video"，合法值：video / audio / danmaku / comments / cover
 *   start / retry / remove / priority 使用 bvid/type；pause/resume 使用 task_id
 *   （传 null 表示后端原子执行全局暂停/恢复）。
 *   retry-all 的时间过滤是 query 参数 ?since=，响应是 { download_id }。
 *   /api/download/status 返回按任务键组织的活动快照，可选 uid 过滤。
 *   burn 的 source 合法值是 danmaku / subtitle / both（没有 all）。
 *   proxy 是资源代理：query 必填 url，可选 filename（没有 task_id）。
 */
export const download = {
  add: (bvid: string, opts: { title: string; url: string; quality?: number; type?: 'video' | 'audio' | 'danmaku' | 'comments' | 'cover' } = { title: '', url: '' }) =>
    post<{ download_id: number }>('/api/download/add', { bvid, ...opts }),
  // start 单P 返回 { download_id }，多P/番剧多集返回 { ok_count, total }
  // （src/api/download/queue_ops.rs::start_download / outcome_to_response）。
  start: (bvid: string, opts: { qn?: number; uid?: string; pages?: Array<{ cid?: number; page?: number; part?: string; part_title?: string }>; media_type?: string; season_title?: string } = {}) =>
    post<{ download_id?: number; ok_count?: number; total?: number }>('/api/download/start', { bvid, ...opts }),
  retry: (bvid: string, type = 'video') => post<{ download_id: number }>('/api/download/retry', { bvid, type }),
  retryAll: (since?: number) =>
    post<{ download_id: number }>(`/api/download/retry-all${since != null ? `?since=${encodeURIComponent(since)}` : ''}`, {}),
  remove: (bvid: string, type = 'video') => post<{ download_id: number | null }>('/api/download/remove', { bvid, type }),
  // task_id=null 由后端解释为全局暂停/恢复，旧前端也使用这个契约。
  pause: (task_id: number | null) => post<{ download_id: number | null }>('/api/download/pause', { task_id }),
  resume: (task_id: number | null) => post<{ download_id: number | null }>('/api/download/resume', { task_id }),
  // 后端返回 { success, priority }（src/services/download/status.rs::set_priority）。
  priority: (bvid: string, type: string, priority: number) => post<{ success: boolean; priority: number }>('/api/download/priority', { bvid, type, priority }),
  status: (uid?: string) => get<DownloadStatusSnapshot>('/api/download/status', { uid }),
  health: () => get<DownloadHealth>('/api/download/health'),
  metrics: () => get<{ statuses: Record<string, number>; error_kinds: Record<string, number>; waiting_retry: number }>('/api/download/metrics'),
  burn: (bvid: string, source: 'danmaku' | 'subtitle' | 'both', history_id?: number | null) =>
    post<{ task_id: string }>('/api/download/burn', { bvid, source, history_id: history_id ?? null }),
  burnStatus: (task_id: string) =>
    get<{ task_id: string; status: string; message?: string; output_path?: string }>(`/api/download/burn/status/${encodeURIComponent(task_id)}`),
  proxy: (url: string, filename?: string) =>
    `/api/download/proxy?url=${encodeURIComponent(url)}${filename ? `&filename=${encodeURIComponent(filename)}` : ''}`,
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
  list: (tab: 'downloading' | 'completed' | 'failed', page = 1, page_size = 50, fresh = false) =>
    // fresh 只能传 "true"/"false"：serde_urlencoded 的 bool 不接受 "1"（会 400）。
    get<HistoryBoardResponse>('/api/history/list', { tab, page, page_size, fresh: fresh ? 'true' : undefined }),
  detail: (bvid: string, history_id?: number) =>
    get<{ server_time: number; video: unknown }>('/api/history/list', { bvid, history_id }),
  search: (keyword: string, page = 1, page_size = 50) =>
    get<{ history: HistoryEntry[]; total: number; page: number; page_size: number }>('/api/history/search', { keyword, page, page_size }),
  // 后端返回 { bvid, removed_files, removed_tasks }（src/api/history/crud.rs::delete_history）。
  delete: (bvid: string, history_id?: number, delete_files?: boolean) =>
    post<{ bvid: string; removed_files: boolean; removed_tasks: number }>('/api/history/delete', { bvid, history_id, delete_files }),
  openDirectory: (bvid: string, history_id?: number, path?: string) =>
    post<{ bvid: string }>('/api/history/open-directory', { bvid, history_id, path }),
  fileDownloadUrl: (bvid: string, path: string, history_id?: number) =>
    `/api/history/file-download?bvid=${encodeURIComponent(bvid)}&path=${encodeURIComponent(path)}${history_id == null ? '' : `&history_id=${encodeURIComponent(String(history_id))}`}`,
};

/** ---------- task ----------
 * 后端约定（src/api/task.rs）：
 *   - POST /api/task/start  body: { uid: String }    （uid 是字符串，不是数字）
 *       响应是调度快照 { next_check, schedule, server_timestamp, server_utc_offset }
 *   - POST /api/task/stop   body: { uid: String }，响应 data 为空对象
 *   - GET  /api/task/status 无参，返回全局聚合；不要按 uid 查
 *   - GET  /api/task/next-check 无参，返回 { bloggers: { uid: {schedule} }, ... }
 */
export const task = {
  start: (uid: number | string) =>
    post<{ next_check?: number; schedule?: TaskSchedule; server_timestamp?: number; server_utc_offset?: string }>(
      '/api/task/start', { uid: String(uid) }),
  stop: (uid: number | string) =>
    post<Record<string, never>>('/api/task/stop', { uid: String(uid) }),
  status: () => get<TaskStatusSummary>('/api/task/status'),
  nextCheck: () => get<{ bloggers: Record<string, TaskSchedule>; server_timestamp: number; server_utc_offset: string }>('/api/task/next-check'),
};

/** ---------- live ---------- */
export const live = {
  roomInfo: (room_id: number) => get<LiveRoom>('/api/live/room-info', { room_id }),
  start: (room_id: number) => post<LiveRecording>('/api/live/start', { room_id }),
  // 后端响应 { operation_id, recording_id, status, progress }（停止是异步收尾）。
  stop: (room_id: number) =>
    post<{ operation_id: string; recording_id: number | string; status: string; progress: number }>('/api/live/stop', { room_id }),
  dashboard: () => get<LiveDashboard>('/api/live/dashboard'),
  // status / sourceAdd / sourceUpdate / sourceDelete 已删除：无调用方的死函数
  // （直播源写操作统一走 stores/live.ts 的 addSource/updateSource/deleteSource，
  //  字段映射 auto_record→auto_record_enabled、quality→max_qn 在那里完成）。
  history: (limit = 30) => get<{ items: LiveRecording[]; total?: number }>('/api/live/history', { limit }),
  // 后端返回 { items }（src/api/live/history.rs::recovery）。
  recovery: () => get<{ items: LiveRecording[] }>('/api/live/recovery'),
  events: (room_id: number, recording_id?: number, after_seq = 0, limit = 100) =>
    get<{ recording_id?: string | number; events: Array<Record<string, unknown>>; next_seq?: number }>('/api/live/events', { room_id, recording_id, after_seq, limit }),
  // 后端返回 { task_id, status: "queued" }（src/api/live/mod.rs::burn_recording_danmaku）。
  burnDanmaku: (recording_id: string | number) =>
    post<{ task_id: string; status: string }>(`/api/live/history/${encodeURIComponent(String(recording_id))}/burn-danmaku`, {}),
  startMerge: (recording_id: string) => post<LiveMergeJob>(`/api/live/history/${recording_id}/merge`, {}),
  mergeJob: (job_id: string) => get<LiveMergeJob>(`/api/live/merge/${job_id}`),
  cancelMerge: (job_id: string) => post<LiveMergeJob>(`/api/live/merge/${job_id}/cancel`, {}),
  openDirectory: (recording_id: number) =>
    post<{ recording_id: number }>(`/api/live/history/${recording_id}/open-directory`, {}),
};

/** ---------- settings ----------
 * 后端约定（src/api/settings.rs）：
 *   - GET /api/settings → { current, defaults, constraints, secret_configured }（套壳）
 *   - PUT /api/settings → body 是嵌套 RuntimeSettings 顶层展开 + expected_revision，
 *     响应是裸 RuntimeSettings；reset 同型。
 *   - ffmpeg-path / ffmpeg-test 都返回 { available, path, source, version }。
 *   - path-preview body 是扁平变量字段 { template, title?, uid?, ... }，响应 { path }。
 */
export const settings = {
  get: () => get<SettingsPayload>('/api/settings'),
  save: (nestedSettings: Record<string, unknown>) =>
    put<Settings>('/api/settings', { ...nestedSettings, expected_revision: nestedSettings.revision }),
  reset: () => post<Settings>('/api/settings/reset'),
  aria2Restart: () => post<{ restarted: boolean; error?: string | null; aria2_diagnostics?: unknown }>('/api/settings/aria2-restart'),
  ffmpegPath: () => get<FfmpegStatus>('/api/settings/ffmpeg-path'),
  ffmpegTest: (opts: { mode?: string; custom_path?: string }) => post<FfmpegStatus>('/api/settings/ffmpeg-test', opts),
  // pathPreview 已删除：路径预览是纯前端 replaceAll（TabSettings 的 pathPreviewText），无组件调用。
};

/** ---------- logs ----------
 * 后端 3 个日志接口都返回 { logs: [...] } 包装对象（src/api/logs.rs），
 * 条目字段是 { id, level, msg, uid, bvid, time, timestamp }，由 store 归一化成 LogEntry。
 * 后端 limit 被 clamp 到 1-100，且没有 level 参数（前端需自行过滤）。
 */
export const logs = {
  get: (limit = 100) => get<{ logs: RawLogEntry[] }>('/api/logs/get', { limit: Math.min(limit, 100) }),
  blogger: (uid: number | string, limit = 100) =>
    get<{ logs: RawLogEntry[] }>('/api/logs/blogger', { uid: String(uid), limit: Math.min(limit, 100) }),
  bvid: (bvid: string, limit = 100) =>
    get<{ logs: RawLogEntry[] }>('/api/logs/bvid', { bvid, limit: Math.min(limit, 100) }),
};

/** ---------- refresh ----------
 * 后端 /api/refresh 是 POST + query(kind=board|blogger|video|verify)（src/api/refresh.rs）
 * 旧前端发 {} 空 body，会返回 400。
 */
export const refresh = {
  trigger: (kind: 'board' | 'blogger' | 'video' | 'verify' = 'board', bvid?: string) =>
    post<{ refreshed?: number; bvid?: string; verified?: number }>(
      `/api/refresh?kind=${encodeURIComponent(kind)}${bvid ? `&bvid=${encodeURIComponent(bvid)}` : ''}`,
    ),
};

/** ---------- update ---------- */
export const update = {
  status: () => get<UpdateStatus>('/api/update/status'),
  check: () => post<UpdateStatus>('/api/update/check'),
  // 后端响应 { applied, version }（或 applied:false + current_version）。
  apply: () => post<UpdateApplyResult>('/api/update/apply'),
};

/** ---------- setup ----------
 * 后端约定（src/api/setup.rs）：
 *   - GET  /api/setup/status  返回详细 SetupStatus 字段（completed, mode, ...）
 *   - POST /api/setup/apply   body: { mode, access_default?, proxy_domain?, mark_completed? }
 *   - POST /api/setup/finish  确认前端已消费 apply 响应后关闭一次性 Setup 端口
 *   - POST /api/setup/ai-skill body: { enabled: bool }   ← 字段名是 enabled，不是 content
 *   - GET  /api/setup/detect  返回 { ipv4, ipv6 }
 *   - GET  /api/setup/ports   返回端口 / URL 信息
 */
export const setup = {
  status: () => get<SetupStatus>('/api/setup/status'),
  apply: (config: { mode: 'local' | 'lan' | 'proxy'; access_default?: 'allow' | 'deny'; proxy_domain?: string; mark_completed?: boolean }) =>
    post<SetupApplyResult>('/api/setup/apply', config),
  finish: () => post<{ main_url: string | null; accessible_urls: string[] }>('/api/setup/finish'),
  aiSkill: (enabled: boolean) => post<{ ai_skill_enabled: boolean }>('/api/setup/ai-skill', { enabled }),
  detect: () => get<DetectResult>('/api/setup/detect'),
  ports: () => get<{ main_port: number; setup_port: number; main_url: string | null; setup_url: string | null; accessible_urls: string[] }>('/api/setup/ports'),
};

/** ---------- cover (static) ----------
 * 封面统一走 /api/cover/{bvid}（本地优先 + 兜底下载），history_id 用于同一 bvid 多条记录定位。
 */
export const cover = {
  url: (bvid: string, history_id?: number) =>
    `/api/cover/${bvid}${history_id == null ? '' : `?history_id=${encodeURIComponent(history_id)}`}`,
};

/** ---------- health ---------- */
export const health = {
  health: () => get<{ status: 'ok' | 'degraded'; aria2: boolean; ffmpeg: boolean }>('/api/health'),
  ready: () => get<{ status: 'ok' | 'degraded'; db: boolean; aria2: boolean }>('/api/ready'),
};

/** ---------- default export for convenience ---------- */
export default {
  auth, foundation, cookies, blogger, video, download, history,
  task, live, settings, logs, refresh, update, setup, cover, health,
};
