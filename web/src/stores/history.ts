/**
 * 历史下载 store：按博主分组的看板数据。
 *
 * 后端 GET /api/history/list?tab=... 返回的是按博主分组的结构
 * `{ items: [{ uid, name, face, counts, videos: [...] }], counts, total, ... }`。
 * 前端 HistoryEntry 是平铺的（一个 entry 一个 video），所以这里拍平一下。
 *
 * 与 download store 的关系：
 * - download：内存中正在运行的任务（短暂生命周期）
 * - history：已完成/失败/已下载到本地的持久记录（跨进程）
 * 看板"下载中"子 Tab 取 download；"已下载/失败"取 history。
 *
 * 所有 action 内部自带 try/catch，**不向调用者抛 promise reject**。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { history as historyApi } from '@/api';
import type { HistoryEntry, HistoryGroup, HistoryBoardResponse } from '@/api/types';

export type HistoryTab = 'downloading' | 'completed' | 'failed';

/** 把后端 history video 节点拍平成 HistoryEntry。 */
function flattenVideo(v: any, fallbackUid?: string | number): HistoryEntry {
  const task = v.task ?? {};
  const sidecar = v.sidecar ?? {};
  return {
    id: Number(v.history_id ?? v.id ?? 0),
    bvid: String(v.bvid ?? ''),
    title: String(v.title ?? v.bvid ?? ''),
    uid: v.uid ?? fallbackUid,
    uploader_name: v.uploader_name,
    status: String(task.status ?? v.state ?? 'unknown'),
    is_completed: v.is_completed,
    has_danmaku: sidecar.danmaku?.exists || sidecar.has_danmaku,
    has_comments: sidecar.comments?.exists || sidecar.has_comments,
    has_video: sidecar.video?.exists || sidecar.has_video,
    has_audio: sidecar.audio?.exists || sidecar.has_audio,
    has_cover: !!v.cover_local_path || sidecar.cover?.exists || sidecar.has_cover,
    local_path: v.file_path,
    relative_path: v.relative_path,
    downloaded_at: v.download_time ? Math.floor(new Date(v.download_time).getTime() / 1000) : undefined,
    duration: v.duration,
    page: v.page,
    cid: v.cid,
    part_title: v.part_title,
    pic: v.pic,
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
  const completedTotal = ref(0);
  const failedTotal = ref(0);
  const downloadingTotal = ref(0);
  const page = ref(1);
  const pageSize = ref(50);
  const loading = ref(false);
  const lastLoadedAt = ref<number>(0);

  async function loadBoard(tab: HistoryTab, append = false) {
    activeTab.value = tab;
    if (!append) page.value = 1;
    loading.value = true;
    try {
      const data = (await historyApi.list(tab, page.value, pageSize.value)) as unknown as HistoryBoardResponse | null;
      const items = Array.isArray(data?.items) ? (data!.items as any[]).map(normalizeGroup) : [];
      const flat: HistoryEntry[] = [];
      for (const group of items) {
        for (const v of group.videos) flat.push(v);
      }
      // counts 是全局的（跨所有博主），不受 page 影响
      const counts = data?.counts ?? {};
      if (tab === 'completed') {
        completedGroups.value = append ? [...completedGroups.value, ...items] : items;
        completed.value = append ? [...completed.value, ...flat] : flat;
        completedTotal.value = Number(counts.completed ?? 0);
      } else if (tab === 'failed') {
        failedGroups.value = append ? [...failedGroups.value, ...items] : items;
        failed.value = append ? [...failed.value, ...flat] : flat;
        failedTotal.value = Number(counts.failed ?? 0);
      } else {
        downloadingGroups.value = append ? [...downloadingGroups.value, ...items] : items;
        downloading.value = append ? [...downloading.value, ...flat] : flat;
        downloadingTotal.value = Number(counts.downloading ?? 0);
      }
      lastLoadedAt.value = Date.now();
    } catch { /* 静默 */ }
    finally { loading.value = false; }
  }

  async function loadMore() {
    page.value += 1;
    await loadBoard(activeTab.value, true);
  }

  async function search(keyword: string) {
    try {
      const r: any = await historyApi.search(keyword, activeTab.value === 'failed' ? 'failed' : 'completed');
      return (r?.items ?? []) as HistoryEntry[];
    } catch { return []; }
  }

  async function deleteEntry(id: number) {
    try {
      await historyApi.delete(id);
      completed.value = completed.value.filter(e => e.id !== id);
      failed.value = failed.value.filter(e => e.id !== id);
      downloading.value = downloading.value.filter(e => e.id !== id);
      // 同步从分组中移除（跨所有 group）
      const filterGroups = (arr: HistoryGroup[]) => arr
        .map(g => ({ ...g, videos: g.videos.filter(v => v.id !== id) }))
        .filter(g => g.videos.length > 0);
      completedGroups.value = filterGroups(completedGroups.value);
      failedGroups.value = filterGroups(failedGroups.value);
      downloadingGroups.value = filterGroups(downloadingGroups.value);
    } catch { /* 静默 */ }
  }

  async function openDirectory(id: number) {
    try { await historyApi.openDirectory(id); } catch { /* 静默 */ }
  }

  function reset() {
    completed.value = []; failed.value = []; downloading.value = [];
    completedGroups.value = []; failedGroups.value = []; downloadingGroups.value = [];
    completedTotal.value = 0; failedTotal.value = 0; downloadingTotal.value = 0;
    page.value = 1;
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
    completedTotal, failedTotal, downloadingTotal, loading, pageSize,
    lastLoadedAt,
    loadBoard, loadMore, search, deleteEntry, openDirectory, reset,
  };
});
