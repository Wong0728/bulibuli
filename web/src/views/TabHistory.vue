<script setup lang="ts">
import { ref, computed, onBeforeUnmount, onActivated, onDeactivated } from 'vue';
import { useDownloadStore } from '@/stores/download';
import { useHistoryStore } from '@/stores/history';
import { useAppStore } from '@/stores/app';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { openDrawer } from '@/composables/drawer';
import { video as videoApi, refresh as refreshApi } from '@/api';
import { postFull } from '@/api/client';
import type { HistoryEntry, HistoryGroup, DownloadTask } from '@/api/types';

const download = useDownloadStore();
const history = useHistoryStore();
const app = useAppStore();
const toast = useToastStore();

function imageUrl(url?: string) { return url ? videoApi.proxyImage(url) : ''; }
function imageError(event: Event) {
  const image = event.target as HTMLImageElement;
  image.hidden = true;
  image.nextElementSibling?.removeAttribute('hidden');
}

const subTab = ref<'downloading' | 'completed' | 'failed'>('completed');
const refreshing = ref(false);
const lastManualRefreshAt = ref(0);
let pollTimer: ReturnType<typeof setTimeout> | null = null;
let pollingActive = false;

/**
 * 轮询：对齐老框架 startProgressUpdates——页面可见时拉「快照 + aria2 健康」，
 * WS 在线 10s / 断开 1.5s；store 内 fetchShared TTL 去重，与 app 兜底轮询共享请求。
 */
function startPolling() {
  if (pollingActive) return;
  pollingActive = true;
  const tick = async () => {
    // stop 后不再续排，防止 await 窗口内 stopPolling 清不到句柄导致轮询泄漏/双循环
    if (!pollingActive) return;
    if (document.visibilityState === 'visible') {
      // 老框架 loadDownloadStatus：Promise.all([fetchDownloadSnapshot(), fetchDownloadHealth()])
      await Promise.allSettled([download.refreshStatus(), download.refreshHealth()]);
    }
    if (pollingActive) pollTimer = setTimeout(tick, app.wsConnected ? 10_000 : 1_500);
  };
  void tick();
}
function stopPolling() {
  pollingActive = false;
  if (pollTimer) clearTimeout(pollTimer);
  pollTimer = null;
}

/**
 * 进 history tab（老框架 bootstrap.js:344-347）：只 loadHistoryBoard + updateDownloadLists，
 * **不 POST /api/refresh、不弹「看板已刷新」**（refresh 仅手动按钮触发）。
 */
onActivated(() => {
  history.selectTab(subTab.value);
  void history.loadBoard(subTab.value);
  void download.refreshStatus();
  startPolling();
});
onDeactivated(stopPolling);
onBeforeUnmount(stopPolling);

async function switchTab(t: 'downloading' | 'completed' | 'failed') {
  // 老框架 switchBoardTab：切 tab 即重拉看板（点击当前 tab 相当于手动重载该板）。
  subTab.value = t;
  history.selectTab(t);
  await history.loadBoard(t);
}

const downloadingCount = computed(() => history.globalCounts.downloading ?? history.downloadingTotal);

/** 手动刷新看板（老框架 manualRefreshBoard：checkNetwork → 5s 防抖 → refresh → 重拉；成功无 toast）。 */
async function manualRefresh() {
  if (!app.checkNetworkBeforeAction()) return;
  const now = Date.now();
  if (now - lastManualRefreshAt.value < 5000) {
    toast.warn('刷新太频繁，请稍候');
    return;
  }
  lastManualRefreshAt.value = now;
  refreshing.value = true;
  try {
    // 触发后端 L1 worker，再重新拉取看板。
    await refreshApi.trigger('board');
    await history.loadBoard(subTab.value);
  } catch (e: any) {
    toast.error('刷新失败：' + (e && e.message ? e.message : e));
  } finally {
    refreshing.value = false;
  }
}

/** 操作成功后的刷新链（老框架 updateDownloadLists + refreshBoardIfActive）。 */
function refreshAfterAction() {
  void download.refreshStatus();
  void history.loadBoard(subTab.value);
}

/** 全局暂停（老框架 pauseAllDownloads：确认弹窗无 danger，okText「暂停」）。 */
async function pauseAll() {
  if (!await confirmDialog({
    title: '全部暂停',
    message: '确定要暂停所有下载任务吗？',
    confirmText: '暂停',
  })) return;
  try {
    const msg = await download.pauseAll();
    toast.success(msg || '已暂停全部任务');
    refreshAfterAction();
  } catch (e: any) {
    toast.error(e?.message || '全局暂停失败');
  }
}

/** 全局恢复（老框架 resumeAllDownloads：无确认弹窗）。 */
async function resumeAll() {
  try {
    const msg = await download.resumeAll();
    toast.success(msg || '已恢复全部任务');
    refreshAfterAction();
  } catch (e: any) {
    toast.error(e?.message || '全局恢复失败');
  }
}

/** 打开目录。 */
async function openDirectory(entry: HistoryEntry) {
  try { await history.openDirectory(entry.bvid, entry.id); } catch (e: any) { toast.error(e?.message || '失败'); }
}

function showVideo(bvid: string, historyId?: number) {
  openDrawer({ bvid, history_id: historyId, source: 'history' });
}

/** 老框架 utils.js formatFileSize：'0 B' 兜底 + B/KB/MB/GB/TB + toFixed(2)parseFloat。 */
function formatSize(bytes?: number): string {
  const value = Number(bytes);
  if (!Number.isFinite(value) || value <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${parseFloat((value / Math.pow(1024, index)).toFixed(2))} ${units[index]}`;
}

/** 老框架 utils.js formatSpeed。 */
function formatSpeed(bytesPerSecond?: number): string {
  return `${formatSize(bytesPerSecond)}/s`;
}

/** 老框架 utils.js clampPercent。 */
function clampPercent(value: unknown): number {
  const numeric = Number(value);
  return Number.isFinite(numeric) ? Math.min(100, Math.max(0, numeric)) : 0;
}

function formatDuration(seconds?: number) {
  if (!seconds || seconds <= 0) return '';
  const total = Math.floor(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
  return `${m}:${String(s).padStart(2, '0')}`;
}

/** 与老框架 formatTimestamp 一致：YYYY-MM-DD HH:MM。 */
function formatTimestamp(ts?: number) {
  if (!ts || ts <= 0) return '';
  const d = new Date(ts * 1000);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 与老框架 formatViewCount 一致：万/亿缩写。 */
function formatViewCount(view?: number) {
  const v = Number(view) || 0;
  if (v >= 100000000) return (v / 100000000).toFixed(1) + '亿';
  if (v >= 10000) return (v / 10000).toFixed(1) + '万';
  return v.toString();
}

/** 「上次拉取」时间（老框架 updateLastPullTimeDisplay：后端 server_time → MM-DD HH:MM:SS）。 */
const lastPullText = computed(() => {
  const t = history.lastServerTime;
  if (!t) return '--';
  const d = new Date(t * 1000);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `上次拉取：${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
});

/** 以内存任务的历史/P/cid 标识匹配持久记录；同 BV 多 P 不互相遮蔽。 */
function taskMatchesEntry(task: any, entry: HistoryEntry): boolean {
  if (!task || task.bvid !== entry.bvid) return false;
  if (task.history_id != null && entry.id) return Number(task.history_id) === Number(entry.id);
  if (task.cid != null && entry.cid != null) return Number(task.cid) === Number(entry.cid)
    && (task.page == null || entry.page == null || Number(task.page) === Number(entry.page));
  if (task.page != null && entry.page != null) return Number(task.page) === Number(entry.page);
  // 只有两边都没有分 P 标识时，才允许唯一 bvid 兜底。
  return task.history_id == null && task.cid == null && task.page == null
    && entry.cid == null && entry.page == null;
}

/** 按内存任务的 video 类型建 bvid 索引（WS 推送后 O(1) 匹配看板卡片）。 */
const tasksByBvid = computed(() => {
  const map = new Map<string, DownloadTask[]>();
  for (const t of download.tasks.values()) {
    if ((t.type || 'video') !== 'video') continue;
    const list = map.get(t.bvid);
    if (list) list.push(t);
    else map.set(t.bvid, [t]);
  }
  return map;
});

/**
 * 下载中卡片匹配内存实时任务（老框架 patchBoardCardProgress 的 byBvid 聚合）：
 * 同一 entry 取 taskStatusPriority 最高的一条。
 */
function liveTaskFor(e: HistoryEntry): DownloadTask | null {
  const candidates = tasksByBvid.value.get(e.bvid);
  if (!candidates) return null;
  let best: DownloadTask | null = null;
  let bestPriority = -1;
  for (const t of candidates) {
    if (!taskMatchesEntry(t, e)) continue;
    const p = TASK_STATUS_PRIORITY[t.status as string] ?? 0;
    if (p > bestPriority) { best = t; bestPriority = p; }
  }
  return best;
}

/** 老框架 download-queue.js taskStatusPriority。 */
const TASK_STATUS_PRIORITY: Record<string, number> = {
  downloading: 4, pending: 3, retrying: 3, paused: 3, failed: 2, completed: 1,
};

function coverUrl(entry: HistoryEntry) {
  // 与老框架一致：封面统一走 /api/cover（本地缓存优先）。
  const query = entry.id == null ? '' : `?history_id=${encodeURIComponent(String(entry.id))}`;
  return `/api/cover/${encodeURIComponent(entry.bvid)}${query}`;
}

/** 状态点 class（与老框架 stateDotClass 一致）：绿=完成，蓝=下载中，黄=暂停/可下载充电，红=下架/重投/失败，灰=不可下载充电。 */
function stateDotClass(e: HistoryEntry): string {
  if (e.reupload_of) return 'removed';
  const payNote = e.pay_note || '';
  const state = e.state || 'completed';
  switch (state) {
    case 'completed':
    case 'merged':
      return 'completed';
    case 'pending':
    case 'downloading':
      return 'downloading';
    case 'paused':
      return 'paused';
    case 'removed':
      return 'removed';
    case 'pay_blocked':
      return payNote.endsWith('_paid') ? 'pay_blocked' : 'stale';
    case 'failed':
    case 'merge_failed':
    case 'tampered':
      return 'removed';
    default:
      return 'completed';
  }
}

/** 状态文本（与老框架 stateLabel 一致）。 */
function stateLabel(e: HistoryEntry): string {
  if (e.reupload_of) return `疑似重传（${e.reupload_of}）`;
  const payNote = e.pay_note || '';
  const map: Record<string, string> = {
    completed: '已下载', merged: '已合并', pending: '待下载', downloading: '下载中',
    paused: '已暂停', failed: '下载失败', merge_failed: '合并失败', removed: '已下架',
    pay_blocked: payNote.endsWith('_paid') ? '充电专属（可下载）' : '充电专属（不可下载）',
    tampered: 'MD5 不一致',
  };
  const state = e.state || 'completed';
  return map[state] || state;
}

/**
 * 卡片进度视图（老框架 renderBoardVideoCard + _applyProgressToCard 合并语义）：
 * 进度条仅活跃任务显示；文本带 (step/total) 分数式（实时合并来源），
 * 纯板数据初始渲染无 step 前缀。
 */
function cardProgress(e: HistoryEntry) {
  const live = liveTaskFor(e);
  const task: any = live ?? e.task ?? {};
  const status = task.status as string | undefined;
  const isTaskActive = status === 'downloading' || status === 'pending' || status === 'paused';
  const show = isTaskActive
    && (e.state === 'pending' || e.state === 'downloading' || e.state === 'paused' || subTab.value === 'downloading');
  const percent = clampPercent(task.progress_percent);
  const isPaused = status === 'paused';
  const speed = task.speed ? formatSpeed(task.speed) : '';
  const downloadedSize = task.downloaded_size ? formatSize(task.downloaded_size) : '';
  const totalSize = task.total_size ? formatSize(task.total_size) : '';
  let label = '';
  if (live) {
    const step = live.step ?? 1;
    const totalSteps = live.total_steps ?? 2;
    const stepText = `(${step}/${totalSteps})`;
    if (status === 'downloading') label = `${stepText}${live.step_label ? ` ${live.step_label}` : ''} ${percent}%`;
    else if (status === 'pending') label = `${stepText} 等待中`;
    else if (isPaused) label = `${stepText} 已暂停 ${percent}%`;
  } else if (isPaused) {
    label = `已暂停 ${percent}%`;
  } else {
    label = `${percent}%`;
  }
  return {
    show,
    percent,
    label,
    speedText: !isPaused && speed ? speed : '',
    sizeText: downloadedSize && totalSize ? `${downloadedSize} / ${totalSize}` : '',
  };
}

/**
 * 卡片暂停/恢复与优先级控件（老框架 pauseResumeHtml / priorityHtml）。
 * 老框架只用板数据 v.task 渲染（WS patch 只改进度文本不改按钮），
 * 按钮状态在板重拉（switchTab/手动刷新/操作后刷新）时更新——这里同源取 e.task。
 */
function cardControls(e: HistoryEntry) {
  const task: any = e.task ?? {};
  const status = task.status as string | undefined;
  const taskId = Number(task.task_id) || 0;
  const show = !!taskId && (status === 'downloading' || status === 'pending' || status === 'paused');
  return {
    show,
    taskId,
    isPaused: status === 'paused',
    priority: Number(task.priority) || 100,
  };
}

/** 卡片暂停/恢复（老框架 pauseDownload/resumeDownload：toast 优先后端 message）。 */
async function cardPauseResume(entry: HistoryEntry) {
  // 与 cardControls 同源（板数据 v.task），保证图标与点击动作一致。
  const task: any = entry.task ?? {};
  const taskId = Number(task.task_id) || 0;
  if (!taskId) return;
  const isPaused = task.status === 'paused';
  try {
    const msg = isPaused ? await download.resumeDownload(taskId) : await download.pauseDownload(taskId);
    toast.success(msg || (isPaused ? '已恢复' : '已暂停'));
    refreshAfterAction();
  } catch (e: any) {
    toast.error(e?.message || (isPaused ? '恢复失败' : '暂停失败'));
  }
}

/** 卡片优先级调整（老框架 adjustDownloadPriority：步进 10、钳制在 store、next===current 不请求）。 */
async function adjustCardPriority(entry: HistoryEntry, delta: number) {
  const current = Number(entry.task?.priority) || 100;
  try {
    const next = await download.adjustDownloadPriority(entry.bvid, delta, current);
    if (next == null) return;
    toast.success(`下载优先级已调整为 ${next}`);
    refreshAfterAction();
  } catch (e: any) {
    toast.error(e?.message || '调整优先级失败');
  }
}

/** sidecar 四图标（老框架 renderSidecarIcons）：后端四字段是 bool，直接真值判断。 */
const SIDECAR_ITEMS: Array<{ key: 'video' | 'danmaku' | 'comments' | 'subtitle'; label: string; icon: string }> = [
  { key: 'video', label: '视频', icon: 'fa-film' },
  { key: 'danmaku', label: '弹幕', icon: 'fa-comment-dots' },
  { key: 'comments', label: '评论', icon: 'fa-comments' },
  { key: 'subtitle', label: '字幕', icon: 'fa-closed-captioning' },
];

function sidecarOk(e: HistoryEntry, key: 'video' | 'danmaku' | 'comments' | 'subtitle'): boolean {
  // 后端 SidecarStatus 四字段是 bool（src/services/history.rs:28-36），
  // 对齐老框架 renderSidecarIcons 的 `const ok = sidecar[it.key]` 真值判断。
  return !!(e.sidecar as any)?.[key];
}

async function copyPath(path: string) {
  try {
    await navigator.clipboard.writeText(path);
    toast.success('路径已复制');
  } catch {
    toast.error('复制失败');
  }
}

/** 立即整理（老框架 blogger.js:531-545 cleanupBloggerNowByUid，文案逐字对齐）。 */
async function cleanupGroup(g: HistoryGroup) {
  const uid = String(g.uid);
  if (!await confirmDialog({
    title: '立即整理',
    message: `确认立即整理博主 ${uid}？\n\n将按保留数删除多余的旧视频（文件 + 记录）。`,
    confirmText: '开始整理',
  })) return;
  try {
    // 老框架 apiPost：body 只有 uid；成功 toast '整理完成'（后端 message 同文案）。
    const { message } = await postFull('/api/blogger/cleanup-now', { uid });
    toast.success(message && message !== 'success' ? message : '整理完成');
    await history.loadBoard(subTab.value);
  } catch (e: any) {
    toast.error(e?.message || '整理失败');
  }
}

/** aria2 指示点点击诊断（老框架 updateAria2StatusDot 的 dot click toast）。 */
function onAria2DotClick() {
  const d = download.diagnoseAria2();
  if (!d) return;
  if (d.type === 'success') toast.success(d.message, d.duration);
  else if (d.type === 'info') toast.info(d.message, d.duration);
  else toast.error(d.message, d.duration);
}

/** 与老框架一致：分组按 uid 稳定排序。 */
const sortedGroups = computed(() => {
  const groups = subTab.value === 'completed' ? history.completedGroups
    : subTab.value === 'failed' ? history.failedGroups
      : history.downloadingGroups;
  return [...groups].sort((a, b) => String(a.uid || '').localeCompare(String(b.uid || '')));
});
const loadedCount = computed(() => sortedGroups.value.reduce((count, g) => count + g.videos.length, 0));
/** 「加载更多」用后端按 tab 的 data.total（老框架 historyPagination.total）。 */
const currentTotal = computed(() => history.boardTotals[subTab.value] ?? 0);

/** 空态文案（老框架 renderHistoryBoard 的 hints，三个子 tab 各自一套）。 */
const emptyHint = computed(() => ({
  downloading: '当前没有下载中的视频',
  completed: '还没有已下载的视频，去博主搜索或手动查询添加吧',
  failed: '没有下载失败的视频',
}[subTab.value]));

/** 老框架 utils.js ERROR_KIND_LABELS。 */
const ERROR_KIND_LABELS: Record<string, string> = {
  Paywall: '充电/付费',
  PermissionDenied: '权限不足',
  AccountFrozen: '账号被封',
  RegionRestricted: '区域限制',
  LoginRequired: '需要登录',
  NetworkError: '网络错误',
  CookieInvalid: 'Cookie 失效',
  AlreadyExists: '重复任务',
  RateLimited: '触发风控',
  BvidNotFound: '视频不存在',
  NotFound: '视频不存在',
  Internal: '服务器内部错误',
};

/** 失败原因（老框架 utils.js formatFailureText：[kindLabel, fallback, message] join ' · '）。 */
function describeFailure(failure?: { message?: string; kind?: string; fallback_reason?: string } | null): string {
  if (!failure) return '';
  const kindLabel = failure.kind
    ? (ERROR_KIND_LABELS[failure.kind] || `${failure.kind}（未知错误）`)
    : '';
  const parts = [kindLabel, failure.fallback_reason, failure.message].filter(Boolean);
  return parts.join(' · ');
}
</script>

<template>
  <section class="tab-panel">
    <div class="card">
      <div class="board-top-bar">
        <div class="board-top-bar-left">
          <i class="fa-solid fa-download"></i>
          <span>下载管理</span>
          <span id="last-pull-time" class="last-pull-time" title="上次从后端拉取看板数据的时间">{{ lastPullText }}</span>
        </div>
        <div class="board-top-bar-right">
          <span id="download-queue-summary" class="queue-summary" title="队列摘要：各状态任务数">{{ download.queueSummary }}</span>
          <span id="aria2-status-dot-history" class="aria2-dot" :class="download.aria2DotClass" :title="download.aria2Title" @click="onAria2DotClick"></span>
          <button class="btn btn-sm btn-ghost" data-mutating title="暂停全部下载任务" @click="pauseAll">
            <i class="fa-solid fa-pause"></i> 全部暂停
          </button>
          <button class="btn btn-sm btn-ghost" data-mutating title="恢复全部暂停任务" @click="resumeAll">
            <i class="fa-solid fa-play"></i> 全部恢复
          </button>
          <button class="btn btn-sm btn-ghost" id="board-refresh-btn" title="手动刷新看板数据（5 秒内防抖）" :disabled="refreshing" @click="manualRefresh">
            <i class="fa-solid fa-sync-alt" :class="{ 'fa-spin': refreshing }"></i> 刷新
          </button>
        </div>
      </div>
      <div v-if="history.error" class="live-alert error" role="alert">历史记录加载失败：{{ history.error }}</div>

      <div class="board-sub-tabs">
        <div :class="['board-sub-tab', { active: subTab === 'downloading' }]" data-board-tab="downloading" @click="switchTab('downloading')">
          下载中 <span class="board-tab-count" id="board-count-downloading">{{ downloadingCount }}</span>
        </div>
        <div :class="['board-sub-tab', { active: subTab === 'completed' }]" data-board-tab="completed" @click="switchTab('completed')">
          已下载 <span class="board-tab-count" id="board-count-completed">{{ history.completedTotal }}</span>
        </div>
        <div :class="['board-sub-tab', { active: subTab === 'failed' }]" data-board-tab="failed" @click="switchTab('failed')">
          下载失败 <span class="board-tab-count" id="board-count-failed">{{ history.failedTotal }}</span>
        </div>
      </div>

      <div class="history-board" id="history-board">
        <!-- 三个子 tab 统一按博主分组的卡片列表（老框架 renderHistoryBoard）。 -->
        <div v-if="sortedGroups.length === 0" class="empty-state-grid">
          <i class="fa-solid fa-inbox"></i>
          <p>{{ emptyHint }}</p>
          <p class="empty-hint"><a href="#" @click.prevent="app.setTab('search')">去博主搜索</a> 或 <a href="#" @click.prevent="app.setTab('manual')">手动查询</a></p>
        </div>
        <div v-else class="board-groups">
          <div v-for="g in sortedGroups" :key="`group-${g.uid}`" class="blogger-section" :data-uid="g.uid">
            <div class="blogger-section-header">
              <div class="blogger-section-info">
                <template v-if="g.face">
                  <img :src="imageUrl(g.face)" class="blogger-section-avatar" alt="" @error="imageError" />
                  <div class="blogger-section-avatar blogger-section-avatar-fallback" hidden>{{ String(g.name || g.uid || '?').slice(0, 1) }}</div>
                </template>
                <div v-else class="blogger-section-avatar blogger-section-avatar-fallback">{{ String(g.name || g.uid || '?').slice(0, 1) }}</div>
                <div class="blogger-section-text">
                  <div class="blogger-section-name">{{ g.name || g.uid || '未知博主' }}</div>
                  <div class="blogger-section-uid">UID: {{ g.uid }}</div>
                </div>
              </div>
              <button class="btn btn-sm btn-ghost" data-mutating title="立即按保留数清理该博主" @click="cleanupGroup(g)">
                <i class="fa-solid fa-broom"></i> 立即整理
              </button>
            </div>
            <div class="blogger-section-videos">
              <article v-for="e in g.videos" :key="e.id"
                       :class="['board-video-card', `state-${stateDotClass(e)}`]"
                       @click="showVideo(e.bvid, e.id)">
                <div class="board-card-state-dot" :title="stateLabel(e)"></div>
                <div class="board-card-thumb">
                  <img :src="coverUrl(e)" alt="" loading="lazy" @error="imageError" />
                  <div class="video-thumb-fallback" hidden><i class="fa-solid fa-video"></i></div>
                  <span v-if="e.duration" class="board-card-duration">{{ formatDuration(e.duration) }}</span>
                  <span v-if="e.reupload_of" class="reupload-badge" :title="`可能是 ${e.reupload_of} 的重传`">重投?</span>
                </div>
                <div class="board-card-body">
                  <div class="board-card-title" :title="e.title">{{ e.title }}</div>
                  <div class="board-card-meta">
                    <span title="发布时间"><i class="fa-solid fa-calendar-alt"></i> {{ e.pub_date || formatTimestamp(e.pub_timestamp) || '--' }}</span>
                    <span title="播放量"><i class="fa-solid fa-play"></i> {{ formatViewCount(e.view) }}</span>
                  </div>
                  <template v-if="cardProgress(e).show">
                    <div class="board-card-progress">
                      <progress class="board-card-progress-bar" max="100" :value="cardProgress(e).percent"></progress>
                    </div>
                    <div class="board-card-progress-text">
                      <span>{{ cardProgress(e).label }}</span>
                      <span v-if="cardProgress(e).speedText" class="board-card-speed">{{ cardProgress(e).speedText }}</span>
                      <span v-if="cardProgress(e).sizeText" class="board-card-size">{{ cardProgress(e).sizeText }}</span>
                    </div>
                  </template>
                  <div v-if="(e.state === 'failed' || e.state === 'merge_failed') && describeFailure(e.failure)" class="board-card-failure" :title="describeFailure(e.failure)">
                    <i class="fa-solid fa-circle-exclamation"></i><span>{{ describeFailure(e.failure) }}</span>
                  </div>
                  <div class="board-card-sidecar">
                    <span v-for="item in SIDECAR_ITEMS" :key="item.key"
                          :class="['sidecar-icon', sidecarOk(e, item.key) ? 'ok' : 'missing']"
                          :title="`${item.label}: ${sidecarOk(e, item.key) ? '已下载' : '未下载'}`">
                      <i class="fa-solid" :class="item.icon"></i>{{ sidecarOk(e, item.key) ? '✓' : '—' }}
                    </span>
                  </div>
                  <div v-if="e.local_path || e.relative_path" class="board-card-path" :title="e.local_path || '路径已隐藏'">
                    <i class="fa-solid fa-file-video"></i>
                    <span>{{ e.local_path || '路径已隐藏' }}</span>
                    <button v-if="e.local_path" class="btn btn-sm btn-ghost" title="复制路径" @click.stop="copyPath(e.local_path!)"><i class="fa-solid fa-copy"></i></button>
                    <button v-if="e.can_open_directory && e.relative_path" class="btn btn-sm btn-ghost" title="打开文件所在目录" @click.stop="openDirectory(e)"><i class="fa-solid fa-folder-open"></i></button>
                  </div>
                  <button v-if="cardControls(e).show" class="board-card-action-btn" data-mutating
                          :title="cardControls(e).isPaused ? '恢复下载' : '暂停下载'"
                          @click.stop="cardPauseResume(e)">
                    <i class="fa-solid" :class="cardControls(e).isPaused ? 'fa-play' : 'fa-pause'"></i>
                  </button>
                  <span v-if="cardControls(e).show" class="board-card-priority" title="下载优先级（1-300，越大越先下载）">
                    <button class="board-card-action-btn" data-mutating title="降低优先级" @click.stop="adjustCardPriority(e, -10)">−</button>
                    <span class="board-card-priority-value">{{ cardControls(e).priority }}</span>
                    <button class="board-card-action-btn" data-mutating title="提高优先级" @click.stop="adjustCardPriority(e, 10)">+</button>
                  </span>
                </div>
              </article>
            </div>
          </div>
        </div>
        <div v-if="loadedCount < currentTotal" style="text-align: center; margin-top: 12px;">
          <button class="btn btn-secondary history-load-more" @click="history.loadMore">加载更多（{{ loadedCount }}/{{ currentTotal }}）</button>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.board-groups {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
</style>
