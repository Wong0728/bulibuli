/**
 * 博主 store：博主监控看板 + 详情 + 日志。
 *
 * 注：博主搜索/手动查询的中间结果放在各自 Tab 的本地状态，跨 Tab 才用 store。
 *
 * 所有 action 内部自带 try/catch，**不向调用者抛 promise reject**，
 * 避免 unhandledrejection / pageerror。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { blogger as bloggerApi, task as taskApi } from '@/api';
import type { Blogger, SearchBloggerResult, SavedBlogger, Series } from '@/api/types';

export const useBloggerStore = defineStore('blogger', () => {
  const bloggers = ref<Blogger[]>([]);
  const selectedBloggerId = ref<number | null>(null);
  const savedBloggers = ref<SavedBlogger[]>([]);
  const knownBloggers = ref<SavedBlogger[]>([]);
  const taskStatuses = ref<Record<number, { running: boolean; next_check?: number; last_check?: number; message?: string }>>({});

  const selected = computed(() => bloggers.value.find(b => b.id === selectedBloggerId.value) || null);

  async function refreshList() {
    try {
      const r = await bloggerApi.list();
      if (Array.isArray(r)) bloggers.value = r as Blogger[];
    } catch { /* 静默 */ }
  }

  /** 黄点列表：需要前端展示"知道了"通知的博主（改名/换头像未确认）。 */
  const noticeBloggers = computed(() => bloggers.value.filter(b => b.notice_visible));
  const hasUnackNotices = computed(() => noticeBloggers.value.length > 0);

  async function acknowledgeOne(uid: number) {
    try {
      await bloggerApi.acknowledgeChange(uid);
      const idx = bloggers.value.findIndex(b => b.uid === uid);
      if (idx >= 0) {
        const next = { ...bloggers.value[idx] } as Blogger;
        next.notice_visible = false;
        bloggers.value.splice(idx, 1, next);
      }
    } catch { /* 静默 */ }
  }
  async function acknowledgeAll() {
    const uids = noticeBloggers.value.map(b => Number(b.uid)).filter(Number.isFinite);
    if (!uids.length) return;
    try {
      await bloggerApi.acknowledgeBatch(uids);
      await refreshList();
    } catch { /* 静默 */ }
  }

  async function refreshSaved() {
    try {
      const r: any = await bloggerApi.savedList();
      // 后端返回 { bloggers: [...] }（src/api/blogger/manage.rs::list_saved_bloggers）
      const list: SavedBlogger[] = Array.isArray(r) ? r : (r?.bloggers ?? []);
      savedBloggers.value = list;
      knownBloggers.value = list;
    } catch { /* 静默 */ }
  }

  async function search(keyword: string): Promise<SearchBloggerResult[]> {
    if (!keyword.trim()) return [];
    try {
      const r = await bloggerApi.search(keyword);
      return (Array.isArray(r) ? r : []) as SearchBloggerResult[];
    } catch { return []; }
  }

  async function validateUid(uid: number) {
    try { return await bloggerApi.validateUid(uid); } catch { return null; }
  }

  async function addBlogger(uid: number, config: Partial<Blogger>) {
    try {
      const b = await bloggerApi.add(uid, config);
      await refreshList();
      return b;
    } catch { return null; }
  }

  async function updateBlogger(id: number, patch: Partial<Blogger>) {
    try {
      const b = await bloggerApi.update(id, patch);
      if (b) {
        const idx = bloggers.value.findIndex(x => x.id === id);
        if (idx >= 0) bloggers.value[idx] = b;
      }
      return b;
    } catch { return null; }
  }

  async function deleteBlogger(id: number) {
    try {
      await bloggerApi.remove(id);
      if (selectedBloggerId.value === id) selectedBloggerId.value = null;
      await refreshList();
    } catch { /* 静默 */ }
  }

  async function cleanupNow(uid: number) {
    try { await bloggerApi.cleanupNow(uid); } catch { /* 静默 */ }
  }

  async function fetchSeries(uid: number): Promise<Series[]> {
    try { const r = await bloggerApi.series(uid); return (Array.isArray(r) ? r : []) as Series[]; } catch { return []; }
  }

  async function startTask(uid: number) {
    try { await taskApi.start(uid); await refreshList(); await refreshStatus(uid); } catch { /* 静默 */ }
  }

  async function stopTask(uid: number) {
    try { await taskApi.stop(uid); await refreshList(); await refreshStatus(uid); } catch { /* 静默 */ }
  }

  /** 拉取一次所有博主的状态，缓存到 taskStatuses map（key 是数字 uid）。 */
  async function refreshAllStatus() {
    try {
      // /api/task/status 是全局聚合（enabled_tasks / active_tasks / waiting_window_tasks）；
      // 真正按博主的状态在 /api/task/next-check 的 bloggers map 里。
      const r: any = await taskApi.nextCheck();
      if (r && r.bloggers) {
        const map: Record<number, { running: boolean; next_check?: number; message?: string }> = {};
        for (const [k, v] of Object.entries(r.bloggers as Record<string, any>)) {
          const u = Number(k);
          if (!Number.isFinite(u)) continue;
          map[u] = {
            running: !!v.is_running,
            next_check: v.next_action_at ?? v.next_check ?? 0,
            message: v.runtime_state === 'waiting_window' ? '等待时段' : (v.pause_reason ?? ''),
          };
        }
        taskStatuses.value = { ...map, ...taskStatuses.value };
      }
    } catch { /* 静默 */ }
  }

  async function refreshStatus(_uid: number) {
    // 兼容旧签名：每次都拉全量，简单可靠
    await refreshAllStatus();
  }

  function selectBlogger(id: number | null) {
    selectedBloggerId.value = id;
  }

  async function saveKnownBlogger(uid: number) {
    try { await bloggerApi.savedAdd(uid); await refreshSaved(); } catch { /* 静默 */ }
  }

  const boardSummary = computed(() => {
    const total = bloggers.value.length;
    const running = bloggers.value.filter(b => taskStatuses.value[b.uid]?.running).length;
    return `${total} 位博主 · ${running} 个任务运行中`;
  });

  return {
    bloggers, selectedBloggerId, savedBloggers, knownBloggers, taskStatuses, boardSummary,
    selected,
    noticeBloggers, hasUnackNotices,
    refreshList, refreshSaved, search, validateUid, addBlogger, updateBlogger, deleteBlogger,
    cleanupNow, fetchSeries, startTask, stopTask, refreshStatus, refreshAllStatus, selectBlogger, saveKnownBlogger,
    acknowledgeOne, acknowledgeAll,
  };
});
