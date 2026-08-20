<script setup lang="ts">
import { ref, computed, watch } from 'vue';
import { drawerState, closeDrawer } from '@/composables/drawer';
import { useDownloadStore } from '@/stores/download';
import { useToastStore } from '@/stores/toast';
import { video as videoApi, logs as logsApi, history as historyApi, settings as settingsApi } from '@/api';
import type { VideoInfo, VideoUrlsResult } from '@/api/types';

const download = useDownloadStore();
const toast = useToastStore();

/** 复制文本到剪贴板；失败时降级为 prompt。 */
async function copyToClipboard(text: string) {
  if (!text) return;
  try {
    if (navigator.clipboard?.writeText) await navigator.clipboard.writeText(text);
    else {
      // 降级：execCommand('copy') 在 https 之外受限，但尝试一次
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.position = 'fixed';
      ta.style.left = '-9999px';
      document.body.appendChild(ta);
      ta.select();
      document.execCommand('copy');
      ta.remove();
    }
    toast.success('已复制');
  } catch {
    toast.warn('复制失败，请手动选择');
    window.prompt('请复制路径：', text);
  }
}

const drawerTab = ref<'info' | 'download' | 'files' | 'logs'>('info');
const videoInfo = ref<VideoInfo | null>(null);
const urls = ref<VideoUrlsResult | null>(null);
const selectedQn = ref<number>(80);
const cid = ref<number | null>(null);
const comments = ref<{ count: number } | null>(null);
const danmaku = ref<{ count: number } | null>(null);
const logs = ref<string[]>([]);
const selectedPage = ref(0);
// 单视频详情（产物文件 + sidecar + task 状态）
const detail = ref<any>(null);
const sidecar = computed(() => (detail.value as any)?.sidecar || null);
const files = computed<any[]>(() => ((detail.value as any)?.files) || []);
const canBrowserDownload = computed(() => !!(detail.value as any)?.can_browser_download);

watch(() => drawerState.video, async (v) => {
  if (!v) return;
  drawerTab.value = 'info';
  videoInfo.value = null; urls.value = null; comments.value = null; danmaku.value = null; logs.value = [];
  detail.value = null;
  burningTask.value = null;
  try {
    const info = await videoApi.info(v.bvid);
    if (info) {
      videoInfo.value = info;
      cid.value = info.pages?.[0]?.cid ?? null;
      selectedPage.value = 0;
    }
  } catch (e: any) { toast.error(e?.message || '加载视频信息失败'); }
  // 拉单视频详情（产物文件 + sidecar 状态）
  try {
    const r: any = await historyApi.detail(v.bvid, v.history_id);
    if (r?.video) detail.value = r.video;
  } catch { /* 没历史记录也无所谓 */ }
  // 探测 FFmpeg 烧录能力；接口失败不视为不可烧录（用户可能在旧版下也能跑），
  // 这里只在显式返回 ok=false 时禁用。
  try {
    const fp: any = await settingsApi.ffmpegPath();
    if (fp && fp.available === false) {
      ffmpegOk.value = false;
      ffmpegHint.value = '当前 FFmpeg 不支持烧录（精简版缺少 ass 滤镜或视频编码器）。建议在设置 → FFmpeg 中更换自定义路径。';
    } else if (fp && fp.path) {
      ffmpegOk.value = true;
      ffmpegHint.value = '';
    }
  } catch {
    // 接口不可用 → 不禁用按钮（兜底：交给后端拦截）
    ffmpegOk.value = true;
    ffmpegHint.value = '';
  }
}, { immediate: true });

const acceptQuality = computed(() => urls.value?.accept_quality || []);
const pages = computed(() => videoInfo.value?.pages || []);
const drawerTitle = computed(() => videoInfo.value?.title || drawerState.video?.title || drawerState.video?.bvid || '视频详情');
// 当前 history 记录的主键（用于 pause/resume/retry/remove/delete）
const historyId = computed<number | null>(() => Number((detail.value as any)?.history_id) || null);
// 当前关联下载任务（detail.task 是 board.rs::build_task_info 的产物）
const taskInfo = computed<any>(() => (detail.value as any)?.task || null);
const taskId = computed<number | null>(() => Number(taskInfo.value?.task_id) || null);
const taskStatus = computed<string>(() => String(taskInfo.value?.status || 'unknown'));
const canOpenDirectory = computed(() => !!(detail.value as any)?.can_open_directory);
const filePath = computed(() => (detail.value as any)?.file_path || '');
const failureMessage = computed(() => (detail.value as any)?.failure || taskInfo.value?.error || null);
const isFailed = computed(() => ['failed', 'merge_failed'].includes(taskStatus.value));
// 是否已烧录弹幕/字幕
const burnedDanmaku = computed(() => Boolean((detail.value as any)?.burned?.danmaku));
const burnedSubtitle = computed(() => Boolean((detail.value as any)?.burned?.subtitle));
// FFmpeg 烧录能力探测：拉一次 ffmpeg-path / ffmpeg-test，
// 失败或不可用时禁用烧录按钮；命中任何视频产物行才有烧录入口。
const ffmpegOk = ref(true);
const ffmpegHint = ref('');
const burningTask = ref<string | null>(null); // 当前正在烧录的 source（danmaku/subtitle/all）

// 弹幕归档版本（json / txt / xml），从 files 里聚合 file_type='danmaku' 的条目。
const danmakuVersions = computed<Array<{ format: string; path: string; version: string | null; is_current: boolean }>>(() => {
  const out: Array<{ format: string; path: string; version: string | null; is_current: boolean }> = [];
  for (const f of files.value as Array<any>) {
    if ((f.file_type || f.type) !== 'danmaku') continue;
    const fmt = String(f.format || (f.name?.split('.').pop() || 'xml')).toLowerCase();
    out.push({
      format: fmt,
      path: String(f.path || ''),
      version: f.version ?? null,
      is_current: !!f.is_current,
    });
  }
  // 优先级 json > txt > xml，与旧版 drawer-render.preferredDanmakuVersions 一致。
  const order = { json: 0, txt: 1, xml: 2 } as Record<string, number>;
  out.sort((a, b) => (order[a.format] ?? 99) - (order[b.format] ?? 99));
  return out;
});
// 封面是否已下载（detail.cover_local_path 字段或 files 中 file_type='cover'）。
const coverExists = computed(() => {
  const d: any = detail.value;
  if (d?.cover_local_path) return true;
  return (files.value as Array<any>).some(f => (f.file_type || f.type) === 'cover');
});
// 用户当前选中的弹幕归档格式：默认 json（与旧版 preferredDanmakuVersions 一致）。
const selectedDanmakuFormat = ref<string>('json');
watch(danmakuVersions, (vs) => {
  if (!vs.length) return;
  if (!vs.some(v => v.format === selectedDanmakuFormat.value)) {
    selectedDanmakuFormat.value = vs[0].format;
  }
}, { immediate: true });
const selectedDanmakuPath = computed(() => {
  const hit = danmakuVersions.value.find(v => v.format === selectedDanmakuFormat.value);
  return hit?.path || '';
});
function selectDanmakuFormat(fmt: string) { selectedDanmakuFormat.value = fmt; }
const downloadingCover = ref(false);
async function downloadCoverNow() {
  if (!drawerState.video) return;
  downloadingCover.value = true;
  try {
    await videoApi.downloadCover(drawerState.video.bvid);
    toast.success('已请求下载封面');
    // 简单兜底：3 秒后重新拉详情刷新文件列表。
    window.setTimeout(async () => {
      try {
        const r: any = await historyApi.detail(drawerState.video!.bvid, drawerState.video!.history_id);
        if (r?.video) detail.value = r.video;
      } catch { /* ignore */ }
      downloadingCover.value = false;
    }, 3000);
  } catch (e: any) {
    toast.error(e?.message || '下载封面失败');
    downloadingCover.value = false;
  }
}

async function loadUrls() {
  if (!drawerState.video) return;
  try {
    urls.value = await videoApi.getVideoUrls(drawerState.video.bvid, cid.value ?? undefined, selectedQn.value);
  } catch (e: any) { toast.error(e?.message || '加载视频链接失败'); }
}

async function loadComments() {
  if (!drawerState.video) return;
  try { comments.value = await videoApi.comments(drawerState.video.bvid); } catch {}
}
async function loadDanmaku() {
  if (!drawerState.video) return;
  try { danmaku.value = await videoApi.danmaku(drawerState.video.bvid); } catch {}
}
async function loadLogs() {
  if (!drawerState.video) return;
  try {
    const r: any = await logsApi.bvid(drawerState.video.bvid);
    const arr = Array.isArray(r?.logs) ? r.logs : (Array.isArray(r) ? r : []);
    logs.value = arr.map((l: any) => {
      if (typeof l === 'string') return l;
      const t = l.ts ? new Date(l.ts * 1000).toLocaleTimeString() : '';
      return `[${(l.level || 'info').toUpperCase()}] ${t} ${l.message ?? ''}`.trim();
    });
  } catch {}
}

watch(drawerTab, async (t) => {
  if (t === 'download' && !urls.value) await loadUrls();
  if (t === 'logs' && logs.value.length === 0) await loadLogs();
});

async function selectPage(idx: number) {
  selectedPage.value = idx;
  if (videoInfo.value?.pages?.[idx]) cid.value = videoInfo.value.pages[idx].cid;
  urls.value = null;
  if (drawerTab.value === 'download') await loadUrls();
}

async function startDownload() {
  if (!drawerState.video) return;
  try {
    await download.addTask(drawerState.video.bvid, {
      uid: videoInfo.value?.mid ? Number(videoInfo.value.mid) : undefined,
      title: videoInfo.value?.title || drawerState.video.title,
    });
    toast.success('已加入下载队列');
    closeDrawer();
  } catch (e: any) { toast.error(e?.message || '加入下载失败'); }
}

async function downloadDanmaku() {
  if (!drawerState.video) return;
  try {
    await download.addTask(drawerState.video.bvid, { mode: 'danmaku', title: videoInfo.value?.title });
    toast.success('已加入弹幕下载队列');
  } catch (e: any) { toast.error(e?.message || '失败'); }
}

async function downloadComments() {
  if (!drawerState.video) return;
  try {
    await download.addTask(drawerState.video.bvid, { mode: 'comments', title: videoInfo.value?.title });
    toast.success('已加入评论下载队列');
  } catch (e: any) { toast.error(e?.message || '失败'); }
}

function openBilibili() {
  if (!drawerState.video) return;
  window.open(`https://www.bilibili.com/video/${drawerState.video.bvid}`, '_blank');
}

async function pauseTask() {
  if (!taskId.value) { toast.warn('当前没有可暂停的下载任务'); return; }
  try { await download.pauseTask(taskId.value); toast.success('已暂停'); }
  catch (e: any) { toast.error(e?.message || '暂停失败'); }
}
async function resumeTask() {
  if (!taskId.value) { toast.warn('当前没有可恢复的下载任务'); return; }
  try { await download.resumeTask(taskId.value); toast.success('已恢复'); }
  catch (e: any) { toast.error(e?.message || '恢复失败'); }
}
async function retryTask() {
  if (!taskId.value) { toast.warn('当前没有可重试的下载任务'); return; }
  try { await download.retryTask(taskId.value); toast.success('已重新加入队列'); }
  catch (e: any) { toast.error(e?.message || '重试失败'); }
}
async function removeTask() {
  if (!taskId.value) { toast.warn('当前没有下载任务可移除'); return; }
  try { await download.removeTask(taskId.value); toast.success('已移除下载任务'); }
  catch (e: any) { toast.error(e?.message || '移除失败'); }
}
async function deleteHistoryRecord() {
  if (!historyId.value) { toast.warn('没有可删除的下载记录'); return; }
  if (!window.confirm('确认删除该下载记录？本地文件不会删除。')) return;
  try {
    await historyApi.delete(historyId.value);
    toast.success('已删除下载记录');
    closeDrawer();
  } catch (e: any) { toast.error(e?.message || '删除失败'); }
}
async function openDirectory() {
  if (!historyId.value) { toast.warn('当前没有下载记录，无法打开目录'); return; }
  try { await historyApi.openDirectory(historyId.value); toast.success('已请求打开目录'); }
  catch (e: any) { toast.error(e?.message || '打开目录失败'); }
}
function copyPath() {
  if (!filePath.value) { toast.warn('当前没有可复制的路径'); return; }
  void copyToClipboard(filePath.value);
}

async function burnMedia(source: 'danmaku' | 'subtitle') {
  if (!drawerState.video) return;
  if (burningTask.value) { toast.warn(`已在烧录：${burningTask.value === 'danmaku' ? '弹幕' : '字幕'}`); return; }
  if (!drawerState.video.history_id) { toast.warn('未找到 history_id，无法定位烧录产物'); return; }
  burningTask.value = source;
  try {
    await download.burn(drawerState.video.bvid, source, drawerState.video.history_id);
    toast.success(source === 'danmaku' ? '已开始烧录弹幕' : '已开始烧录字幕');
    // 简单兜底：3 秒后允许重试 + 清空标记（不轮询；旧版 drawer 用 pollBurnStatus 跟踪进度，
    // 本任务范围内先做"启动"按钮，轮询可在 P2-7 或后续轮次接入）
    window.setTimeout(() => { if (burningTask.value === source) burningTask.value = null; }, 3000);
  } catch (e: any) {
    toast.error(e?.message || '烧录启动失败');
    burningTask.value = null;
  }
}

/** 浏览器下载产物文件：通过 a[download] 触发。 */
function browserDownloadFile(file: any) {
  if (!drawerState.video || !file?.path) return;
  const url = historyApi.fileDownloadUrl(drawerState.video.bvid, file.path);
  const a = document.createElement('a');
  a.href = url;
  a.download = file.name || 'download';
  a.style.display = 'none';
  document.body.appendChild(a);
  a.click();
  setTimeout(() => a.remove(), 100);
}

function formatSize(b?: number) {
  if (!b) return '';
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(1)} MB`;
  return `${(b / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatDuration(s?: number) {
  if (!s) return '';
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${String(r).padStart(2, '0')}`;
}
</script>

<template>
  <template v-if="drawerState.visible">
    <div class="drawer-overlay" id="drawer-overlay" data-action="close-video-drawer" @click="closeDrawer"></div>
    <div class="video-drawer" id="video-drawer">
      <div class="drawer-header">
        <div class="drawer-title" id="drawer-video-title" :title="drawerTitle">{{ drawerTitle }}</div>
        <button class="drawer-close-btn" data-action="close-video-drawer" aria-label="关闭" @click="closeDrawer">
          <i class="fa-solid fa-times"></i>
        </button>
      </div>

      <div class="drawer-body" id="drawer-body">
        <div class="drawer-tabs">
          <button :class="['drawer-tab', { active: drawerTab === 'info' }]" @click="drawerTab = 'info'">信息</button>
          <button :class="['drawer-tab', { active: drawerTab === 'download' }]" @click="drawerTab = 'download'">下载</button>
          <button :class="['drawer-tab', { active: drawerTab === 'files' }]" @click="drawerTab = 'files'">文件</button>
          <button :class="['drawer-tab', { active: drawerTab === 'logs' }]" @click="drawerTab = 'logs'">日志</button>
        </div>

        <!-- 信息 Tab -->
        <div v-if="drawerTab === 'info'">
          <img v-if="videoInfo?.pic" :src="videoInfo.pic" class="drawer-cover" alt="cover" />
          <div v-if="videoInfo" class="drawer-info-block">
            <div class="drawer-info-row">
              <span class="drawer-info-label">UP</span>
              <span class="drawer-info-value">{{ videoInfo.up_name || videoInfo.mid || '—' }}</span>
            </div>
            <div class="drawer-info-row">
              <span class="drawer-info-label">类型</span>
              <span class="drawer-info-value">{{ videoInfo.tname || '—' }}</span>
            </div>
            <div class="drawer-info-row">
              <span class="drawer-info-label">时长</span>
              <span class="drawer-info-value">{{ formatDuration(videoInfo.duration) }}</span>
            </div>
            <div class="drawer-section-title">简介</div>
            <div class="drawer-info-desc">{{ videoInfo.desc || '—' }}</div>
          </div>
          <div class="btn-row" style="margin-top: 12px;">
            <button class="btn btn-sm" @click="loadComments">
              <i class="fa-solid fa-comment"></i> 评论 {{ comments?.count ?? '—' }}
            </button>
            <button class="btn btn-sm" @click="loadDanmaku">
              <i class="fa-solid fa-comment-dots"></i> 弹幕 {{ danmaku?.count ?? '—' }}
            </button>
            <button class="btn btn-sm" @click="openBilibili">
              <i class="fa-solid fa-external-link-alt"></i> 打开 B 站
            </button>
          </div>
        </div>

        <!-- 下载 Tab -->
        <div v-else-if="drawerTab === 'download'">
          <div v-if="pages.length > 1" class="drawer-section">
            <div class="drawer-section-title">分P</div>
            <div class="drawer-pills">
              <button v-for="(p, i) in pages" :key="p.cid"
                      :class="['quality-pill', { active: selectedPage === i }]"
                      @click="selectPage(i)">P{{ i + 1 }}: {{ p.part }}</button>
            </div>
          </div>
          <div v-if="acceptQuality.length" class="drawer-section">
            <div class="drawer-section-title">清晰度</div>
            <div class="drawer-pills">
              <button v-for="q in acceptQuality" :key="q.qn"
                      :class="['quality-pill', { active: selectedQn === q.qn }]"
                      @click="selectedQn = q.qn">{{ q.name }}</button>
            </div>
          </div>
          <div class="btn-row" style="margin-top: 12px;">
            <button class="btn btn-primary" @click="startDownload"><i class="fa-solid fa-download"></i> 视频+音频</button>
            <button class="btn" @click="downloadDanmaku"><i class="fa-solid fa-comment"></i> 弹幕</button>
            <button class="btn" @click="downloadComments"><i class="fa-solid fa-comment-dots"></i> 评论</button>
          </div>

          <!-- 任务控制：仅在已有 task 时显示 -->
          <div v-if="taskId" class="btn-row" style="margin-top: 8px;">
            <button v-if="taskStatus === 'downloading'" class="btn btn-sm" @click="pauseTask">
              <i class="fa-solid fa-pause"></i> 暂停下载
            </button>
            <button v-else-if="taskStatus === 'paused'" class="btn btn-sm btn-primary" @click="resumeTask">
              <i class="fa-solid fa-play"></i> 恢复下载
            </button>
            <button v-if="isFailed" class="btn btn-sm btn-primary" @click="retryTask">
              <i class="fa-solid fa-redo"></i> 重新下载
            </button>
            <button class="btn btn-sm btn-danger" @click="removeTask">
              <i class="fa-solid fa-trash"></i> 移除任务
            </button>
            <span v-if="failureMessage" class="form-note" style="align-self: center; color: var(--tone-error, #c0392b);">
              失败：{{ failureMessage }}
            </span>
          </div>
          <div v-if="urls?.dash?.video" class="drawer-info-block" style="margin-top: 12px;">
            <div v-for="v in urls.dash.video" :key="v.id" class="drawer-info-row">
              <span class="drawer-info-label">视频 #{{ v.id }}</span>
              <span class="drawer-info-value">{{ v.bandwidth }} bps · {{ v.codecs }}</span>
            </div>
            <div v-for="a in urls.dash.audio || []" :key="a.id" class="drawer-info-row">
              <span class="drawer-info-label">音频 #{{ a.id }}</span>
              <span class="drawer-info-value">{{ a.bandwidth }} bps · {{ a.codecs }}</span>
            </div>
          </div>
        </div>

        <!-- 文件 Tab：产物 + sidecar 状态 -->
        <div v-else-if="drawerTab === 'files'">
          <div class="btn-row" style="margin-bottom: 12px;">
            <button v-if="canOpenDirectory" class="btn btn-sm" @click="openDirectory">
              <i class="fa-solid fa-folder-open"></i> 打开文件目录
            </button>
            <button v-if="filePath" class="btn btn-sm" @click="copyPath">
              <i class="fa-solid fa-copy"></i> 复制路径
            </button>
            <button v-if="historyId && !burnedDanmaku" class="btn btn-sm btn-primary"
                    :disabled="!ffmpegOk || burningTask !== null"
                    :title="ffmpegOk ? '将弹幕烧录进视频' : ffmpegHint"
                    @click="burnMedia('danmaku')">
              <i class="fa-solid fa-fire"></i>
              {{ burningTask === 'danmaku' ? '弹幕烧录中…' : '烧录弹幕' }}
            </button>
            <button v-if="historyId && !burnedSubtitle" class="btn btn-sm btn-primary"
                    :disabled="!ffmpegOk || burningTask !== null"
                    :title="ffmpegOk ? '将 CC 字幕烧录进视频' : ffmpegHint"
                    @click="burnMedia('subtitle')">
              <i class="fa-solid fa-closed-captioning"></i>
              {{ burningTask === 'subtitle' ? '字幕烧录中…' : '烧录字幕' }}
            </button>
            <button v-if="historyId" class="btn btn-sm btn-danger" @click="deleteHistoryRecord">
              <i class="fa-solid fa-trash"></i> 删除下载记录
            </button>
            <span v-if="filePath" class="form-note" style="align-self: center; word-break: break-all;">
              {{ filePath }}
            </span>
          </div>
          <div v-if="!ffmpegOk" class="form-note" style="color: var(--tone-error, #c0392b); margin-bottom: 8px;">
            <i class="fa-solid fa-triangle-exclamation"></i> {{ ffmpegHint }}
          </div>
          <div v-if="sidecar" class="drawer-section">
            <div class="drawer-section-title">附件状态</div>
            <div class="drawer-info-block">
              <div class="drawer-info-row">
                <span class="drawer-info-label">弹幕</span>
                <span class="drawer-info-value">{{ sidecar.danmaku ? '已下载' : '未下载' }}</span>
              </div>
              <div class="drawer-info-row">
                <span class="drawer-info-label">评论</span>
                <span class="drawer-info-value">{{ sidecar.comments ? '已下载' : '未下载' }}</span>
              </div>
              <div class="drawer-info-row">
                <span class="drawer-info-label">封面</span>
                <span class="drawer-info-value">{{ coverExists ? '已下载' : '未下载' }}</span>
              </div>
              <div class="drawer-info-row">
                <span class="drawer-info-label">视频</span>
                <span class="drawer-info-value">{{ sidecar.video ? '已下载' : '未下载' }}</span>
              </div>
              <div class="drawer-info-row">
                <span class="drawer-info-label">字幕</span>
                <span class="drawer-info-value">{{ sidecar.subtitle ? '已下载' : '未下载' }}</span>
              </div>
            </div>

            <!-- 弹幕归档版本（json / txt / xml），按 file_type='danmaku' 的 files 派生 -->
            <div v-if="danmakuVersions.length > 1" class="drawer-section-title" style="margin-top: 12px;">弹幕归档</div>
            <div v-if="danmakuVersions.length > 1" class="btn-row" style="flex-wrap: wrap;">
              <button v-for="v in danmakuVersions" :key="`${v.format}-${v.path}`"
                      :class="['btn', 'btn-sm', { 'btn-primary': selectedDanmakuFormat === v.format }]"
                      :title="v.path"
                      @click="selectDanmakuFormat(v.format)">
                <i class="fa-solid" :class="v.is_current ? 'fa-circle-check' : 'fa-clock-rotate-left'"></i>
                {{ v.format.toUpperCase() }}{{ v.is_current ? ' · 当前' : '' }}
              </button>
            </div>
            <div v-if="selectedDanmakuPath" class="form-note" style="word-break: break-all;">
              当前选中：{{ selectedDanmakuFormat.toUpperCase() }} · {{ selectedDanmakuPath }}
            </div>

            <!-- 下载封面：未下载时显示入口 -->
            <div v-if="!coverExists" class="btn-row" style="margin-top: 12px;">
              <button class="btn btn-sm" :disabled="downloadingCover" @click="downloadCoverNow">
                <i class="fa-solid fa-image"></i>
                {{ downloadingCover ? '封面下载中…' : '下载封面' }}
              </button>
            </div>
          </div>

          <div v-if="files.length" class="drawer-section">
            <div class="drawer-section-title">产物文件 ({{ files.length }})</div>
            <table class="table" style="font-size: 12px;">
              <thead>
                <tr><th>类型</th><th>名称</th><th>大小</th><th v-if="canBrowserDownload">操作</th></tr>
              </thead>
              <tbody>
                <tr v-for="(f, i) in files" :key="i">
                  <td><code style="font-size: 11px;">{{ f.file_type || f.location || '—' }}</code></td>
                  <td><code style="font-size: 11px;">{{ f.name }}</code></td>
                  <td>{{ formatSize(f.size) }}</td>
                  <td v-if="canBrowserDownload">
                    <button class="btn btn-sm" @click="browserDownloadFile(f)" :disabled="!f.path">
                      <i class="fa-solid fa-download"></i> 下载
                    </button>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
          <div v-else-if="!sidecar" class="empty-state">
            <i class="fa-solid fa-folder-open"></i>
            <p>暂无历史记录</p>
            <p class="empty-hint">下载完成后会在此展示产物文件与弹幕/评论</p>
          </div>
        </div>

        <!-- 日志 Tab -->
        <div v-else-if="drawerTab === 'logs'">
          <div class="drawer-logs">
            <div v-if="logs.length === 0" class="drawer-logs-hint">暂无日志</div>
            <div v-for="(line, i) in logs" :key="i" class="drawer-log-line">
              <span class="drawer-log-time">{{ (i + 1).toString().padStart(3, '0') }}</span>
              <span class="drawer-log-msg">{{ line }}</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  </template>
</template>
