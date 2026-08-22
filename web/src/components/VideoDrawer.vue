<script setup lang="ts">
/**
 * 视频详情侧拉抽屉（单列布局，对齐老框架 renderDrawerContent / renderDrawerContentForManualQuery）。
 *
 * 数据与交互遵循迁移前已验证的抽屉行为：
 * - 已下载抽屉打开只调 GET /api/history/list?bvid=（纯本地库）+ 日志，从不取流；
 *   /api/video/info 仅在"实时数据 → 刷新"时按需加载，/api/video/get-video-urls 仅手动抽屉打开时拉取
 *   （对齐老框架：已下载抽屉无清晰度 pills / 分P区，重试只传 {bvid, qn}）。
 * - 封面统一 /api/cover/{bvid}?history_id=（本地优先 + 兑底下载）。
 * - 下载/重试前走 /api/video/gate-download 预检（下架/付费无权限拦截）。
 * - 弹幕/评论下载是同步接口，携带 source（manual/auto），按返回 count 提示。
 * - 删除记录固定连文件一起删（强确认），toast 显示删除文件数。
 * - 烧录用 detail.can_burn 判断能力，轮询 burn/status 直至 completed/failed。
 * - DOM 常驻 + .active class 驱动 transition（与老框架动效一致）。
 */
import { ref, computed, watch, onUnmounted, toRef } from 'vue';
import { drawerState, openDrawer, closeDrawer } from '@/composables/drawer';
import { useDownloadStore } from '@/stores/download';
import { useHistoryStore } from '@/stores/history';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { video as videoApi, logs as logsApi, history as historyApi, download as downloadApi, cover as coverApi } from '@/api';
import type { VideoInfo, VideoUrlsResult } from '@/api/types';
import { useModalFocus } from '@/composables/modalFocus';
// 需要消费后端 message（“开始下载”等）时用 Full 变体，在白名单文件内本地封装。
import { postFull } from '../api/client';

const download = useDownloadStore();
const historyStore = useHistoryStore();
const toast = useToastStore();
const drawerRoot = ref<HTMLElement | null>(null);
useModalFocus(toRef(drawerState, 'visible'), drawerRoot, closeDrawer);

/** 与老框架 drawer.js::_ALL_QUALITY_OPTIONS 一致；不含 126/127（后端下载白名单最高 125）。 */
const QUALITY_OPTIONS = [
  { qn: 125, label: 'HDR', tag: 'HDR' },
  { qn: 120, label: '4K 超清', tag: '4K' },
  { qn: 116, label: '1080P60', tag: '60帧' },
  { qn: 112, label: '1080P+ 高码率', tag: '高码' },
  { qn: 80, label: '1080P 高清', tag: '1080P' },
  { qn: 74, label: '720P60', tag: '60帧' },
  { qn: 64, label: '720P 高清', tag: '720P' },
  { qn: 32, label: '480P 清晰', tag: '480P' },
  { qn: 16, label: '360P 流畅', tag: '360P' },
];
const TYPE_LABELS: Record<string, string> = {
  video: '视频', danmaku_video: '弹幕版视频', audio: '音频', cover: '封面',
  danmaku: '弹幕', subtitle: 'CC 字幕', comment: '评论', other: '文件',
};
const TYPE_ICONS: Record<string, string> = {
  video: 'fa-film', danmaku_video: 'fa-fire', audio: 'fa-music', cover: 'fa-image',
  danmaku: 'fa-comment-dots', subtitle: 'fa-closed-captioning', comment: 'fa-comments', other: 'fa-file',
};

// 单视频详情（产物文件 + sidecar + task 状态），来自 /api/history/list?bvid=
const detail = ref<any>(null);
const detailLoading = ref(false);
// B 站实时信息：按需加载，绝不随打开抽屉自动请求（风控）。
const videoInfo = ref<VideoInfo | null>(null);
const liveStatsLoading = ref(false);
const liveStatsLoaded = ref(false);
// 取流结果：只用于点亮清晰度 pill，选画质是纯本地状态（与老框架一致）。
const urls = ref<VideoUrlsResult | null>(null);
const selectedQn = ref(80);
const selectedPages = ref<Set<number>>(new Set());
// 取流被充电/付费权限拦截（HTTP 402）时的抽屉内提示：
// 打开抽屉不弹 toast，点“开始下载”时 gate 预检会给一次明确的提示，
// 避免“开抽屉一个报错、点下载又一个报错”的双重打扰。
const streamBlockedMessage = ref('');
// 日志（打开抽屉自动拉一次，可手动刷新）
type LogLine = { time: string; level: string; msg: string; cls: 'log-error' | 'log-info' };
const logLines = ref<LogLine[]>([]);
const logsLoading = ref(false);
// 侧车浏览器
type SidecarViewer = { kind: 'comments' | 'danmaku'; path: string; exists: boolean; format?: string; comments?: any[]; danmaku?: any[]; content?: string };
const sidecarViewer = ref<SidecarViewer | null>(null);
const sidecarLoading = ref(false);
const sidecarError = ref('');
// 服务器到本机：主勾选框开启选择模式（默认全选）
const selectMode = ref(false);
const selectedFilePaths = ref<Set<string>>(new Set());
// 烧录
const burningTask = ref<string | null>(null);
let burnPollTimer: number | null = null;
let burnPollGeneration = 0;
let drawerGeneration = 0;

/* ---------------- 派生数据：优先 detail.video，拉不到时退回看板卡片（对齐老框架 cachedVideo 兜底链） ---------------- */
const video = computed<any>(() => detail.value || drawerState.video || null);
// detail 未返回时用打开抽屉时传入的 history_id 兜底（看板卡片精确到多 P 记录）。
const historyId = computed<number | null>(() => {
  const value = video.value?.history_id ?? drawerState.video?.history_id ?? (drawerState.video as any)?.id;
  return value != null ? Number(value) : null;
});
const drawerTitle = computed(() => video.value?.title || drawerState.video?.title || drawerState.video?.bvid || '视频详情');
const coverUrl = computed(() => drawerState.video ? coverApi.url(drawerState.video.bvid, historyId.value ?? undefined) : '');
// 状态点/文案：兼容 history/list 与 download/status 两种来源（对齐 history.js）
const state = computed<string>(() => video.value?.state || video.value?.status || 'completed');
const stateDot = computed(() => {
  const v = video.value; const s = state.value;
  if (v?.reupload_of) return 'removed';
  const payNote = v?.pay_note || '';
  switch (s) {
    case 'completed': case 'merged': return 'completed';
    case 'pending': case 'downloading': return 'downloading';
    case 'paused': return 'paused';
    case 'removed': return 'removed';
    case 'pay_blocked': return payNote.endsWith('_paid') ? 'pay_blocked' : 'stale';
    case 'failed': case 'merge_failed': return 'removed';
    default: return 'stale';
  }
});
const stateLabel = computed(() => {
  const v = video.value; const s = state.value;
  if (v?.reupload_of) return `疑似重传（${v.reupload_of}）`;
  const payNote = v?.pay_note || '';
  const map: Record<string, string> = {
    completed: '已下载', merged: '已合并', pending: '待下载', downloading: '下载中',
    paused: '已暂停', failed: '下载失败', merge_failed: '合并失败', removed: '已下架',
    pay_blocked: payNote.endsWith('_paid') ? '充电专属（可下载）' : '充电专属（不可下载）',
    tampered: 'MD5 不一致',
  };
  return map[s] || s;
});
const task = computed<any>(() => video.value?.task || null);
const taskStatus = computed<string>(() => String(task.value?.status || ''));
const taskId = computed<number | null>(() => Number(task.value?.task_id) || null);
const taskActive = computed(() => ['downloading', 'pending', 'paused'].includes(taskStatus.value));
const isPaused = computed(() => taskStatus.value === 'paused');
const progressPercent = computed(() => clampPercent(task.value?.progress_percent));
// 主按钮：重新下载仅在 state ∈ failed/removed/pay_blocked 时出现（老框架语义）
const canRetry = computed(() => ['failed', 'removed', 'pay_blocked'].includes(state.value));
const canBurn = computed(() => video.value?.can_burn !== false);
const artifactSource = computed(() =>
  (video.value?.source === 'manual' || drawerState.video?.source === 'manual') ? 'manual' : 'auto');
const canOpenDirectory = computed(() => video.value?.can_open_directory === true);
const canBrowserDownload = computed(() => video.value?.can_browser_download === true);
const filePath = computed<string>(() => video.value?.file_path || '');
const relativePath = computed<string>(() => video.value?.relative_path || '');
const burned = computed<any>(() => video.value?.burned || {});
const sidecar = computed<any>(() => video.value?.sidecar || null);
const files = computed<any[]>(() => Array.isArray(video.value?.files) ? video.value.files : []);
const primaryFiles = computed<any[]>(() => files.value.filter(f => !['danmaku', 'comment'].includes(f.file_type || f.type)));
const fileGroups = computed(() => {
  const groups = new Map<string, any[]>();
  for (const file of primaryFiles.value) {
    const location = file.location || 'other';
    if (!groups.has(location)) groups.set(location, []);
    groups.get(location)!.push(file);
  }
  return [...groups.entries()];
});
const overview = computed(() => {
  const list = primaryFiles.value;
  if (!list.length) return null;
  return {
    total: list.length,
    locations: new Set(list.map(f => f.location || 'other')).size,
    videos: list.filter(f => ['video', 'danmaku_video'].includes(f.file_type)).length,
    archives: list.filter(f => Boolean(f.version)).length,
  };
});
// 弹幕归档版本：分组键/优先级与老框架 preferredDanmakuVersions 完全一致
const danmakuVersions = computed<any[]>(() => {
  const groups = new Map<string, any>();
  const priority: Record<string, number> = { json: 0, txt: 1, xml: 2 };
  for (const file of files.value.filter(f => (f.file_type || f.type) === 'danmaku')) {
    const directory = String(file.path || '').split('/').slice(0, -1).join('/');
    const key = `${file.location || 'other'}|${directory}|${file.version || 'latest'}`;
    const previous = groups.get(key);
    if (!previous || (priority[file.format] ?? 9) < (priority[previous.format] ?? 9)) groups.set(key, file);
  }
  return [...groups.values()];
});
const commentVersions = computed<any[]>(() => files.value.filter(f => (f.file_type || f.type) === 'comment'));
// 清晰度 pill：静态全量 + 按实际可用置灰（老框架 refreshQualityPills 语义）
const qualityAvailable = computed<Set<number>>(() => {
  const available = new Set((urls.value?.qualities || []).map(q => q.quality));
  const accept = new Set(urls.value?.accept_quality || []);
  return new Set([...available].filter(qn => accept.has(qn)));
});
const pages = computed<any[]>(() => videoInfo.value?.pages || []);
const allPagesSelected = computed(() => pages.value.length > 0 && selectedPages.value.size === pages.value.length);

/* ---------------- 手动查询抽屉（老框架 renderDrawerContentForManualQuery 的独立渲染） ---------------- */
const isManual = computed(() => drawerState.video?.source === 'manual');
const manualVideo = computed<any>(() => drawerState.video?.manualVideo || null);
const manualPic = computed<string>(() => {
  const raw = manualVideo.value?.poster || manualVideo.value?.pic || '';
  return raw ? videoApi.proxyImage(raw) : '';
});
const manualDuration = computed(() => {
  const d = manualVideo.value?.duration ?? manualVideo.value?.length;
  if (typeof d === 'number') return formatDuration(d);
  return String(d || '');
});
// 对齐老框架 manualQueryVideos[].view = (play || 0).toLocaleString()：千分位而非万/亿缩写。
const manualView = computed(() => {
  const v = manualVideo.value?.view ?? manualVideo.value?.play;
  return v != null && v !== '' ? (Number(v) || 0).toLocaleString() : '--';
});
// 对齐老框架 manualQueryVideos[].pubdate：created 转 zh-CN 日期，缺失时回退 pubdate 字符串。
const manualPubdate = computed(() => {
  const mv = manualVideo.value || {};
  const created = Number(mv.created ?? mv.pubdate);
  if (created > 0) return new Date(created * 1000).toLocaleDateString('zh-CN');
  return (mv.pubdate != null && String(mv.pubdate) !== '') ? String(mv.pubdate) : '--';
});

/* ---------------- 打开抽屉：已下载只拉本地 detail + 日志；手动查询拉取流与分P ---------------- */
watch(() => drawerState.video, async (v) => {
  const generation = ++drawerGeneration;
  stopBurnPolling();
  if (!v) return;
  detail.value = null;
  videoInfo.value = null; liveStatsLoaded.value = false;
  urls.value = null; selectedPages.value = new Set();
  streamBlockedMessage.value = '';
  logLines.value = []; sidecarViewer.value = null; sidecarError.value = '';
  selectMode.value = false; selectedFilePaths.value = new Set();
  burningTask.value = null;
  if (v.source === 'manual') {
    // 手动查询抽屉无本地详情/日志；默认画质取服务端设置（条目 default_quality），
    // 与老框架一致：打开即异步拉取流可用性（置灰不可用档）与分P列表。
    selectedQn.value = Number(manualVideo.value?.default_quality) || 80;
    void loadUrls();
    return;
  }
  // 对齐老框架 _state.selectedQuality：全局持久，已下载抽屉打开时不重置。
  await refreshDetail(generation);
  void loadLogs();
}, { immediate: true });

watch(() => drawerState.visible, (visible) => { if (!visible) stopBurnPolling(); });

async function refreshDetail(generation = drawerGeneration) {
  const target = drawerState.video;
  if (!target) return;
  if (generation === drawerGeneration) detailLoading.value = true;
  try {
    // 对齐老框架 openVideoDrawer：始终拉单视频详情（只有它带完整 files/burned/blogger）；
    // 未命中或网络异常时静默退回看板卡片数据渲染（drawer.js:44-73 的 cachedVideo 兜底链）。
    const r: any = await historyApi.detail(target.bvid, (target as any).history_id ?? (target as any).id);
    if (generation !== drawerGeneration || drawerState.video?.bvid !== target.bvid) return;
    detail.value = r?.video || null;
  } catch (e) {
    console.warn('[drawer] 视频详情加载失败，退回看板卡片数据：', e);
    if (generation === drawerGeneration) detail.value = null;
  } finally {
    if (generation === drawerGeneration) detailLoading.value = false;
  }
}

async function loadUrls(silent = false) {
  const target = drawerState.video;
  if (!target) return;
  const generation = drawerGeneration;
  try {
    // 老框架 refreshQualityPills 只传 { bvid }；画质选择是纯本地状态。
    const result = await videoApi.getVideoUrls(target.bvid);
    if (generation !== drawerGeneration || drawerState.video?.bvid !== target.bvid) return;
    urls.value = result;
    if (!result) return;
    // 默认选中：服务端 default_quality 优先，取不超过它的最高可用档；否则最高可用。
    const defaultQn = Number(result.default_quality) || 80;
    const available = qualityAvailable.value;
    const picked = [...QUALITY_OPTIONS].sort((a, b) => b.qn - a.qn)
      .find(q => q.qn <= defaultQn && available.has(q.qn))?.qn
      ?? [...QUALITY_OPTIONS].find(q => available.has(q.qn))?.qn
      ?? defaultQn;
    selectedQn.value = picked;
    // 分P列表：对齐老框架 loadManualPages，手动抽屉打开时异步拉取 info 填充分P多选列表。
    if (pages.value.length === 0) void loadVideoInfo(silent);
  } catch (e: any) {
    if (generation !== drawerGeneration || drawerState.video?.bvid !== target.bvid) return;
    // 充电专属/付费无权限（后端 402）：抽屉内静默提示，不弹 toast——
    // 用户点“开始下载”时 gate 预检会给一次明确的提示，避免双重报错。
    if (e?.status === 402) {
      streamBlockedMessage.value = e?.message || '该视频需要充电或付费权限';
      return;
    }
    if (!silent) toast.error(e?.message || '加载视频链接失败');
  }
}

/** 按需拉 B 站实时信息；silent=true 时失败不打扰（仅分P列表用途）。 */
async function loadVideoInfo(silent = false) {
  const target = drawerState.video;
  if (!target) return;
  const generation = drawerGeneration;
  if (!silent) liveStatsLoading.value = true;
  try {
    const info = await videoApi.info(target.bvid);
    if (generation !== drawerGeneration || drawerState.video?.bvid !== target.bvid) return;
    if (info) {
      videoInfo.value = info;
      liveStatsLoaded.value = true;
      if (selectedPages.value.size === 0) {
        selectedPages.value = new Set((info.pages || []).map((_, i) => i));
      }
    }
  } catch (e: any) {
    if (!silent && generation === drawerGeneration && drawerState.video?.bvid === target.bvid) {
      toast.error(e?.message || '刷新视频数据失败');
    }
  } finally {
    if (generation === drawerGeneration) liveStatsLoading.value = false;
  }
}

/* ---------------- 下载 / 重试 / 任务控制（对齐 media-actions.js） ---------------- */
async function gateDownloadCheck(bvid: string): Promise<{ blocked: boolean; message?: string }> {
  try {
    const data: any = await videoApi.gateDownload(bvid);
    if (!data) return { blocked: false };
    if (data.allow === false) return { blocked: true, message: data.message || '该视频无法下载' };
    if (data.state === 'removed') return { blocked: true, message: '该视频已下架，无法下载' };
    if (data.state === 'pay_blocked') {
      const payNote = data.pay_note || '';
      if (payNote.endsWith('_no_permission')) {
        const reason = payNote.includes('upower') ? '充电专属' : payNote.includes('ugc_pay') ? 'UGC付费' : '付费';
        return { blocked: true, message: `该视频为${reason}内容，当前账号无观看权限` };
      }
      if (payNote.endsWith('_paid')) return { blocked: false }; // 已付费：允许下载
      return { blocked: false, message: data.message };
    }
    return { blocked: !data.allow, message: data.message };
  } catch {
    return { blocked: false }; // 预检异常放行，避免阻塞
  }
}

async function startOrRetryDownload() {
  const target = drawerState.video;
  if (!target) return;
  const gate = await gateDownloadCheck(target.bvid);
  if (gate.blocked) { toast.warn(gate.message || '该视频无法下载，已跳过'); return; }
  try {
    // 对齐老框架：已下载抽屉的重试/开始（startVideoDownload / retryVideoDownload）只传 {bvid, qn}，
    // 不携带 pages（重试只下 P1）；手动抽屉（startVideoDownloadFromManual）多P时携带勾选分P。
    const payload: any = { bvid: target.bvid, qn: selectedQn.value };
    if (isManual.value && pages.value.length > 1) {
      const selected = pages.value.filter((_, i) => selectedPages.value.has(i));
      if (selected.length === 0) { toast.warn('请至少选择一个分P'); return; }
      payload.pages = selected.map((p, i) => ({ cid: p.cid, page: p.page ?? i + 1, part: p.part || '' }));
    }
    const { message } = await postFull<any>('/api/download/start', payload);
    // 老框架：已下载路径硬编码“开始下载”；手动路径优先后端 result.message。
    toast.success(isManual.value ? (message || '开始下载') : '开始下载');
    closeDrawer();
    void download.refreshStatus();
  } catch (e: any) { toast.error(e?.message || '启动下载失败'); }
}

async function pauseTask() {
  if (!taskId.value) return;
  try { await downloadApi.pause(taskId.value); await refreshDetail(); }
  catch (e: any) { toast.error(e?.message || '暂停失败'); }
}
async function resumeTask() {
  if (!taskId.value) return;
  try { await downloadApi.resume(taskId.value); await refreshDetail(); }
  catch (e: any) { toast.error(e?.message || '恢复失败'); }
}

async function deleteRecord() {
  const target = drawerState.video;
  if (!target) return;
  const ok = await confirmDialog({
    title: '删除记录',
    confirmText: '删除',
    tone: 'danger',
    message: `确认删除视频 ${target.bvid} 的记录？\n\n将同时删除：\n- 本地视频文件\n- 本地封面文件\n- 弹幕 / 字幕侧车文件\n- download_task 记录\n- history 记录\n\n此操作不可撤销。`,
  });
  if (!ok) return;
  try {
    const r: any = await historyApi.delete(target.bvid, historyId.value ?? undefined, true);
    toast.success(`已删除记录（${(r?.removed_files || []).length} 个文件）`);
    closeDrawer();
    void historyStore.loadBoard(historyStore.activeTab);
  } catch (e: any) { toast.error(e?.message || '删除失败'); }
}

/* ---------------- 弹幕 / 评论 / 封面（同步接口，按 count 提示） ---------------- */
async function downloadDanmakuNow() {
  const target = drawerState.video;
  if (!target) return;
  try {
    const r = await videoApi.downloadDanmaku(target.bvid, {
      source: artifactSource.value,
      page: video.value?.page ?? undefined,
      history_id: historyId.value ?? undefined,
    });
    if (r && Number(r.count) > 0) { toast.success(`弹幕下载完成: ${r.count} 条`); await refreshDetail(); }
    else toast.info('该视频暂无弹幕');
  } catch (e: any) { toast.error(e?.message || '弹幕下载失败'); }
}
async function downloadCommentsNow() {
  const target = drawerState.video;
  if (!target) return;
  try {
    const r = await videoApi.downloadComments(target.bvid, {
      source: artifactSource.value,
      history_id: historyId.value ?? undefined,
    });
    if (r && Number(r.count) > 0) { toast.success(`评论下载完成: ${r.count} 条主评论`); await refreshDetail(); }
    else toast.info('该视频暂无评论');
  } catch (e: any) { toast.error(e?.message || '评论下载失败'); }
}
async function downloadCoverNow() {
  const target = drawerState.video;
  if (!target) return;
  try {
    await videoApi.downloadCover(target.bvid);
    toast.success('封面下载已开始');
  } catch (e: any) { toast.error(e?.message || '封面下载失败'); }
}
function openBilibili() {
  if (!drawerState.video) return;
  // noopener/noreferrer：切断新页面对本页的 window.opener 引用，防反向钓鱼篡改。
  window.open(`https://www.bilibili.com/video/${drawerState.video.bvid}`, '_blank', 'noopener,noreferrer');
}

/* ---------------- 手动抽屉：保存到本机（对齐老框架 saveManualToLocal） ---------------- */
function pickVideoQuality(qualities: any[], acceptQuality: Set<number>, preferredQn: number): any | null {
  const available = qualities.filter(q => acceptQuality.has(q.quality) && q.url);
  if (!available.length) return null;
  return available.find(q => q.quality === preferredQn)
    || available.filter(q => q.quality <= preferredQn).sort((a, b) => b.quality - a.quality)[0]
    || [...available].sort((a, b) => b.quality - a.quality)[0];
}
function formatAudioQuality(id: number, kbps: number): string {
  const names: Record<number, string> = { 30251: 'Hi-Res 无损', 30250: '杜比全景声', 30280: '192K', 30232: '132K', 30216: '64K' };
  if (names[id]) return names[id];
  return kbps > 0 ? `${kbps}K` : '未知音质';
}
async function saveManualToLocal() {
  const target = drawerState.video;
  if (!target) return;
  const gate = await gateDownloadCheck(target.bvid);
  if (gate.blocked) { toast.warn(gate.message || '该视频无法下载，已跳过'); return; }
  const safeTitle = String(manualVideo.value?.title || target.title || target.bvid).replace(/[^\w\u4e00-\u9fff\-_. ]/g, '');
  toast.info('正在获取下载链接...');
  try {
    // B 站为 DASH 分离流，浏览器端无法合并，因此保存为两个文件（视频 + 音频）。
    const [videoRequest, audioRequest] = await Promise.allSettled([
      videoApi.getVideoUrls(target.bvid),
      videoApi.getAudioUrl(target.bvid),
    ]);
    const videoResult: any = videoRequest.status === 'fulfilled' ? videoRequest.value : null;
    const audioResult: any = audioRequest.status === 'fulfilled' ? audioRequest.value : null;
    if (!videoResult && !audioResult) { toast.error('获取下载链接失败'); return; }
    let saved = 0;
    if (videoResult) {
      const accept = new Set<number>(videoResult.accept_quality || []);
      const targetStream = pickVideoQuality(videoResult.qualities || [], accept, selectedQn.value || 80);
      if (targetStream) {
        const qualityTag = String(targetStream.quality_name || targetStream.quality).replace(/\s+/g, '_');
        downloadViaIframe(downloadApi.proxy(targetStream.url, `${safeTitle}_${target.bvid}_${qualityTag}.${targetStream.format || 'mp4'}`));
        saved++;
      } else {
        toast.warn('无可用视频流（可能需要登录或大会员）');
      }
    }
    if (audioResult) {
      const aq = (audioResult.qualities || [])[0];
      if (aq?.url) {
        const ext = audioResult.ext || 'm4a';
        const kbps = Math.round((aq.bandwidth || 0) / 1000);
        const tag = formatAudioQuality(aq.id, kbps).replace(/\s+/g, '_');
        const audioUrl = downloadApi.proxy(aq.url, `${safeTitle}_${target.bvid}_音频_${tag}.${ext}`);
        // 错开触发，避免浏览器忽略连续的 iframe 下载。
        setTimeout(() => downloadViaIframe(audioUrl), 800);
        saved++;
      }
    }
    if (saved > 0) toast.success('已开始保存到本机：视频与音频为两个文件，请查看浏览器下载栏');
  } catch (e: any) { toast.error(e?.message || '保存到本机失败'); }
}

/* ---------------- 烧录（can_burn 判定 + 轮询） ---------------- */
function stopBurnPolling() {
  burnPollGeneration += 1;
  if (burnPollTimer) window.clearTimeout(burnPollTimer);
  burnPollTimer = null;
}
function pollBurnStatus(burnTaskId: string, source: 'danmaku' | 'subtitle', generation: number, attempt = 0) {
  if (generation !== burnPollGeneration) return;
  burnPollTimer = window.setTimeout(async () => {
    burnPollTimer = null;
    if (generation !== burnPollGeneration) return;
    let status: any = null;
    try { status = await downloadApi.burnStatus(burnTaskId); } catch { /* 轮询容错 */ }
    if (generation !== burnPollGeneration) return;
    if (status?.status === 'completed') {
      burningTask.value = null;
      toast.success('烧录完成！');
      await refreshDetail();
      return;
    }
    if (status?.status === 'failed') {
      burningTask.value = null;
      toast.error(status.message || '烧录失败');
      return;
    }
    if (attempt >= 600) {
      burningTask.value = null;
      toast.warn('烧录超时，请稍后刷新查看');
      return;
    }
    pollBurnStatus(burnTaskId, source, generation, attempt + 1);
  }, 1000);
}
async function burnMedia(source: 'danmaku' | 'subtitle') {
  const target = drawerState.video;
  if (!target) return;
  if (burningTask.value) { toast.info('该视频正在烧录中，请耐心等待'); return; }
  burningTask.value = source;
  toast.info('正在烧录，请等待...');
  try {
    const result: any = await downloadApi.burn(target.bvid, source, historyId.value ?? null);
    if (!result?.task_id) throw new Error('后端未返回烧录任务 ID');
    pollBurnStatus(result.task_id, source, burnPollGeneration);
  } catch (e: any) {
    toast.error(e?.message || '启动烧录失败');
    burningTask.value = null;
  }
}

/* ---------------- 侧车浏览器（按 path 精确定位版本） ---------------- */
async function loadCommentsViewer(path = '') {
  const target = drawerState.video;
  if (!target) return;
  const generation = drawerGeneration;
  sidecarLoading.value = true; sidecarError.value = '';
  sidecarViewer.value = { kind: 'comments', path, exists: false };
  try {
    const data: any = await videoApi.comments(target.bvid, {
      path: path || undefined,
      history_id: historyId.value ?? target.history_id,
    });
    if (generation !== drawerGeneration || drawerState.video?.bvid !== target.bvid) return;
    sidecarViewer.value = { kind: 'comments', path, ...(data || {}) };
  } catch (e: any) {
    if (generation === drawerGeneration && drawerState.video?.bvid === target.bvid) sidecarError.value = e?.message || '评论暂不可用';
  } finally {
    if (generation === drawerGeneration && drawerState.video?.bvid === target.bvid) sidecarLoading.value = false;
  }
}
async function loadDanmakuViewer(path: string) {
  const target = drawerState.video;
  if (!target || !path) return;
  const generation = drawerGeneration;
  sidecarLoading.value = true; sidecarError.value = '';
  sidecarViewer.value = { kind: 'danmaku', path, exists: false };
  try {
    const data: any = await videoApi.danmaku(target.bvid, path, historyId.value ?? target.history_id);
    if (generation !== drawerGeneration || drawerState.video?.bvid !== target.bvid) return;
    sidecarViewer.value = { kind: 'danmaku', path, ...(data || {}) };
  } catch (e: any) {
    if (generation === drawerGeneration && drawerState.video?.bvid === target.bvid) sidecarError.value = e?.message || '弹幕暂不可用';
  } finally {
    if (generation === drawerGeneration && drawerState.video?.bvid === target.bvid) sidecarLoading.value = false;
  }
}

/* ---------------- 日志（打开自动拉一次 + 手动刷新） ---------------- */
async function loadLogs() {
  const target = drawerState.video;
  if (!target) return;
  const generation = drawerGeneration;
  logsLoading.value = true;
  try {
    const r: any = await logsApi.bvid(target.bvid);
    if (generation !== drawerGeneration || drawerState.video?.bvid !== target.bvid) return;
    const arr = Array.isArray(r?.logs) ? r.logs : (Array.isArray(r) ? r : []);
    logLines.value = arr.map((l: any) => {
      let time = l.time || '';
      if (!time && l.timestamp) time = new Date(Number(l.timestamp) * 1000).toLocaleString();
      if (!time && l.created_at) time = new Date(l.created_at).toLocaleString();
      const level = String(l.level || '').toLowerCase();
      return {
        time,
        level: String(l.level || 'INFO'),
        msg: l.msg ?? l.message ?? '',
        cls: ['error', 'warn', 'warning'].includes(level) ? 'log-error' : 'log-info',
      } as LogLine;
    });
  } catch { /* 静默：日志不可用不打扰 */ }
  finally { if (generation === drawerGeneration) logsLoading.value = false; }
}

/* ---------------- 目录 / 路径 / 浏览器下载 ---------------- */
async function openDirectoryTop() {
  const target = drawerState.video;
  if (!target) return;
  try {
    await historyApi.openDirectory(target.bvid, historyId.value ?? undefined, relativePath.value || undefined);
    toast.success('已请求打开目录');
  } catch (e: any) { toast.error(e?.message || '打开目录失败'); }
}
async function openFileDirectory(path: string) {
  const target = drawerState.video;
  if (!target || !path) return;
  try {
    await historyApi.openDirectory(target.bvid, historyId.value ?? undefined, path);
    toast.success('已请求打开目录');
  } catch (e: any) { toast.error(e?.message || '打开目录失败'); }
}
async function copyToClipboard(text: string) {
  if (!text) return;
  try {
    if (navigator.clipboard?.writeText) await navigator.clipboard.writeText(text);
    else {
      const ta = document.createElement('textarea');
      ta.value = text; ta.style.position = 'fixed'; ta.style.left = '-9999px';
      document.body.appendChild(ta); ta.select(); document.execCommand('copy'); ta.remove();
    }
    toast.success('路径已复制');
  } catch { toast.warn('复制失败，请手动选择'); }
}
function toggleSelectMode(enabled: boolean) {
  selectMode.value = enabled;
  selectedFilePaths.value = enabled
    ? new Set([...primaryFiles.value, ...danmakuVersions.value, ...commentVersions.value].map(f => String(f.path)).filter(Boolean))
    : new Set();
}
function toggleFile(path: string) {
  const next = new Set(selectedFilePaths.value);
  if (next.has(path)) next.delete(path); else next.add(path);
  selectedFilePaths.value = next;
}
function downloadViaIframe(url: string) {
  const iframe = document.createElement('iframe');
  iframe.hidden = true;
  iframe.src = url;
  document.body.appendChild(iframe);
  setTimeout(() => iframe.remove(), 60000);
}
function browserDownloadFile(file: any) {
  const target = drawerState.video;
  if (!target || !file?.path) return;
  const params = new URLSearchParams({ bvid: target.bvid, path: String(file.path) });
  const hid = historyId.value;
  if (hid != null) params.set('history_id', String(hid));
  downloadViaIframe(`/api/history/file-download?${params.toString()}`);
  toast.success(`服务器到本机：正在下载 ${file.name || '文件'}，请查看浏览器下载栏`);
}
async function browserDownloadSelected() {
  const picked = [...primaryFiles.value, ...danmakuVersions.value, ...commentVersions.value]
    .filter(f => f.path && selectedFilePaths.value.has(String(f.path)));
  if (!picked.length) { toast.info('请先勾选要保存到本机的文件'); return; }
  toast.info(`开始下载 ${picked.length} 个文件；如浏览器询问"允许多个文件下载"，请选择允许`);
  for (const file of picked) {
    browserDownloadFile(file);
    await new Promise(resolve => window.setTimeout(resolve, 600));
  }
}

/* ---------------- 分P / 清晰度 ---------------- */
function togglePage(idx: number) {
  const next = new Set(selectedPages.value);
  if (next.has(idx)) next.delete(idx); else next.add(idx);
  selectedPages.value = next;
}
function toggleAllPages() {
  selectedPages.value = allPagesSelected.value ? new Set() : new Set(pages.value.map((_, i) => i));
}
function selectQuality(qn: number) {
  // 与老框架一致：选画质是纯本地状态，不重新取流。
  if (!qualityAvailable.value.has(qn)) return;
  selectedQn.value = qn;
}

/* ---------------- 格式化工具（与老框架 utils.js / history.js 对齐） ---------------- */
function clampPercent(value: any): number {
  const n = Number(value);
  return Number.isFinite(n) ? Math.min(100, Math.max(0, n)) : 0;
}
function formatSize(bytes?: number): string {
  // 对齐老框架 utils.js::formatFileSize：log 取单位 + toFixed(2) 后 parseFloat 去尾零（1.75 MB 而非 1.8 MB）。
  const value = Number(bytes);
  if (!Number.isFinite(value) || value <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${parseFloat((value / Math.pow(1024, index)).toFixed(2))} ${units[index]}`;
}
function formatSpeed(b?: number): string { return b ? `${formatSize(b)}/s` : ''; }
function formatDuration(s?: number): string {
  if (!s) return '';
  const sec = Math.floor(s % 60), min = Math.floor((s / 60) % 60), hr = Math.floor(s / 3600);
  const mm = String(min).padStart(2, '0'), ss = String(sec).padStart(2, '0');
  return hr > 0 ? `${hr}:${mm}:${ss}` : `${min}:${ss}`;
}
function formatViewCount(view?: number): string {
  const v = Number(view) || 0;
  if (v >= 100000000) return (v / 100000000).toFixed(1) + '亿';
  if (v >= 10000) return (v / 10000).toFixed(1) + '万';
  return String(v);
}
function formatTimestamp(ts?: number): string {
  const t = Number(ts) || 0;
  if (t <= 0) return '';
  const d = new Date(t * 1000);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
function locationLabel(location?: string): string {
  const loc = location || 'other';
  if (loc === 'manual') return '手动下载区域';
  if (loc.startsWith('auto:')) return `自动下载区域 · UID ${loc.slice(5)}`;
  if (loc.startsWith('other:')) return `自定义目录 · ${loc.slice(6)}`;
  return '其他下载区域';
}
function formatArchiveVersion(version?: string): string {
  if (!version) return '最新';
  const m = /^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})-(\d{3})$/.exec(version);
  if (!m) return version;
  return `${m[1]}-${m[2]}-${m[3]} ${m[4]}:${m[5]}:${m[6]}.${m[7]}`;
}
function sidecarVersionLabel(f: any): string {
  return f.version ? formatArchiveVersion(f.version) : (f.is_current ? '当前最新版' : '最新副本');
}
function fileVersionLabel(f: any): string {
  return f.version ? formatArchiveVersion(f.version) : (f.is_current ? '当前' : '副本');
}
function hasSidecar(value: any): boolean {
  // 对齐老框架 renderSidecarIcons 的 `sidecar[it.key]` 纯真值判断（后端字段是 bool）。
  return !!value;
}
function formatDanmakuTime(item: any): string {
  const raw = Number(item?.progress ?? item?.time ?? item?.ctime ?? 0);
  const seconds = raw > 10000 ? raw / 1000 : raw;
  return `${Math.max(0, Math.floor(seconds / 60))}:${String(Math.max(0, Math.floor(seconds % 60))).padStart(2, '0')}`;
}
function formatSidecarTime(ts?: number): string {
  if (!ts) return '';
  const d = new Date(ts * 1000);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleString();
}
function commentUserTone(value?: string): string {
  const tones: Record<string, string> = {
    '#00a1d6': ' cmt-user-cyan', '#00aeec': ' cmt-user-cyan', '#67c23a': ' cmt-user-green',
    '#9b59b6': ' cmt-user-purple', '#f39800': ' cmt-user-orange', '#fb7299': ' cmt-user-pink', '#ff6699': ' cmt-user-pink',
  };
  return tones[String(value || '').trim().toLowerCase()] || '';
}
const pubDate = computed(() => {
  const v = video.value;
  if (!v) return '';
  return v.pub_date || (v.pub_timestamp ? formatTimestamp(Number(v.pub_timestamp)) : '') || (v.pubdate ? String(v.pubdate) : '');
});
// 对齐老框架 history.js::renderSidecarIcons 四项（视频/弹幕/评论/字幕，✓/— 形态）。
const SIDECAR_ICONS = [
  { key: 'video', label: '视频', icon: 'fa-film' },
  { key: 'danmaku', label: '弹幕', icon: 'fa-comment-dots' },
  { key: 'comments', label: '评论', icon: 'fa-comments' },
  { key: 'subtitle', label: '字幕', icon: 'fa-closed-captioning' },
];
// 对齐老框架 renderDrawerFiles 空态分支：无主产物但有弹幕/评论文件时提示从下方入口查看。
const hasSidecarFiles = computed(() => files.value.some(f => ['danmaku', 'comment'].includes(f.file_type || f.type)));

onUnmounted(stopBurnPolling);
</script>

<template>
  <!-- DOM 常驻：进出场动画由 .active class 驱动（与老框架一致） -->
  <div class="drawer-overlay" :class="{ active: drawerState.visible }" id="drawer-overlay" data-action="close-video-drawer" @click="closeDrawer"></div>
  <div ref="drawerRoot" class="video-drawer" :class="{ active: drawerState.visible, 'browser-select-on': selectMode }" id="video-drawer" role="dialog" aria-modal="true" aria-labelledby="drawer-video-title">
    <div class="drawer-header">
      <div class="drawer-title" id="drawer-video-title" :title="drawerTitle">{{ detailLoading ? '加载中...' : drawerTitle }}</div>
      <button class="drawer-close-btn" data-action="close-video-drawer" aria-label="关闭" @click="closeDrawer">
        <i class="fa-solid fa-times"></i>
      </button>
    </div>

    <div class="drawer-body" id="drawer-body">
      <!-- 手动查询抽屉：独立渲染（老框架 renderDrawerContentForManualQuery），不套本地详情 tab -->
      <template v-if="isManual">
        <div class="drawer-preview">
          <img v-if="manualPic" :src="manualPic" alt="" @error="($event.target as HTMLImageElement).style.display = 'none'" />
          <span v-if="manualDuration" class="drawer-preview-badge">{{ manualDuration }}</span>
        </div>

        <div class="drawer-info-row">
          <div class="drawer-info-item">
            <span class="drawer-info-label">发布时间</span>
            <span class="drawer-info-value">{{ manualPubdate }}</span>
          </div>
          <div class="drawer-info-item">
            <span class="drawer-info-label">播放量</span>
            <span class="drawer-info-value">{{ manualView }}</span>
          </div>
          <div class="drawer-info-item">
            <span class="drawer-info-label">状态</span>
            <span class="drawer-info-value drawer-info-muted">未下载</span>
          </div>
        </div>

        <div class="drawer-section">
          <div class="drawer-section-title">视频下载</div>
          <div class="quality-pills" id="quality-pills-container">
            <button v-for="q in QUALITY_OPTIONS" :key="q.qn"
                    :class="['quality-pill', { active: selectedQn === q.qn, disabled: urls && !qualityAvailable.has(q.qn) }]"
                    :disabled="!!urls && !qualityAvailable.has(q.qn)"
                    @click="selectQuality(q.qn)">
              {{ q.label }}
              <span v-if="q.tag" class="quality-pill-tag">{{ q.tag }}</span>
            </button>
          </div>
          <div v-if="streamBlockedMessage" class="drawer-pay-note">
            <i class="fa-solid fa-coins"></i>
            {{ streamBlockedMessage }}
          </div>
          <div v-if="pages.length > 1" class="drawer-pages" id="drawer-pages-section">
            <div class="drawer-pages-header">
              <span class="drawer-pages-label">分P选择（共 {{ pages.length }} 个）</span>
              <button class="btn btn-ghost btn-sm" @click="toggleAllPages">全选/全不选</button>
            </div>
            <div class="drawer-pages-list">
              <label v-for="(p, i) in pages" :key="p.cid" class="drawer-page-item">
                <input type="checkbox" class="drawer-page-check" :checked="selectedPages.has(i)" @change="togglePage(i)" />
                <span class="drawer-page-index">P{{ p.page ?? i + 1 }}</span>
                <span class="drawer-page-title">{{ p.part || `P${p.page ?? i + 1}` }}</span>
              </label>
            </div>
          </div>
          <div class="drawer-actions">
            <button class="drawer-btn drawer-btn-primary" data-action="start-manual-video" data-mutating @click="startOrRetryDownload">
              <i class="fa-solid fa-download"></i>
              开始下载
            </button>
            <button class="drawer-btn drawer-btn-ghost" data-action="save-manual-to-local"
                    title="用服务器登录态经代理把视频流和音频流直接保存到本机，不占用服务器存储" @click="saveManualToLocal">
              <i class="fa-solid fa-desktop"></i>
              保存到本机
            </button>
          </div>
        </div>

        <div class="drawer-section">
          <div class="drawer-section-title">更多选项</div>
          <div class="drawer-extras">
            <button class="drawer-extra-btn" @click="openBilibili">
              <i class="fa-solid fa-external-link-alt"></i>
              原视频链接
            </button>
            <button class="drawer-extra-btn" data-mutating @click="downloadDanmakuNow">
              <i class="fa-solid fa-comment-dots"></i>
              下载弹幕
            </button>
            <button class="drawer-extra-btn" data-mutating @click="downloadCommentsNow">
              <i class="fa-solid fa-comments"></i>
              下载评论
            </button>
          </div>
        </div>
      </template>

      <!-- 已下载/历史抽屉：单列布局（对齐老框架 renderDrawerContent，无 tab） -->
      <template v-else>
        <div v-if="detailLoading" class="drawer-logs-hint"><i class="fa-solid fa-spinner fa-spin"></i> 正在加载视频详情…</div>
        <div v-else-if="!video" class="drawer-empty-state">
          <i class="fa-solid fa-exclamation-circle drawer-error-icon"></i>
          <p class="drawer-empty-message">未找到该视频记录</p>
          <p class="empty-hint">该视频可能尚未入库或已被删除</p>
        </div>
        <template v-else>
          <div class="drawer-preview">
            <img :src="coverUrl" alt="" @error="($event.target as HTMLImageElement).style.display = 'none'" />
            <span v-if="video.duration" class="drawer-preview-badge">{{ formatDuration(Number(video.duration)) }}</span>
            <span class="drawer-state-badge" :class="`state-${stateDot}`">{{ stateLabel }}</span>
          </div>

          <div v-if="video.reupload_of" class="drawer-reupload-hint">
            <i class="fa-solid fa-exclamation-triangle"></i>
            可能是 <a href="javascript:void(0)" @click="openDrawer({ bvid: video.reupload_of })">{{ video.reupload_of }}</a> 的重传
          </div>

          <div class="drawer-info-row">
            <div class="drawer-info-item">
              <span class="drawer-info-label">发布时间</span>
              <span class="drawer-info-value">{{ pubDate || '--' }}</span>
            </div>
            <div class="drawer-info-item">
              <span class="drawer-info-label">播放量</span>
              <span class="drawer-info-value">{{ video.view != null ? formatViewCount(Number(video.view)) : '--' }}</span>
            </div>
            <div class="drawer-info-item">
              <span class="drawer-info-label">下载时间</span>
              <span class="drawer-info-value">{{ video.download_time || '--' }}</span>
            </div>
            <div v-if="video.md5" class="drawer-info-item">
              <span class="drawer-info-label">MD5</span>
              <span class="drawer-info-value drawer-md5" :title="video.md5">{{ String(video.md5).slice(0, 16) }}...</span>
              <span v-if="video.md5_last_checked_at" class="drawer-info-sub">校验于 {{ new Date(video.md5_last_checked_at).toLocaleString() }}</span>
            </div>
          </div>

          <div v-if="video.blogger" class="drawer-blogger">
            <img v-if="video.blogger.face" :src="videoApi.proxyImage(video.blogger.face)" class="drawer-blogger-avatar" alt=""
                 @error="($event.target as HTMLImageElement).style.display = 'none'" />
            <div v-else class="drawer-blogger-avatar blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>
            <div class="drawer-blogger-info">
              <div class="drawer-blogger-name">{{ video.blogger.name || video.blogger.uid || '--' }}</div>
              <div class="drawer-blogger-uid">UID: {{ video.blogger.uid || '--' }}</div>
            </div>
          </div>

          <div v-if="state === 'pay_blocked' && video.pay_note" class="drawer-pay-note">
            <i class="fa-solid fa-coins"></i>
            {{ String(video.pay_note).endsWith('_paid') ? '充电专属（当前账号可下载）' : '充电专属（当前账号不可下载）' }}
            <span class="drawer-info-sub">{{ video.pay_note }}</span>
          </div>

          <div v-if="filePath || relativePath" class="drawer-file-path" :title="filePath || '路径已隐藏'">
            <i class="fa-solid fa-file-video"></i>
            <span>{{ filePath || '路径已隐藏' }}</span>
            <button v-if="filePath" class="btn btn-sm btn-ghost" title="复制路径" @click="copyToClipboard(filePath)"><i class="fa-solid fa-copy"></i></button>
            <button v-if="canOpenDirectory && relativePath" class="btn btn-sm btn-ghost" title="打开文件所在目录" @click="openDirectoryTop"><i class="fa-solid fa-folder-open"></i></button>
          </div>

          <!-- 进度条（活跃状态显示：下载中 / 等待 / 已暂停） -->
          <template v-if="taskActive">
            <div class="drawer-progress">
              <progress class="drawer-progress-bar" max="100" :value="progressPercent"></progress>
            </div>
            <div class="drawer-progress-text">
              <span>{{ isPaused ? `已暂停 ${progressPercent}%` : `${progressPercent}%` }}</span>
              <span v-if="!isPaused && task?.speed">{{ formatSpeed(task.speed) }}</span>
              <span v-if="task?.downloaded_size && task?.total_size">{{ formatSize(task.downloaded_size) }} / {{ formatSize(task.total_size) }}</span>
            </div>
          </template>

          <!-- 全部产物：同时展示 manual、自动目录和历史归档。 -->
          <div class="drawer-section">
            <div class="drawer-section-title">全部产物</div>
            <div v-if="overview" class="drawer-artifact-overview">
              <span><strong>{{ overview.total }}</strong> 个文件</span>
              <span><strong>{{ overview.locations }}</strong> 个产物区域</span>
              <span><strong>{{ overview.videos }}</strong> 个视频产物</span>
              <span><strong>{{ overview.archives }}</strong> 个历史归档文件</span>
            </div>
            <!-- 服务器到本机：勾选产物后经浏览器保存（对齐老框架 renderBrowserDownloadToolbar） -->
            <div v-if="canBrowserDownload && primaryFiles.length" class="drawer-browser-download-bar">
              <label class="drawer-browser-master" title="勾选后可挑选下方要保存到本机的产物，默认全选">
                <input type="checkbox" :checked="selectMode" @change="toggleSelectMode(($event.target as HTMLInputElement).checked)" />
                <span><i class="fa-solid fa-desktop"></i> 服务器到本机</span>
              </label>
              <button class="btn btn-sm btn-primary" id="drawer-browser-download-btn" :disabled="selectedFilePaths.size === 0" :hidden="!selectMode"
                      title="把勾选的产物通过浏览器保存到本机" @click="browserDownloadSelected">
                <i class="fa-solid fa-download"></i> 下载所选（{{ selectedFilePaths.size }}）
              </button>
            </div>
            <div class="drawer-file-list" id="drawer-file-list">
              <template v-if="fileGroups.length">
                <div v-for="[location, entries] in fileGroups" :key="location" class="drawer-file-group">
                  <div class="drawer-file-group-title">
                    <span><i class="fa-solid fa-folder"></i> {{ locationLabel(location) }}</span>
                    <span>{{ entries.length }} 个文件</span>
                  </div>
                  <div v-for="(f, i) in entries" :key="String(f.path) + i" class="drawer-file-item" :data-file-type="f.file_type || f.type || 'other'">
                    <div class="drawer-file-icon"><i class="fa-solid" :class="TYPE_ICONS[f.file_type || f.type || 'other'] || 'fa-file'"></i></div>
                    <div class="drawer-file-main">
                      <div class="drawer-file-name" :title="f.display_path || f.name">{{ f.name || '未知文件' }}</div>
                      <div class="drawer-file-meta">
                        <span class="drawer-file-type">{{ TYPE_LABELS[f.file_type || f.type || 'other'] || '文件' }}</span>
                        <span class="drawer-file-version" :class="{ current: f.is_current }">{{ fileVersionLabel(f) }}</span>
                        <span v-if="f.size" class="drawer-file-size">{{ formatSize(Number(f.size)) }}</span>
                        <span v-if="f.format" class="drawer-file-format">{{ f.format }}</span>
                        <span v-if="f.modified_at">{{ new Date(Number(f.modified_at) * 1000).toLocaleString() }}</span>
                      </div>
                      <div v-if="f.display_path" class="drawer-file-display-path" :title="f.display_path">{{ f.display_path }}</div>
                    </div>
                    <div class="drawer-file-actions">
                      <label v-if="canBrowserDownload && f.path" class="drawer-file-check" title="勾选后随“下载所选”保存到本机">
                        <input type="checkbox" :checked="selectedFilePaths.has(String(f.path))" @change="toggleFile(String(f.path))" />
                      </label>
                      <button v-if="f.display_path" class="btn btn-sm btn-ghost" title="复制文件路径" @click="copyToClipboard(f.display_path)">
                        <i class="fa-solid fa-copy"></i> 路径
                      </button>
                      <button v-if="canOpenDirectory && f.path" class="btn btn-sm btn-ghost" title="打开所在目录" @click="openFileDirectory(f.path)">
                        <i class="fa-solid fa-folder-open"></i> 打开
                      </button>
                      <template v-if="(f.file_type || f.type) === 'video'">
                        <span v-if="burned.danmaku" class="burn-badge burned" title="弹幕已烧录"><i class="fa-solid fa-check"></i> 弹幕已烧录</span>
                        <span v-if="burned.subtitle" class="burn-badge burned" title="字幕已烧录"><i class="fa-solid fa-check"></i> 字幕已烧录</span>
                        <button v-if="!burned.danmaku" class="btn btn-sm btn-primary" data-mutating
                                :disabled="!canBurn || burningTask !== null"
                                :title="canBurn ? '将弹幕烧录进视频' : '当前 FFmpeg 不支持烧录（精简版缺少 ass 滤镜或视频编码器）。建议下载完整版 FFmpeg，或在 设置 → FFmpeg 中更换自定义路径'"
                                @click="burnMedia('danmaku')">
                          <i class="fa-solid fa-fire"></i> {{ burningTask === 'danmaku' ? '烧录中…' : '烧录弹幕' }}
                        </button>
                        <button v-if="!burned.subtitle" class="btn btn-sm btn-primary" data-mutating
                                :disabled="!canBurn || burningTask !== null"
                                :title="canBurn ? '将 CC 字幕烧录进视频' : '当前 FFmpeg 不支持烧录（精简版缺少 ass 滤镜或视频编码器）。建议下载完整版 FFmpeg，或在 设置 → FFmpeg 中更换自定义路径'"
                                @click="burnMedia('subtitle')">
                          <i class="fa-solid fa-closed-captioning"></i> {{ burningTask === 'subtitle' ? '烧录中…' : '烧录字幕' }}
                        </button>
                      </template>
                    </div>
                  </div>
                </div>
              </template>
              <div v-else-if="hasSidecarFiles" class="drawer-files-empty"><i class="fa-solid fa-layer-group"></i>弹幕与评论版本请从下方入口查看</div>
              <div v-else-if="sidecar" class="drawer-sidecar">
                <span v-for="it in SIDECAR_ICONS" :key="it.key" class="sidecar-icon" :class="hasSidecar(sidecar[it.key]) ? 'ok' : 'missing'"
                      :title="`${it.label}: ${hasSidecar(sidecar[it.key]) ? '已下载' : '未下载'}`">
                  <i class="fa-solid" :class="it.icon"></i>{{ hasSidecar(sidecar[it.key]) ? '✓' : '—' }}
                </span>
              </div>
              <div v-else class="drawer-files-empty"><i class="fa-solid fa-inbox"></i> 暂无本地文件记录</div>
            </div>
          </div>

          <!-- 历史弹幕与评论浏览器。 -->
          <div class="drawer-section">
            <div class="drawer-section-title">弹幕与评论历史</div>
            <div class="drawer-sidecar-browser">
              <div class="sidecar-version-column">
                <div class="sidecar-version-title"><i class="fa-solid fa-comment-dots"></i> 弹幕版本（{{ danmakuVersions.length }}）</div>
                <div class="sidecar-version-list">
                  <div v-for="v in danmakuVersions" :key="String(v.path)" class="sidecar-version-row">
                    <button class="sidecar-version-btn" :class="{ active: sidecarViewer?.path === v.path }" :title="v.path" @click="loadDanmakuViewer(v.path)">
                      <i class="fa-solid fa-comment-dots"></i>
                      <span>{{ sidecarVersionLabel(v) }}</span>
                      <small>{{ locationLabel(v.location) }} · {{ String(v.format || '').toUpperCase() }}</small>
                    </button>
                    <label v-if="canBrowserDownload && v.path" class="drawer-file-check" title="勾选后随“下载所选”保存到本机">
                      <input type="checkbox" :checked="selectedFilePaths.has(String(v.path))" @change="toggleFile(String(v.path))" />
                    </label>
                  </div>
                  <div v-if="!danmakuVersions.length" class="drawer-comments-hint">暂无本地版本</div>
                </div>
              </div>
              <div class="sidecar-version-column">
                <div class="sidecar-version-title"><i class="fa-solid fa-comments"></i> 评论版本（{{ commentVersions.length }}）</div>
                <div class="sidecar-version-list">
                  <div v-for="(f, i) in commentVersions" :key="String(f.path) + i" class="sidecar-version-row">
                    <button class="sidecar-version-btn" :class="{ active: sidecarViewer?.path === f.path }" :title="f.path" @click="loadCommentsViewer(f.path)">
                      <i class="fa-solid fa-comments"></i>
                      <span>{{ sidecarVersionLabel(f) }}</span>
                      <small>{{ locationLabel(f.location) }} · {{ String(f.format || '').toUpperCase() }}</small>
                    </button>
                    <label v-if="canBrowserDownload && f.path" class="drawer-file-check" title="勾选后随“下载所选”保存到本机">
                      <input type="checkbox" :checked="selectedFilePaths.has(String(f.path))" @change="toggleFile(String(f.path))" />
                    </label>
                  </div>
                  <div v-if="!commentVersions.length" class="drawer-comments-hint">暂无本地版本</div>
                </div>
              </div>
            </div>
            <div class="drawer-comments" id="drawer-sidecar-viewer">
              <div v-if="sidecarLoading" class="drawer-comments-hint"><i class="fa-solid fa-spinner fa-spin"></i> 加载中...</div>
              <div v-else-if="sidecarError" class="drawer-comments-hint status-error">{{ sidecarError }}</div>
              <template v-else-if="sidecarViewer">
                <!-- 评论 -->
                <template v-if="sidecarViewer.kind === 'comments'">
                  <div v-if="!sidecarViewer.exists" class="drawer-comments-hint">
                    <i class="fa-solid fa-comment-slash"></i> 未下载评论（可先点下方“下载评论”，完成后再查看）
                  </div>
                  <template v-else-if="sidecarViewer.format === 'json' && Array.isArray(sidecarViewer.comments)">
                    <div v-if="!sidecarViewer.comments.length" class="drawer-comments-hint"><i class="fa-solid fa-comment-slash"></i> 暂无评论内容</div>
                    <div v-else class="drawer-comments-list">
                      <article v-for="(c, i) in sidecarViewer.comments" :key="i" class="cmt-card">
                        <div class="cmt-line">
                          <span :class="['cmt-user', commentUserTone(c.name_color)]">{{ c.uname || '' }}</span>
                          <span v-if="Number(c.vip_status || 0) > 0" class="cmt-vip">{{ c.vip_label || '大会员' }}</span>
                          <span class="cmt-lv">Lv{{ c.level || 0 }}</span>
                          <span class="cmt-meta"><i class="fa-solid fa-thumbs-up"></i> {{ c.like || 0 }} · <i class="fa-solid fa-comment"></i> {{ c.total_replies || 0 }} · {{ formatSidecarTime(c.ctime) }}</span>
                        </div>
                        <div class="cmt-text">{{ c.message || '' }}</div>
                        <div v-if="Array.isArray(c.replies) && c.replies.length" class="cmt-replies">
                          <div class="cmt-replies-title">回复 · 显示 {{ c.replies.length }}/{{ c.total_replies || 0 }} 条</div>
                          <div v-for="(reply, j) in c.replies" :key="j" class="cmt-reply">
                            <div class="cmt-line">
                              <span :class="['cmt-user', commentUserTone(reply.name_color)]">{{ reply.uname || '' }}</span>
                              <span v-if="Number(reply.vip_status || 0) > 0" class="cmt-vip">{{ reply.vip_label || '大会员' }}</span>
                              <span class="cmt-lv">Lv{{ reply.level || 0 }}</span>
                              <span class="cmt-meta"><i class="fa-solid fa-thumbs-up"></i> {{ reply.like || 0 }} · {{ formatSidecarTime(reply.ctime) }}</span>
                            </div>
                            <div class="cmt-text">{{ reply.message || '' }}</div>
                          </div>
                        </div>
                      </article>
                    </div>
                  </template>
                  <div v-else-if="sidecarViewer.content" class="drawer-comments-hint"><i class="fa-solid fa-file-lines"></i> 该评论版本为原始文本，前端不提供直接查看</div>
                  <div v-else class="drawer-comments-hint"><i class="fa-solid fa-comment-slash"></i> 暂无评论内容</div>
                </template>
                <!-- 弹幕 -->
                <template v-else>
                  <template v-if="sidecarViewer.format === 'json' && Array.isArray(sidecarViewer.danmaku)">
                    <div class="drawer-sidecar-result-title">
                      <span><i class="fa-solid fa-comment-dots"></i> {{ sidecarViewer.danmaku.length }} 条弹幕</span>
                      <span>{{ sidecarViewer.path }}</span>
                    </div>
                    <div v-if="sidecarViewer.danmaku.length" class="drawer-danmaku-list">
                      <div v-for="(item, i) in sidecarViewer.danmaku" :key="i" class="drawer-danmaku-row">
                        <span class="drawer-danmaku-time">{{ formatDanmakuTime(item) }}</span>
                        <span class="drawer-danmaku-text">{{ item.content ?? item.text ?? item.message ?? '' }}</span>
                        <span v-if="item.mid_hash ?? item.user_hash" class="drawer-danmaku-user">{{ item.mid_hash ?? item.user_hash }}</span>
                      </div>
                    </div>
                    <div v-else class="drawer-comments-hint">暂无弹幕内容</div>
                  </template>
                  <div v-else class="drawer-comments-hint"><i class="fa-solid fa-file-lines"></i> 该弹幕版本为原始文本，前端不提供直接查看</div>
                </template>
              </template>
              <div v-else class="drawer-comments-hint">选择上方任一弹幕或评论版本查看本地内容</div>
            </div>
          </div>

          <!-- 实时数据：按需加载，避免打开抽屉即触发风控 -->
          <div class="drawer-section">
            <div class="drawer-section-title">
              实时数据
              <button class="btn btn-sm btn-ghost" :disabled="liveStatsLoading" title="从 B 站拉取最新数据（5 分钟缓存）" @click="loadVideoInfo(false)">
                <i class="fa-solid fa-sync-alt" :class="{ 'fa-spin': liveStatsLoading }"></i> 刷新数据
              </button>
            </div>
            <div class="drawer-live-stats">
              <div v-if="!liveStatsLoaded" class="drawer-live-stats-hint">点击"刷新数据"从 B 站拉取最新统计（点赞 / 投币 / 收藏 / 评论等）</div>
              <template v-else-if="videoInfo?.stat">
                <div class="drawer-live-stats-grid">
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">播放</span><span class="drawer-stat-value">{{ formatViewCount(videoInfo.stat.view) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">弹幕</span><span class="drawer-stat-value">{{ formatViewCount(videoInfo.stat.danmaku) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">评论</span><span class="drawer-stat-value">{{ formatViewCount(videoInfo.stat.reply) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">收藏</span><span class="drawer-stat-value">{{ formatViewCount(videoInfo.stat.favorite) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">投币</span><span class="drawer-stat-value">{{ formatViewCount(videoInfo.stat.coin) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">分享</span><span class="drawer-stat-value">{{ formatViewCount(videoInfo.stat.share) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">点赞</span><span class="drawer-stat-value">{{ formatViewCount(videoInfo.stat.like) }}</span></div>
                </div>
                <div class="drawer-live-owner">
                  <span>UP 主：{{ videoInfo.owner?.name || '--' }}</span>
                  <span class="drawer-info-sub">MID: {{ videoInfo.owner?.mid ?? '--' }}</span>
                </div>
              </template>
              <div v-else class="drawer-live-stats-hint">实时数据暂不可用</div>
            </div>
          </div>

          <!-- 主操作。 -->
          <div class="drawer-section">
            <div class="drawer-section-title">操作</div>
            <div class="drawer-actions">
              <button v-if="taskStatus === 'downloading' || taskStatus === 'pending'" class="drawer-btn drawer-btn-primary" disabled>
                <i class="fa-solid fa-spinner fa-spin"></i> 下载中 {{ progressPercent }}%
              </button>
              <button v-else-if="isPaused" class="drawer-btn drawer-btn-primary" disabled>
                <i class="fa-solid fa-pause"></i> 已暂停 {{ progressPercent }}%
              </button>
              <button v-else-if="state === 'completed' || state === 'tampered'" class="drawer-btn drawer-btn-success" disabled>
                <i class="fa-solid fa-check-circle"></i> 已完成
              </button>
              <button v-else-if="canRetry" class="drawer-btn drawer-btn-primary" data-mutating @click="startOrRetryDownload">
                <i class="fa-solid fa-redo"></i> 重新下载
              </button>
              <button v-else class="drawer-btn drawer-btn-primary" data-mutating @click="startOrRetryDownload">
                <i class="fa-solid fa-download"></i> 开始下载
              </button>
              <button v-if="taskId && taskActive" class="drawer-btn drawer-btn-ghost" data-mutating @click="isPaused ? resumeTask() : pauseTask()">
                <i class="fa-solid" :class="isPaused ? 'fa-play' : 'fa-pause'"></i> {{ isPaused ? '恢复下载' : '暂停下载' }}
              </button>
              <button class="drawer-btn drawer-btn-danger" data-mutating @click="deleteRecord">
                <i class="fa-solid fa-trash"></i> 删除记录
              </button>
            </div>
            <div class="drawer-extras">
              <button class="drawer-extra-btn" data-mutating @click="downloadCoverNow">
                <i class="fa-solid fa-image"></i> 下载封面
              </button>
              <button class="drawer-extra-btn" @click="openBilibili">
                <i class="fa-solid fa-external-link-alt"></i> 原视频链接
              </button>
              <button class="drawer-extra-btn" data-mutating @click="downloadDanmakuNow">
                <i class="fa-solid fa-comment-dots"></i> 下载弹幕
              </button>
              <button class="drawer-extra-btn" data-mutating @click="downloadCommentsNow">
                <i class="fa-solid fa-comments"></i> 下载评论
              </button>
            </div>
          </div>

          <!-- 日志区（按 bvid 过滤，时间倒序）。 -->
          <div class="drawer-section">
            <div class="drawer-section-title">
              日志
              <button class="btn btn-sm btn-ghost" title="刷新日志" :disabled="logsLoading" @click="loadLogs">
                <i class="fa-solid fa-sync-alt" :class="{ 'fa-spin': logsLoading }"></i> 刷新
              </button>
            </div>
            <div class="drawer-logs">
              <div v-if="logsLoading" class="drawer-logs-hint"><i class="fa-solid fa-spinner fa-spin"></i> 加载中...</div>
              <div v-else-if="!logLines.length" class="drawer-logs-hint">暂无该视频的日志</div>
              <div v-for="(l, i) in logLines" :key="i" class="drawer-log-item" :class="l.cls">
                <span class="drawer-log-time">{{ l.time }}</span>
                <span class="drawer-log-level">{{ l.level }}</span>
                <span class="drawer-log-msg">{{ l.msg }}</span>
              </div>
            </div>
          </div>
        </template>
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 分P选择列表（与老框架抽屉分P区视觉一致） */
.drawer-pages-list {
  display: grid;
  gap: 6px;
  max-height: 220px;
  overflow-y: auto;
}
.drawer-page-item {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  padding: 7px 9px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface);
  cursor: pointer;
}
.drawer-page-item:hover {
  border-color: var(--brand);
  background: var(--brand-soft);
}
.drawer-page-item input { flex: 0 0 auto; }
.drawer-page-index {
  flex: 0 0 auto;
  color: var(--brand);
  font-family: var(--font-mono);
  font-size: 12px;
  font-weight: 600;
}
.drawer-page-title {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
  font-size: 12px;
}
</style>
