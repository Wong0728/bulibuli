/**
 * 历史下载 store：按博主分组的看板数据。
 *
 * 后端 GET /api/history/list?tab=... 返回的是按博主分组的结构
 * `{ items: [{ uid, name, face, counts, videos: [...] }], counts, total, server_time, ... }`。
 * 前端 HistoryEntry 是平铺的（一个 entry 一个 video），所以这里拍平一下。
 *
 * 与 download store 的关系：
 * - download：内存中正在运行的任务（短暂生命周期）
 * - history：已完成/失败/已下载到本地的持久记录（跨进程）
 * 看板"下载中"子 Tab 取 download；"已下载/失败"取 history。
 *
 * 对齐老框架（static/js/history.js）：
 * - sidecar 四字段（video/danmaku/comments/subtitle）后端是 bool，直接真值判断。
 * - 「上次拉取」时间用后端 server_time（秒），格式 MM-DD HH:MM:SS。
 * - 加载更多的 total 用后端按 tab 的 data.total；徽章计数用全局 counts。
 * - 不产出 version/乐观锁（老框架一律不带）。
 *
 * 所有 action 内部自带 try/catch，**不向调用者抛 promise reject**。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { history as historyApi } from '@/api';
import type { HistoryEntry, HistoryGroup, HistoryBoardResponse, HistoryCounts } from '@/api/types';

export type HistoryTab = 'downloading' | 'completed' | 'failed';

/**
 * 把后端 history video 节点拍平成 HistoryEntry。
 * 后端 sidecar 是 SidecarStatus（src/services/history.rs:28-36）：
 * video/danmaku/comments/subtitle 四个字段全是 bool，直接真值判断（对齐 renderSidecarIcons）。
 */
function flattenVideo(v: any, fallbackUid?: string | number): HistoryEntry {
  const task = v.task ?? {};
  const sidecar = v.sidecar ?? {};
  const downloadTime = v.download_time == null ? undefined : String(v.download_time);
  const downloadedAt = downloadTime ? Math.floor(new Date(downloadTime).getTime() / 1000) : undefined;
  return {
    id: Number(v.history_id ?? v.id ?? 0),
    bvid: String(v.bvid ?? ''),
    title: String(v.title ?? v.bvid ?? ''),
    uid: v.uid ?? fallbackUid,
    uploader_name: v.uploader_name,
    status: String(v.state ?? task.status ?? 'unknown'),
    state: v.state == null ? undefined : String(v.state),
    is_completed: v.is_completed,
    has_danmaku: sidecar.danmaku === true || sidecar.has_danmaku === true,
    has_comments: sidecar.comments === true || sidecar.has_comments === true,
    has_video: sidecar.video === true || sidecar.has_video === true,
    has_audio: sidecar.audio === true || sidecar.has_audio === true,
    has_cover: !!v.cover_local_path || sidecar.cover === true || sidecar.has_cover === true,
    local_path: v.file_path,
    relative_path: v.relative_path,
    downloaded_at: Number.isFinite(downloadedAt) ? downloadedAt : undefined,
    download_time: downloadTime,
    pub_date: v.pub_date,
    pub_timestamp: v.pub_timestamp,
    view: v.view,
    duration: v.duration,
    page: v.page,
    cid: v.cid,
    part_title: v.part_title,
    pic: v.pic,
    reupload_of: v.reupload_of,
    pay_note: v.pay_note,
    md5: v.md5,
    md5_last_checked_at: v.md5_last_checked_at,
    sha256: v.sha256,
    sha256_last_checked_at: v.sha256_last_checked_at,
    can_open_directory: v.can_open_directory === true,
    sidecar,
    failure: v.failure ?? null,
    task: task,
    burned: v.burned,
  };
}

/** 把后端的 group 节点规整成 HistoryGroup，补齐缺省。 */
function normalizeGroup(g: any): HistoryGroup {
  const videos = Array.isArray(g?.videos) ? g.videos.map((v: any) => flattenVideo(v, g?.uid)) : [];
  return {
    uid: g?.uid ?? 'unknown',
    name: g?.name,
    face: g?.face,
    last_seen_name: g?.last_seen_name,
    last_seen_face: g?.last_seen_face,
    last_seen_at: g?.last_seen_at,
    notice_visible: !!g?.notice_visible,
    counts: g?.counts ?? {},
    videos,
  };
}

export const useHistoryStore = defineStore('history', () => {
  const activeTab = ref<HistoryTab>('completed');
  const completed = ref<HistoryEntry[]>([]);
  const failed = ref<HistoryEntry[]>([]);
  const downloading = ref<HistoryEntry[]>([]);
  /** 按博主分组的原始结构，组件按博主分组渲染时直接读它。 */
  const completedGroups = ref<HistoryGroup[]>([]);
  const failedGroups = ref<HistoryGroup[]>([]);
  const downloadingGroups = ref<HistoryGroup[]>([]);
  /** 子 tab 徽章计数（对齐老框架 updateElement：已下载 = completed + removed + pay_blocked）。 */
  const completedTotal = ref(0);
  const failedTotal = ref(0);
  const downloadingTotal = ref(0);
  /** 「加载更多」用的 per-tab 总数（对齐老框架 historyPagination.total = data.total）。 */
  const boardTotals = ref<Record<HistoryTab, number>>({ downloading: 0, completed: 0, failed: 0 });
  /** 后端返回的跨分页全局计数；任何 tab 拉取后都更新，供顶栏稳定显示。 */
  const globalCounts = ref<HistoryCounts>({ downloading: 0, completed: 0, failed: 0, removed: 0, pay_blocked: 0 });
  /** 上次拉取的 server_time（秒），对齐老框架 _state.lastBoardServerTime。 */
  const lastServerTime = ref(0);
  const pages: Record<HistoryTab, number> = { downloading: 1, completed: 1, failed: 1 };
  const pageSize = ref(50);
  /** per-tab 加载标记：与 requestId 守卫同粒度，避免跨子 tab 的 append 防抖误伤。 */
  const loading = ref<Record<HistoryTab, boolean>>({ downloading: false, completed: false, failed: false });
  const error = ref<string | null>(null);
  const boardRequestIds: Record<HistoryTab, number> = { downloading: 0, completed: 0, failed: 0 };

  async function loadBoard(tab: HistoryTab, append = false) {
    // 老框架 loadHistoryBoard：append 进行中时跳过，避免快速点击「加载更多」并发翻页。
    if (append && loading.value[tab]) return;
    const requestId = ++boardRequestIds[tab];
    if (!append) pages[tab] = 1;
    loading.value[tab] = true;
    try {
      const data = (await historyApi.list(tab, pages[tab], pageSize.value)) as unknown as HistoryBoardResponse | null;
      const items = Array.isArray(data?.items) ? (data!.items as any[]).map(normalizeGroup) : [];
      const mergedGroups = append ? [...(tab === 'completed' ? completedGroups.value : tab === 'failed' ? failedGroups.value : downloadingGroups.value)] : [];
      for (const group of items) {
        const existing = mergedGroups.find(item => String(item.uid) === String(group.uid));
        if (existing) {
          const known = new Set(existing.videos.map(video => video.id));
          existing.videos = [...existing.videos, ...group.videos.filter(video => !known.has(video.id))];
          existing.counts = group.counts ?? existing.counts;
        } else {
          mergedGroups.push(group);
        }
      }
      const flat = mergedGroups.flatMap(group => group.videos);
      // 较慢的旧请求不能覆盖用户刚切换到的子 Tab。
      if (requestId !== boardRequestIds[tab]) return;
      error.value = null;
      // counts 是全局的（跨所有博主），不受 page 影响
      const counts = data?.counts ?? {};
      globalCounts.value = {
        downloading: Number(counts.downloading ?? globalCounts.value.downloading ?? 0),
        completed: Number(counts.completed ?? globalCounts.value.completed ?? 0),
        failed: Number(counts.failed ?? globalCounts.value.failed ?? 0),
        removed: Number(counts.removed ?? globalCounts.value.removed ?? 0),
        pay_blocked: Number(counts.pay_blocked ?? globalCounts.value.pay_blocked ?? 0),
      };
      // 「上次拉取」时间用后端 server_time（对齐老框架 _state.lastBoardServerTime）。
      lastServerTime.value = Number(data?.server_time) || 0;
      // 「加载更多」计数用后端按 tab 的 DB total（对齐老框架 historyPagination.total）。
      boardTotals.value[tab] = Number(data?.total) || 0;
      if (tab === 'completed') {
        completedGroups.value = mergedGroups;
        completed.value = flat;
        // 旧版“已下载”计数还包含已下架/付费拦截的历史记录。
        completedTotal.value = Number(counts.completed ?? 0)
          + Number(counts.removed ?? 0)
          + Number(counts.pay_blocked ?? 0);
      } else if (tab === 'failed') {
        failedGroups.value = mergedGroups;
        failed.value = flat;
        failedTotal.value = Number(counts.failed ?? 0);
      } else {
        downloadingGroups.value = mergedGroups;
        downloading.value = flat;
        downloadingTotal.value = Number(counts.downloading ?? 0);
      }
    } catch (cause) {
      // 对齐老框架：pagination 只在成功后写入；append 失败回滚页号，避免下次跳页。
      if (append) pages[tab] = Math.max(1, pages[tab] - 1);
      if (requestId === boardRequestIds[tab]) {
        error.value = cause instanceof Error ? cause.message : '历史记录加载失败';
      }
    }
    finally {
      if (requestId === boardRequestIds[tab]) loading.value[tab] = false;
    }
  }

  async function loadMore() {
    // 与 loadBoard 的 append 防抖一致：进行中时不递增页号，避免跳页。
    if (loading.value[activeTab.value]) return;
    pages[activeTab.value] += 1;
    await loadBoard(activeTab.value, true);
  }

  function selectTab(tab: HistoryTab) { activeTab.value = tab; }

  async function search(keyword: string) {
    try {
      const r: any = await historyApi.search(keyword);
      return (r?.history ?? []) as HistoryEntry[];
    } catch { return []; }
  }

  async function openDirectory(bvid: string, historyId?: number) {
    await historyApi.openDirectory(bvid, historyId);
  }

  function reset() {
    completed.value = []; failed.value = []; downloading.value = [];
    completedGroups.value = []; failedGroups.value = []; downloadingGroups.value = [];
    completedTotal.value = 0; failedTotal.value = 0; downloadingTotal.value = 0;
    boardTotals.value = { downloading: 0, completed: 0, failed: 0 };
    globalCounts.value = { downloading: 0, completed: 0, failed: 0, removed: 0, pay_blocked: 0 };
    lastServerTime.value = 0;
    error.value = null;
    pages.downloading = 1; pages.completed = 1; pages.failed = 1;
  }

  /** 当前 active tab 的分组数据，模板里按博主渲染时直接用。 */
  const activeGroups = computed<HistoryGroup[]>(() => {
    if (activeTab.value === 'completed') return completedGroups.value;
    if (activeTab.value === 'failed') return failedGroups.value;
    return downloadingGroups.value;
  });

  return {
    activeTab, completed, failed, downloading,
    completedGroups, failedGroups, downloadingGroups, activeGroups,
    completedTotal, failedTotal, downloadingTotal, boardTotals, globalCounts, loading, error, pageSize,
    lastServerTime,
    loadBoard, loadMore, selectTab, search, openDirectory, reset,
  };
});
