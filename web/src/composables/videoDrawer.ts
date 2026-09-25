/**
 * 视频详情抽屉的数据与交互（从 VideoDrawer.vue 抽出的单例 composable，
 * 与 composables/drawer.ts 的模块级单例风格一致：抽屉全局只有一份）。
 *
 * 数据与交互遵循迁移前已验证的抽屉行为：
 * - 已下载抽屉打开只调 GET /api/history/list?bvid=（纯本地库）+ 日志，从不取流；
 *   /api/video/info 仅在"实时数据 → 刷新"时按需加载，/api/video/get-video-urls 仅手动抽屉打开时拉取
 *   （对齐老框架：已下载抽屉无清晰度 pills / 分P区，重试只传 {bvid, qn}）。
 * - 下载/重试前走 /api/video/gate-download 预检（下架/付费无权限拦截）。
 * - 弹幕/评论下载是同步接口，携带 source（manual/auto），按返回 count 提示。
 * - 删除记录固定连文件一起删（强确认），toast 显示删除文件数。
 * - 烧录用 detail.can_burn 判断能力，轮询 burn/status 直至 completed/failed。
 */
import { ref, computed, watch } from 'vue';
import { drawerState, closeDrawer } from '@/composables/drawer';
import { useDownloadStore } from '@/stores/download';
import { useHistoryStore } from '@/stores/history';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { video as videoApi, logs as logsApi, history as historyApi, download as downloadApi, cover as coverApi } from '@/api';
import type { VideoInfo, VideoUrlsResult } from '@/api/types';
// 需要消费后端 message（“开始下载”等）时用 Full 变体，在白名单文件内本地封装。
import { postFull } from '../api/client';
import {
  QUALITY_OPTIONS, clampPercent, pickVideoQuality, formatAudioQuality,
  formatDuration, formatTimestamp,
} from '@/utils/drawerFormat';

export type LogLine = { time: string; level: string; msg: string; cls: 'log-error' | 'log-info' };
export type SidecarViewer = { kind: 'comments' | 'danmaku'; path: string; exists: boolean; format?: string; comments?: any[]; danmaku?: any[]; content?: string };

function initVideoDrawer() {
  const download = useDownloadStore();
  const historyStore = useHistoryStore();
  const toast = useToastStore();

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
  const logLines = ref<LogLine[]>([]);
  const logsLoading = ref(false);
  const logsError = ref('');
  // 侧车浏览器
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
  // 活跃集合与后端 pause_task 的可暂停集合同源（MONITOR_STATUSES + paused）：
  // 漏掉 retrying 会让抽屉既无进度条也无暂停入口（该状态由后端状态机真实下发）。
  const taskActive = computed(() =>
    ['downloading', 'pending', 'paused', 'retrying'].includes(taskStatus.value),
  );
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
    logLines.value = []; logsError.value = ''; sidecarViewer.value = null; sidecarError.value = '';
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
    logsError.value = '';
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
    } catch { logsError.value = '日志加载失败，请点击刷新重试'; }
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
        ta.value = text; ta.className = 'clipboard-fallback-textarea';
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

  const pubDate = computed(() => {
    const v = video.value;
    if (!v) return '';
    return v.pub_date || (v.pub_timestamp ? formatTimestamp(Number(v.pub_timestamp)) : '') || (v.pubdate ? String(v.pubdate) : '');
  });
  // 对齐老框架 renderDrawerFiles 空态分支：无主产物但有弹幕/评论文件时提示从下方入口查看。
  const hasSidecarFiles = computed(() => files.value.some(f => ['danmaku', 'comment'].includes(f.file_type || f.type)));

  return {
    // 状态
    detailLoading, videoInfo, liveStatsLoading, liveStatsLoaded, urls, selectedQn, selectedPages,
    streamBlockedMessage, logLines, logsLoading, logsError,
    sidecarViewer, sidecarLoading, sidecarError, selectMode, selectedFilePaths, burningTask,
    // 派生数据
    video, historyId, drawerTitle, coverUrl, state, stateDot, stateLabel,
    task, taskStatus, taskId, taskActive, isPaused, progressPercent,
    canRetry, canBurn, canOpenDirectory, canBrowserDownload, filePath, relativePath,
    burned, sidecar, files, primaryFiles, fileGroups, overview,
    danmakuVersions, commentVersions, qualityAvailable, pages, allPagesSelected,
    isManual, manualVideo, manualPic, manualDuration, manualView, manualPubdate,
    pubDate, hasSidecarFiles,
    // 动作
    refreshDetail, loadVideoInfo, loadLogs,
    startOrRetryDownload, pauseTask, resumeTask, deleteRecord,
    downloadDanmakuNow, downloadCommentsNow, downloadCoverNow, openBilibili, saveManualToLocal,
    burnMedia, stopBurnPolling,
    loadCommentsViewer, loadDanmakuViewer,
    openDirectoryTop, openFileDirectory, copyToClipboard,
    toggleSelectMode, toggleFile, browserDownloadSelected,
    togglePage, toggleAllPages, selectQuality,
  };
}

export type VideoDrawerApi = ReturnType<typeof initVideoDrawer>;

let drawerApi: VideoDrawerApi | null = null;
/** 抽屉全局单份状态与动作；VideoDrawer.vue 及其子组件共同消费。 */
export function useVideoDrawer(): VideoDrawerApi {
  if (!drawerApi) drawerApi = initVideoDrawer();
  return drawerApi;
}
