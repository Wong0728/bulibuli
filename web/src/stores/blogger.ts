/**
 * 博主 store：博主监控看板 + 详情 + 任务状态。
 *
 * 注：博主搜索/手动查询的中间结果放在各自 Tab 的本地状态，跨 Tab 才用 store。
 *
 * 对齐老框架（static/js/blogger.js）：
 * - 写操作（删除/更新/启停）一律不传 expected_version（老框架 apiPost 只传业务字段）。
 * - 状态轮询走 GET /api/task/next-check 的 bloggers map
 *   （monitor_enabled / runtime_state / pause_reason / next_action_at，含四态）。
 * - refreshList / refreshAllStatus 内部消化错误（老框架 loadBloggersFromServer 的 catch 静默、
 *   startStatusPolling 的"静默处理网络错误"），不向调用者抛 promise reject，
 *   避免 unhandledrejection / pageerror。
 * - "博主资料变更通知"（黄点/弹窗）只属于搜索页，由 TabSearch 基于 savedBloggers
 *   自行派生（notice_visible 过滤）；本 store 不维护通知派生数据。
 * - last_check / message 字段：后端 next-check 不返回、老框架侧边栏与详情面板
 *   也不展示 → 不保留（老框架只展示"下次检查"倒计时）。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { blogger as bloggerApi, task as taskApi } from '@/api';
import { postFull } from '@/api/client';
import type { Blogger, SearchBloggerResult, SavedBlogger, Series } from '@/api/types';

/** 单博主运行态快照：/api/task/next-check 的 schedule 字段（对齐老框架 bloggerStates 的运行态子集）。 */
export interface BloggerTaskStatus {
  /** 老框架 isRunning：monitor_enabled ?? is_running。 */
  running: boolean;
  /** 运行态四态：scheduled / checking / waiting_window / stopped。 */
  runtime_state?: string;
  pause_reason?: string | null;
  within_active_window?: boolean;
  /** 老框架 nextCheckTime：next_action_at ?? next_check。 */
  next_check?: number;
}

export const useBloggerStore = defineStore('blogger', () => {
  const bloggers = ref<Blogger[]>([]);
  const selectedBloggerId = ref<number | null>(null);
  const savedBloggers = ref<SavedBlogger[]>([]);
  const listError = ref<string | null>(null);
  const savedError = ref<string | null>(null);
  const seriesError = ref<string | null>(null);
  const taskStatuses = ref<Record<number, BloggerTaskStatus>>({});
  /** 老框架 _state.serverUtcOffset：策略摘要"检查时段"后缀（UTC+08:00）。 */
  const serverUtcOffset = ref('');
  let statusRequest: Promise<void> | null = null;

  const selected = computed(() => bloggers.value.find(b => b.id === selectedBloggerId.value) || null);

  /** 对齐老框架 loadBloggersFromServer：catch 只 console.error（不 toast、不 rethrow），
   *  页面加载时的失败不打扰用户；调用方（进页刷新/按钮刷新）都能安全 await。 */
  async function refreshList() {
    try {
      const r = await bloggerApi.list();
      bloggers.value = Array.isArray((r as any)?.bloggers) ? (r as any).bloggers as Blogger[] : [];
      serverUtcOffset.value = (r as any)?.server_utc_offset || serverUtcOffset.value || '';
      listError.value = null;
    } catch (error) {
      listError.value = error instanceof Error ? error.message : '博主列表加载失败';
      console.error('加载博主列表失败:', error);
    }
  }

  async function refreshSaved() {
    try {
      const r: any = await bloggerApi.savedList();
      // 后端返回 { bloggers: [...] }（src/api/blogger/manage.rs::list_saved_bloggers）
      const list: SavedBlogger[] = Array.isArray(r) ? r : (r?.bloggers ?? []);
      savedBloggers.value = list;
      savedError.value = null;
    } catch (error) {
      savedError.value = error instanceof Error ? error.message : '已添加博主加载失败';
      throw error;
    }
  }

  async function search(keyword: string): Promise<SearchBloggerResult[]> {
    if (!keyword.trim()) return [];
    const r = await bloggerApi.search(keyword);
    return (Array.isArray((r as any)?.users) ? (r as any).users : [])
      .map((user: any) => ({
        uid: Number(user.uid ?? user.mid),
        name: user.name ?? user.uname ?? '',
        face: user.face ?? user.upic ?? '',
        sign: user.sign ?? user.usign ?? '',
        level: Number(user.level ?? 0),
        fans: Number(user.fans ?? 0),
        videos_count: Number(user.videos_count ?? user.videos ?? 0),
      }))
      .filter((user: SearchBloggerResult) => Number.isFinite(user.uid));
  }

  async function validateUid(uid: number) {
    return bloggerApi.validateUid(uid);
  }

  async function addBlogger(uid: number, config: Partial<Blogger>) {
    const r: any = await bloggerApi.add(uid, config);
    await refreshList();
    return r?.blogger ?? null;
  }

  /** 老框架 confirmEditBlogger：POST /api/blogger/update 只带业务字段（无 expected_version）。
   *  postFull 保留后端 message（"博主配置已更新"）供调用方 toast 优先使用。 */
  async function updateBlogger(id: number, patch: Partial<Blogger>) {
    const { version, ...body } = patch as Partial<Blogger> & { version?: number };
    const { data, message } = await postFull<{ blogger: Blogger }>('/api/blogger/update', { id, ...body });
    const b = data?.blogger ?? null;
    if (b) {
      const idx = bloggers.value.findIndex(x => x.id === id);
      if (idx >= 0) bloggers.value[idx] = b;
    }
    return message;
  }

  /** 老框架 handleContextMenuDelete：POST /api/blogger/delete 只传 { id }；
   *  成功后清选中并重拉列表，返回后端 message 供 toast。 */
  async function deleteBlogger(id: number) {
    const { message } = await postFull('/api/blogger/delete', { id });
    if (selectedBloggerId.value === id) selectedBloggerId.value = null;
    await refreshList();
    return message;
  }

  async function cleanupNow(uid: number | string) {
    await bloggerApi.cleanupNow(uid);
  }

  async function fetchSeries(uid: number): Promise<Series[]> {
    try {
      const r: any = await bloggerApi.series(uid);
      seriesError.value = null;
      return (Array.isArray(r?.series) ? r.series : []) as Series[];
    } catch (error) {
      seriesError.value = error instanceof Error ? error.message : '合集加载失败';
      throw error;
    }
  }

  /** 老框架 startSelectedBlogger：POST /api/task/start 只传 { uid }；
   *  返回 { data, message }（data.schedule.runtime_state 用于 waiting_window 分支文案）。 */
  async function startTask(uid: number) {
    const { data, message } = await postFull<any>('/api/task/start', { uid: String(uid) });
    await refreshList();
    await refreshAllStatus();
    return { data, message };
  }

  /** 老框架 stopSelectedBlogger：POST /api/task/stop 只传 { uid }。 */
  async function stopTask(uid: number) {
    const { data, message } = await postFull<any>('/api/task/stop', { uid: String(uid) });
    await refreshList();
    await refreshAllStatus();
    return { data, message };
  }

  /** 拉取一次所有博主的状态，缓存到 taskStatuses map（key 是数字 uid）。 */
  async function refreshAllStatus() {
    if (statusRequest) return statusRequest;
    statusRequest = (async () => {
      try {
        // /api/task/status 是全局聚合（enabled_tasks / active_tasks / waiting_window_tasks）；
        // 真正按博主的状态在 /api/task/next-check 的 bloggers map 里。
        const r: any = await taskApi.nextCheck();
        if (r && r.bloggers) {
          serverUtcOffset.value = r.server_utc_offset || serverUtcOffset.value || '';
          const map: Record<number, BloggerTaskStatus> = {};
          for (const [k, v] of Object.entries(r.bloggers as Record<string, any>)) {
            const u = Number(k);
            if (!Number.isFinite(u)) continue;
            // 对齐老框架 startStatusPolling 的字段映射（runtime_state 含
            // waiting_window / checking 等四态，pause_reason 供详情使用）。
            map[u] = {
              running: !!(v.monitor_enabled ?? v.is_running),
              runtime_state: v.runtime_state || (v.is_running ? 'scheduled' : 'stopped'),
              pause_reason: v.pause_reason || null,
              within_active_window: v.within_active_window !== false,
              next_check: v.next_action_at ?? v.next_check ?? 0,
            };
          }
          // next-check 是全量快照；新响应必须覆盖旧状态，不能让停止前的状态反盖回来。
          taskStatuses.value = map;
        }
      } catch {
        // 对齐老框架 startStatusPolling：轮询网络错误静默（全局网络降级体系统一提示）。
      }
    })().finally(() => { statusRequest = null; });
    return statusRequest;
  }

  async function refreshStatus(_uid: number) {
    // 兼容旧签名：每次都拉全量，简单可靠
    await refreshAllStatus();
  }

  function selectBlogger(id: number | null) {
    selectedBloggerId.value = id;
  }

  async function saveKnownBlogger(uid: number) {
    await bloggerApi.savedAdd(uid);
    await refreshSaved();
  }

  /** 对齐老框架 updateAutoBoardSummary：总数为 0 时显示"暂无监控博主"。 */
  const boardSummary = computed(() => {
    const total = bloggers.value.length;
    if (!total) return '暂无监控博主';
    const running = bloggers.value.filter(b => taskStatuses.value[b.uid]?.running).length;
    return `${total} 位博主 · ${running} 个监控运行中`;
  });

  return {
    bloggers, selectedBloggerId, savedBloggers, taskStatuses, boardSummary, serverUtcOffset,
    listError, savedError, seriesError,
    selected,
    refreshList, refreshSaved, search, validateUid, addBlogger, updateBlogger, deleteBlogger,
    cleanupNow, fetchSeries, startTask, stopTask, refreshStatus, refreshAllStatus, selectBlogger, saveKnownBlogger,
  };
});
