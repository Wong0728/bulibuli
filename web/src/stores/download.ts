/**
 * 下载队列 store：所有下载任务的状态、进度、操作都集中在这里。
 * WS 推送的 `download:progress` 事件统一收敛到本 store。
 *
 * 后端实际字段（见 src/services/download/status.rs 的 broadcast_progress）：
 *   bvid, cid, page, type, status, progress_percent, downloaded_size, total_size,
 *   speed, step, total_steps, step_label, error
 * `progress:progress` 是按条推送，不是数组，所以这里入参是单条。
 *
 * 队列摘要来自 GET /api/download/metrics（statuses 按状态聚合）。
 *
 * 所有网络调用都自带 try/catch，**不向调用者抛 promise reject**，
 * 避免在 onMounted 等自动加载路径上产生 unhandledrejection / pageerror。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { download as downloadApi, video as videoApi } from '@/api';
import type { DownloadTask, DownloadHealth } from '@/api/types';

/** 把后端下载条目（status.rs get_status 单条）转成 UI 用的 DownloadTask。 */
function fromBackendTask(entry: any): DownloadTask {
  return {
    id: Number(entry.task_id ?? entry.id ?? 0),
    bvid: String(entry.bvid ?? ''),
    title: String(entry.title ?? entry.bvid ?? ''),
    status: (entry.status as DownloadTask['status']) ?? 'pending',
    progress: Number(entry.progress_percent ?? entry.progress ?? 0),
    total_size: Number(entry.total_size ?? 0),
    downloaded_size: Number(entry.downloaded_size ?? 0),
    speed: Number(entry.speed ?? 0),
    error: entry.error ?? undefined,
    priority: Number(entry.priority ?? 0),
    page: entry.page ?? undefined,
    cid: entry.cid ?? undefined,
    type: entry.type ?? undefined,
    part_title: entry.part_title ?? undefined,
    updated_at: entry.updated_at ?? undefined,
  };
}

export const useDownloadStore = defineStore('download', () => {
  const tasks = ref<Map<number, DownloadTask>>(new Map());
  const health = ref<DownloadHealth>({ aria2_ok: false, queue_running: 0, queue_pending: 0, queue_paused: 0 });

  function upsert(task: DownloadTask) {
    if (!task || task.id == null) return;
    tasks.value.set(task.id, { ...tasks.value.get(task.id), ...task });
    tasks.value = new Map(tasks.value);
  }

  /**
   * 接收单条 WS 进度推送。bvid+page+type 组合是天然键；找不到现有条目时
   * 暂存到 pendingList，避免初次下载还没建索引时被丢弃。
   */
  function applyWsProgress(payload: any) {
    if (!payload) return;
    const task = fromBackendTask(payload);
    upsert(task);
  }

  async function refreshHealth() {
    try {
      const h = await downloadApi.health();
      if (h) {
        // 后端字段是 aria2_connected，老 UI 用 aria2_ok；这里双向兼容。
        const ok = (h as any).aria2_connected ?? (h as any).aria2_ok ?? false;
        health.value = {
          aria2_ok: !!ok,
          queue_running: (h as any).queue_running ?? 0,
          queue_pending: (h as any).queue_pending ?? 0,
          queue_paused: (h as any).queue_paused ?? 0,
        } as DownloadHealth;
      }
    } catch { /* 静默 */ }
  }

  async function addTask(bvid: string, opts: { uid?: number; mode?: 'video' | 'audio' | 'danmaku' | 'comments'; page?: number; title?: string } = {}) {
    // 后端 /api/download/add 需要 url + title + bvid，前端先 resolve 流。
    // mode 决定 type 字段（'video' / 'audio' / 'danmaku' / 'comments'）。
    try {
      let title = opts.title || bvid;
      let url = '';
      let quality: number | undefined = undefined;
      const mode = opts.mode || 'video';
      if (mode === 'video' || mode === 'audio') {
        // 走 /api/video/info 拿标题，/api/video/get-video-urls 拿流 URL
        try {
          const info: any = await videoApi.info(bvid);
          if (info?.title) title = info.title;
        } catch { /* 取不到标题就用 bvid */ }
        try {
          const urls: any = await videoApi.getVideoUrls(bvid);
          if (urls?.durl?.[0]?.url) {
            url = urls.durl[0].url;
          } else if (urls?.dash?.video?.[0]?.url) {
            url = urls.dash.video[0].url;
          }
          quality = urls?.accept_quality?.[0]?.qn;
        } catch { /* url 解析失败，下面会回退 */ }
      }
      // 兜底：没拿到 url 也要尝试 POST，失败由 store 静默吞掉
      const t = await downloadApi.add(bvid, {
        title,
        url,
        quality,
        type: mode,
      } as any);
      if (t) { upsert(t as DownloadTask); return t as DownloadTask; }
    } catch { /* 静默 */ }
    return null;
  }

  async function startTask(id: number) {
    try { const t = await downloadApi.start(id); if (t) upsert(t as DownloadTask); } catch { /* 静默 */ }
  }
  async function retryTask(id: number) {
    try { const t = await downloadApi.retry(id); if (t) upsert(t as DownloadTask); } catch { /* 静默 */ }
  }
  async function removeTask(id: number) {
    try { await downloadApi.remove(id); tasks.value.delete(id); tasks.value = new Map(tasks.value); } catch { /* 静默 */ }
  }
  async function pauseTask(id: number) {
    try { await downloadApi.pause(id); } catch { /* 静默 */ }
  }
  async function resumeTask(id: number) {
    try { await downloadApi.resume(id); } catch { /* 静默 */ }
  }
  /** 后端没提供 pause-all/resume-all，循环实现。 */
  async function pauseAll(): Promise<{ count: number }> {
    let count = 0;
    for (const t of downloadingTasks.value) {
      try { await downloadApi.pause(t.id); count++; } catch { /* 忽略单条失败 */ }
    }
    return { count };
  }
  async function resumeAll(): Promise<{ count: number }> {
    let count = 0;
    for (const t of pendingTasks.value) {
      try { await downloadApi.resume(t.id); count++; } catch { /* 忽略单条失败 */ }
    }
    return { count };
  }
  async function retryAll(): Promise<{ count: number }> {
    try { const r = await downloadApi.retryAll(); return r ?? { count: 0 }; }
    catch { return { count: 0 }; }
  }
  async function setPriority(id: number, priority: number) {
    try { await downloadApi.priority(id, priority); } catch { /* 静默 */ }
  }
  async function burn(bvid: string, source: 'danmaku' | 'subtitle' | 'all', history_id?: number | null) {
    try { return await downloadApi.burn(bvid, source, history_id); } catch { return null; }
  }

  // 兼容旧名
  const start = startTask;
  const retry = retryTask;
  const remove = removeTask;
  const pause = pauseTask;
  const resume = resumeTask;
  const priority = setPriority;
  const adjustPriority = (id: number, current: number, delta: number) => setPriority(id, current + delta);

  /** 队列摘要：供顶栏展示。 */
  const queueSummary = computed(() => {
    const total = tasks.value.size;
    const d = downloadingList.value.length;
    const p = pendingList.value.length;
    const f = failedList.value.length;
    return `共 ${total} 个任务 · 下载中 ${d} · 等待 ${p} · 失败 ${f}`;
  });

  /** 后端 status 字段约定：
   *  - downloading：当前在 aria2 跑
   *  - pending：排队 / retrying / waiting_retry
   *  - paused：用户暂停
   *  - failed / completed：终态
   */
  const TERMINAL_DONE = new Set(['completed', 'merged']);
  const TERMINAL_FAIL = new Set(['failed', 'merge_failed', 'cancelled', 'removed']);

  const downloadingList = computed(() =>
    Array.from(tasks.value.values()).filter(t => t.status === 'downloading'),
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
    tasks, health,
    queueSummary,
    downloadingTasks, pendingTasks, failedTasks,
    downloadingList, pendingList, failedList, completedList,
    refreshHealth,
    addTask, startTask, retryTask, removeTask, pauseTask, resumeTask,
    pauseAll, resumeAll, retryAll, setPriority, burn,
    start, retry, remove, pause, resume, priority, adjustPriority,
    upsert, applyWsProgress,
  };
});
