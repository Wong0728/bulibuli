/**
 * 下载队列 store：所有下载任务的状态、进度、操作都集中在这里。
 * WS 推送的 `download:progress` 事件统一收敛到本 store。
 *
 * 后端 WS 载荷字段（见 src/services/download/status.rs 的 broadcast_progress）：
 *   task_id, bvid, cid, page, type, status, progress_percent, downloaded_size,
 *   total_size, speed, step, total_steps, step_label, error
 * **不含 title/priority/version**（HTTP 快照 /api/download/status 才有）——
 * 合并 WS 推送时必须跳过缺失键，否则标题会被 bvid 覆盖、优先级会被 0 覆盖。
 *
 * 对齐老框架（download-queue.js / download-status-store.js / download-status.js）：
 * - 写操作（pause/resume/remove/retry/priority）一律不带 expected_version，
 *   且走 postFull 透出后端 message（toast 优先取后端文案，'success' 视为无自定义）。
 * - 状态/健康/摘要轮询走 fetchShared：in-flight 去重 + TTL 缓存
 *   （status 250ms / health 2000ms），缓存只在成功时写入。
 * - 队列摘要来自 GET /api/download/metrics，随状态刷新一并更新（updateDownloadLists 链）。
 * - aria2 指示点 4 态（connected/connecting/failed/disconnected）+ 点击诊断文案。
 * - 优先级调整：1..=300 钳制，步进由调用方决定，next===current 不发请求。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { download as downloadApi, video as videoApi } from '@/api';
import { postFull } from '@/api/client';
import type { DownloadTask, DownloadHealth } from '@/api/types';

/** 老框架 download-status-store.js 的缓存节奏：status 250ms / health 2000ms。 */
const STATUS_TTL_MS = 250;
const HEALTH_TTL_MS = 2000;

/**
 * 写操作统一封装（老框架 apiPost）：
 * - 不带 expected_version（乐观锁是老框架没有的概念，会引入 409 冲突路径）。
 * - 返回后端 message；后端 ApiResponse::success 的默认 message 是 "success"，视为无自定义文案。
 */
async function postDownloadAction(url: string, body: any): Promise<string | undefined> {
  const { message } = await postFull(url, body);
  return message && message !== 'success' ? message : undefined;
}

/** 把后端下载条目（status.rs get_status 单条）转成 UI 用的 DownloadTask。 */
function fromBackendTask(entry: any): DownloadTask {
  // WS 载荷的 `id` 可能是 ws 层注入的 UUID 消息 id，不是数据库 task_id；
  // 只有有限正整数才是可信的任务 id。
  const rawId = Number(entry.task_id);
  return {
    id: Number.isFinite(rawId) && rawId > 0 ? rawId : 0,
    bvid: String(entry.bvid ?? ''),
    // HTTP 快照后端保证 title 非空（title.unwrap_or(bvid)）；WS 载荷无 title，
    // 由 applyWsProgress 单独构造 patch，不走本函数。
    title: String(entry.title ?? ''),
    status: (entry.status as DownloadTask['status']) ?? 'pending',
    progress: Number(entry.progress_percent ?? entry.progress ?? 0),
    total_size: Number(entry.total_size ?? 0),
    downloaded_size: Number(entry.downloaded_size ?? 0),
    speed: Number(entry.speed ?? 0),
    error: entry.error ?? undefined,
    // 快照缺 priority 时保持 undefined（不覆盖、不归零）。
    priority: Number.isFinite(Number(entry.priority)) ? Number(entry.priority) : undefined,
    page: entry.page ?? undefined,
    cid: entry.cid ?? undefined,
    history_id: entry.history_id ?? undefined,
    type: entry.type ?? undefined,
    part_title: entry.part_title ?? undefined,
    updated_at: entry.updated_at ?? undefined,
    step: entry.step ?? undefined,
    total_steps: entry.total_steps ?? undefined,
    step_label: entry.step_label ?? undefined,
  };
}

export const useDownloadStore = defineStore('download', () => {
  const tasks = ref<Map<number, DownloadTask>>(new Map());
  const health = ref<DownloadHealth>({ aria2_connected: false });
  const statusError = ref<string | null>(null);
  const healthError = ref<string | null>(null);
  /** health 是否加载过（老框架初始 title「正在检测 Aria2 状态...」的解除条件）。 */
  const healthChecked = ref(false);
  const metrics = ref<{ statuses: Record<string, number>; error_kinds: Record<string, number>; waiting_retry: number } | null>(null);
  const metricsError = ref(false);

  // --- fetchShared：in-flight 去重 + TTL 缓存（download-status-store.js 对齐） ---
  // 缓存只在成功时写入；失败不缓存，下次立即重试。
  let statusInFlight: Promise<{ ok: boolean }> | null = null;
  let statusCacheAt = 0;
  let healthInFlight: Promise<{ ok: boolean }> | null = null;
  let healthCacheAt = 0;
  let metricsInFlight: Promise<{ ok: boolean }> | null = null;
  let metricsCacheAt = 0;

  function upsert(task: DownloadTask) {
    if (!task || task.id == null) return;
    tasks.value.set(task.id, { ...tasks.value.get(task.id), ...task });
    tasks.value = new Map(tasks.value);
  }

  /**
   * 接收单条 WS 进度推送（老框架 patchSingleCardProgress 的数据面）。
   * WS 载荷只有增量字段（无 title/priority/version），合并时跳过缺失键：
   * 标题不被 bvid 覆盖、优先级不被 0 覆盖。
   * 找不到对应条目时不新建（避免 UUID 消息 id 变成 NaN 幽灵任务），等 refreshStatus 全量快照补齐。
   */
  function applyWsProgress(payload: any) {
    if (!payload) return;
    const rawId = Number(payload.task_id);
    const incomingId = Number.isFinite(rawId) && rawId > 0 ? rawId : 0;
    const bvid = String(payload.bvid ?? '');
    // WS 载荷必带字段（broadcast_progress 的 json! 固定键）。
    const patch: DownloadTask = {
      id: incomingId,
      bvid,
      title: '',
      status: (payload.status as DownloadTask['status']) ?? 'pending',
      progress: Number(payload.progress_percent ?? 0),
      total_size: Number(payload.total_size ?? 0),
      downloaded_size: Number(payload.downloaded_size ?? 0),
      speed: Number(payload.speed ?? 0),
    };
    // 可选字段：载荷里没有的键不进 patch（合并时保留现有值）。
    if (payload.part_title != null) patch.part_title = String(payload.part_title);
    if (payload.type != null) patch.type = String(payload.type);
    if (payload.cid != null) patch.cid = Number(payload.cid);
    if (payload.page != null) patch.page = Number(payload.page);
    if (payload.step != null) patch.step = Number(payload.step);
    if (payload.total_steps != null) patch.total_steps = Number(payload.total_steps);
    if (payload.step_label != null) patch.step_label = String(payload.step_label);
    if (payload.error != null) patch.error = String(payload.error);

    let key: number | null = incomingId > 0 ? incomingId : null;
    if (key == null) {
      for (const [id, existing] of tasks.value) {
        if (existing.bvid === bvid
          && (existing.type || 'video') === (patch.type || 'video')
          && (existing.page ?? 0) === (patch.page ?? 0)) {
          key = id;
          break;
        }
      }
    }
    if (key == null) return;
    const existing = tasks.value.get(key);
    // title 仅在现有条目缺失时用 bvid 兜底（WS 载荷本身不带标题）。
    const merged: DownloadTask = { ...existing, ...patch, id: key };
    if (existing && !existing.title) merged.title = bvid;
    tasks.value.set(key, merged);
    tasks.value = new Map(tasks.value);
  }

  /**
   * 拉取持久化快照（老框架 updateDownloadLists → fetchDownloadSnapshot）：
   * 250ms TTL + in-flight 共享；成功后顺带刷新队列摘要（updateQueueSummary）。
   */
  async function refreshStatus(): Promise<{ ok: boolean }> {
    const now = Date.now();
    if (statusCacheAt > 0 && now - statusCacheAt <= STATUS_TTL_MS) return { ok: true };
    if (statusInFlight) return statusInFlight;
    statusInFlight = (async () => {
      try {
        const snapshot: any = await downloadApi.status();
        const statuses = snapshot?.statuses;
        if (statuses && typeof statuses === 'object') {
          statusError.value = null;
          const next = new Map<number, DownloadTask>();
          for (const entry of Object.values(statuses as Record<string, any>)) {
            if (entry?.task_id == null) continue;
            const task = fromBackendTask(entry);
            next.set(task.id, task);
          }
          // status 是全量快照；删除已消失任务，避免完成/删除后本地 Map 长期陈旧。
          tasks.value = next;
          statusCacheAt = Date.now();
          // 老框架 updateDownloadLists 末尾：队列摘要随状态轮询一并刷新。
          void refreshMetrics();
          return { ok: true };
        }
        return { ok: true };
      } catch (error) {
        statusError.value = error instanceof Error ? error.message : '下载状态刷新失败';
        return { ok: false };
      } finally {
        statusInFlight = null;
      }
    })();
    return statusInFlight;
  }

  /** 拉取 aria2 健康（老框架 fetchDownloadHealth）：2000ms TTL + in-flight 共享。 */
  async function refreshHealth(): Promise<{ ok: boolean }> {
    const now = Date.now();
    if (healthCacheAt > 0 && now - healthCacheAt <= HEALTH_TTL_MS) return { ok: true };
    if (healthInFlight) return healthInFlight;
    healthInFlight = (async () => {
      try {
        const h = await downloadApi.health();
        if (h) {
          // 后端字段：aria2_connected / aria2_status / aria2_diagnostics。
          health.value = {
            aria2_connected: !!h.aria2_connected,
            aria2_status: h.aria2_status,
            aria2_diagnostics: h.aria2_diagnostics,
          };
          healthError.value = null;
        }
        healthChecked.value = true;
        healthCacheAt = Date.now();
        return { ok: true };
      } catch (error) {
        // 老框架 loadDownloadStatus catch：网络失败时指示点置 disconnected，不给“状态正常”的错觉。
        health.value = { aria2_connected: false, aria2_status: 'disconnected' };
        healthChecked.value = true;
        healthError.value = error instanceof Error ? error.message : 'aria2 状态刷新失败';
        return { ok: false };
      } finally {
        healthInFlight = null;
      }
    })();
    return healthInFlight;
  }

  /** 拉取队列摘要（老框架 updateQueueSummary）：GET /api/download/metrics。 */
  async function refreshMetrics(): Promise<{ ok: boolean }> {
    const now = Date.now();
    if (metricsCacheAt > 0 && now - metricsCacheAt <= STATUS_TTL_MS) return { ok: !metricsError.value };
    if (metricsInFlight) return metricsInFlight;
    metricsInFlight = (async () => {
      try {
        const data = await downloadApi.metrics();
        metrics.value = data ?? { statuses: {}, error_kinds: {}, waiting_retry: 0 };
        metricsError.value = false;
        metricsCacheAt = Date.now();
        return { ok: true };
      } catch {
        metricsError.value = true;
        return { ok: false };
      } finally {
        metricsInFlight = null;
      }
    })();
    return metricsInFlight;
  }

  async function addTask(bvid: string, opts: { uid?: number; mode?: 'video' | 'audio' | 'danmaku' | 'comments'; page?: number; qn?: number; title?: string } = {}) {
    // 后端 /api/download/add 需要 url + title + bvid，前端先 resolve 流。
    // mode 决定 type 字段（'video' / 'audio' / 'danmaku' / 'comments'）。
    try {
      let title = opts.title || bvid;
      let url = '';
      let quality: number | undefined = undefined;
      const mode = opts.mode || 'video';
      if (mode === 'video') {
        // 走 /api/video/info 拿标题，/api/video/get-video-urls 拿流 URL
        try {
          const info: any = await videoApi.info(bvid);
          if (info?.title) title = info.title;
        } catch { /* 取不到标题就用 bvid */ }
        try {
          const urls: any = await videoApi.getVideoUrls(bvid, undefined, opts.qn);
          url = urls?.selected_quality?.url || urls?.qualities?.[0]?.url || '';
          quality = urls?.selected_quality?.quality || urls?.qualities?.[0]?.quality;
        } catch { /* url 解析失败，下面会回退 */ }
      } else if (mode === 'audio') {
        try {
          const audio: any = await videoApi.getAudioUrl(bvid);
          url = audio?.audio_url || audio?.qualities?.[0]?.url || '';
          quality = audio?.qualities?.[0]?.id;
        } catch { /* url 解析失败，下面会回退 */ }
      }
      // 兜底：没拿到 url 也要尝试 POST，失败由 store 静默吞掉
      const result: any = await downloadApi.add(bvid, {
        title,
        url,
        quality,
        type: mode,
      } as any);
      if (result?.download_id != null) {
        const t: DownloadTask = {
          id: Number(result.download_id), bvid, title, status: 'pending',
          type: mode, priority: 0, progress: 0,
        } as DownloadTask;
        upsert(t);
        return t;
      }
      throw new Error('后端未返回下载任务');
    } catch (error) {
      throw error;
    }
  }

  async function startTask(bvid: string, opts: { qn?: number; uid?: string; pages?: any[]; media_type?: string; season_title?: string } = {}) {
    return await downloadApi.start(bvid, opts);
  }

  // --- 写操作：老框架 download-queue.js（一律不带 expected_version） ---

  /** 暂停单个任务；task_id=null 为全局暂停（老框架 pauseDownload / pauseAllDownloads）。 */
  async function pauseDownload(taskId: number | null): Promise<string | undefined> {
    return postDownloadAction('/api/download/pause', { task_id: taskId });
  }
  /** 恢复单个任务；task_id=null 为全局恢复（老框架 resumeDownload / resumeAllDownloads）。 */
  async function resumeDownload(taskId: number | null): Promise<string | undefined> {
    return postDownloadAction('/api/download/resume', { task_id: taskId });
  }
  /** 移除任务（老框架 removeDownload：确认弹窗在调用方，body 只有 bvid+type）。 */
  async function removeDownload(bvid: string, taskType = 'video'): Promise<string | undefined> {
    return postDownloadAction('/api/download/remove', { bvid, type: taskType });
  }
  /** 重试任务（老框架 retryDownload）。 */
  async function retryDownload(bvid: string, taskType = 'video'): Promise<string | undefined> {
    return postDownloadAction('/api/download/retry', { bvid, type: taskType });
  }

  /** 按内部任务 id 找 bvid（老框架写操作全部用 bvid+type 定位任务）。 */
  function bvidOf(value: number | string, type = 'video'): string | null {
    if (typeof value === 'string') return value;
    const task = tasks.value.get(value);
    return task?.bvid ?? null;
  }

  async function retryTask(value: number | string, type = 'video'): Promise<string | undefined> {
    const bvid = bvidOf(value, type);
    if (!bvid) throw new Error('未找到对应下载任务');
    return retryDownload(bvid, type);
  }

  async function removeTask(value: number | string, type = 'video'): Promise<string | undefined> {
    const bvid = bvidOf(value, type);
    if (!bvid) throw new Error('未找到对应下载任务');
    const message = await removeDownload(bvid, type);
    for (const [id, task] of tasks.value) {
      if (task.bvid === bvid && (task.type || 'video') === type) tasks.value.delete(id);
    }
    tasks.value = new Map(tasks.value);
    return message;
  }

  async function pauseTask(id: number): Promise<string | undefined> {
    return pauseDownload(id);
  }
  async function resumeTask(id: number): Promise<string | undefined> {
    return resumeDownload(id);
  }
  /** 后端支持 task_id=null 的原子全局操作，避免逐条循环造成漏暂停/漏恢复。 */
  async function pauseAll(): Promise<string | undefined> {
    return postDownloadAction('/api/download/pause', { task_id: null });
  }
  async function resumeAll(): Promise<string | undefined> {
    return postDownloadAction('/api/download/resume', { task_id: null });
  }
  /** 后端 retry-all 响应只有 { download_id }，没有计数；可重试数量用本地失败列表算。 */
  async function retryAll(): Promise<{ count: number }> {
    const count = failedTasks.value.length;
    await downloadApi.retryAll();
    return { count };
  }

  /**
   * 调整下载优先级（老框架 adjustDownloadPriority）：
   * 1..=300 钳制；next===current 直接返回 null（不发请求）。
   * currentHint 缺省时从内存任务快照取（老框架 _state.currentDownloadStatuses）。
   */
  async function adjustDownloadPriority(bvid: string, delta: number, currentHint?: number | null, taskType = 'video'): Promise<number | null> {
    if (!bvid) return null;
    let current: number | null | undefined = currentHint;
    if (current == null) {
      const task = Array.from(tasks.value.values())
        .find(t => t.bvid === bvid && (t.type || 'video') === taskType);
      current = task?.priority;
    }
    const base = Number(current) || 100;
    const next = Math.min(300, Math.max(1, base + delta));
    if (next === base) return null;
    await postDownloadAction('/api/download/priority', { bvid, type: taskType, priority: next });
    return next;
  }

  /** 直接设定优先级（不钳制；TabHistory 用 adjustDownloadPriority，本方法保留给需要直设的调用方）。 */
  async function setPriority(value: number | string, priority: number, type = 'video'): Promise<void> {
    const bvid = bvidOf(value, type);
    if (!bvid) throw new Error('未找到对应下载任务');
    await postDownloadAction('/api/download/priority', { bvid, type, priority });
  }

  async function burn(bvid: string, source: 'danmaku' | 'subtitle' | 'both', history_id?: number | null) {
    return await downloadApi.burn(bvid, source, history_id);
  }

  // 兼容旧名
  const start = startTask;
  const retry = retryDownload;
  const remove = removeDownload;
  const pause = pauseDownload;
  const resume = resumeDownload;
  const priority = setPriority;
  const adjustPriority = adjustDownloadPriority;

  // --- 队列摘要（老框架 updateQueueSummary 文案，数据来自 /api/download/metrics） ---
  const queueSummary = computed(() => {
    if (metricsError.value) return '队列摘要暂不可用';
    const data = metrics.value;
    if (!data) return '';
    const statuses = data.statuses || {};
    const parts = ([
      ['等待中', Number(statuses.pending) || 0],
      ['下载中', Number(statuses.downloading) || 0],
      ['已暂停', Number(statuses.paused) || 0],
      ['失败', Number(statuses.failed) || 0],
      ['已完成', Number(statuses.completed) || 0],
    ] as Array<[string, number]>).filter(([, count]) => count > 0);
    const waiting = data.waiting_retry ? ` · 等待重试 ${data.waiting_retry}` : '';
    return parts.length
      ? parts.map(([label, count]) => `${label} ${count}`).join(' · ') + waiting
      : '队列空闲';
  });

  // --- aria2 指示点：4 态 + 点击诊断（老框架 updateAria2StatusDot） ---
  function aria2StatusString(): string {
    return health.value.aria2_status
      || (health.value.aria2_connected ? 'connected' : 'disconnected');
  }
  function startingElapsed(): string {
    const ms = (health.value.aria2_diagnostics as Record<string, any>)?.starting_for_ms;
    return Number.isFinite(ms) ? `（${Math.ceil(Number(ms) / 1000)} 秒）` : '';
  }

  /** 指示点 class：connected / connecting / failed / disconnected（初始为 disconnected）。 */
  const aria2DotClass = computed(() => {
    if (!healthChecked.value) return 'disconnected';
    const status = aria2StatusString();
    if (status === 'connected') return 'connected';
    if (status === 'starting') return 'connecting';
    if (status === 'failed') return 'failed';
    return 'disconnected';
  });

  const aria2Title = computed(() => {
    if (!healthChecked.value) return '正在检测 Aria2 状态...';
    const status = aria2StatusString();
    if (status === 'connected') return 'Aria2 已连接（点击查看详情）';
    if (status === 'starting') return `Aria2 正在启动${startingElapsed()}`;
    if (status === 'failed') return 'Aria2 启动失败（点击查看原因）';
    return 'Aria2 未连接（点击查看详情）';
  });

  /** 点击诊断（老框架 dot click 的 toast 载荷）；health 未加载过时无动作。 */
  function diagnoseAria2(): { type: 'success' | 'info' | 'error'; message: string; duration?: number } | null {
    if (!healthChecked.value) return null;
    const diagnostics = (health.value.aria2_diagnostics || {}) as Record<string, any>;
    const status = aria2StatusString();
    if (status === 'connected') {
      return { type: 'success', message: `Aria2 已连接 · ${diagnostics.mode || '下载引擎'} · ${diagnostics.endpoint || '本机 RPC'}` };
    }
    if (status === 'starting') {
      return { type: 'info', message: `Aria2 正在启动${startingElapsed()}，启动超时后会显示具体故障` };
    }
    if (status === 'failed') {
      return { type: 'error', duration: 5000, message: diagnostics.last_error || 'Aria2 进程已退出或启动失败，当前将使用原生下载兜底' };
    }
    return { type: 'error', duration: 5000, message: diagnostics.last_error || 'Aria2 RPC 当前不可达，下载任务会尝试恢复或使用原生兜底' };
  }

  /** 后端 status 字段约定：
   *  - downloading：当前在 aria2 跑
   *  - pending：排队 / retrying / waiting_retry
   *  - paused：用户暂停
   *  - failed / completed：终态
   */
  const TERMINAL_DONE = new Set(['completed', 'merged']);
  const TERMINAL_FAIL = new Set(['failed', 'merge_failed', 'cancelled', 'removed']);

  const downloadingList = computed(() =>
    // paused 属于未完成任务，与看板「下载中」tab 同语义，避免暂停任务在内存列表里隐形。
    Array.from(tasks.value.values()).filter(t =>
      t.status === 'downloading' || t.status === 'paused',
    ),
  );
  const pendingList = computed(() =>
    Array.from(tasks.value.values()).filter(t =>
      t.status === 'pending' || t.status === 'retrying' || t.status === 'waiting_retry',
    ),
  );
  const failedList = computed(() =>
    Array.from(tasks.value.values()).filter(t => TERMINAL_FAIL.has(t.status as string)),
  );
  const completedList = computed(() =>
    Array.from(tasks.value.values()).filter(t => TERMINAL_DONE.has(t.status as string)),
  );

  /** 老字段名兼容：downloadingTasks / pendingTasks / failedTasks。 */
  const downloadingTasks = downloadingList;
  const pendingTasks = pendingList;
  const failedTasks = failedList;

  return {
    tasks, health, statusError, healthError, healthChecked, metrics, metricsError,
    queueSummary,
    aria2DotClass, aria2Title, diagnoseAria2,
    downloadingTasks, pendingTasks, failedTasks,
    downloadingList, pendingList, failedList, completedList,
    refreshHealth, refreshStatus, refreshMetrics,
    addTask, startTask, retryTask, removeTask, pauseTask, resumeTask,
    pauseDownload, resumeDownload, removeDownload, retryDownload,
    pauseAll, resumeAll, retryAll, setPriority, adjustDownloadPriority, burn,
    start, retry, remove, pause, resume, priority, adjustPriority,
    upsert, applyWsProgress,
  };
});
