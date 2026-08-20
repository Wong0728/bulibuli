<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue';
import { useLiveStore } from '@/stores/live';
import { useAppStore } from '@/stores/app';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';

const live = useLiveStore();
const app = useAppStore();
const toast = useToastStore();

// 添加房间
const showAdd = ref(false);
const addRoomInput = ref('');
const addConfig = ref({ auto_record: true, quality: 10000, segment_seconds: 600, max_segments: 30 });
const adding = ref(false);

// 录制任务子 tab
type LiveBoard = 'recording' | 'history' | 'attention';
const subTab = ref<LiveBoard>('recording');

// 选中直播间实时信息：选中新 source 时拉一次 / 切换时强刷
const localRoomInfo = ref<any | null>(null);
const localRoomInfoLoading = ref(false);
let roomInfoTimer: number | null = null;

onMounted(async () => {
  await live.refreshDashboard();
  await live.refreshHistory();
  // 启动一次轮询刷新（dashboard sessions 是实时录制进度；每 5s 拉一次）
  roomInfoTimer = window.setInterval(() => {
    // 静默：失败已经在 store 内部吞掉
    live.refreshDashboard();
  }, 5000);
});
onUnmounted(() => {
  if (roomInfoTimer) { clearInterval(roomInfoTimer); roomInfoTimer = null; }
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
});

const recordingList = computed(() => live.recordings);

// 选中 source 变化时拉实时信息（5 分钟内复用缓存，强制刷新按钮走 force）
watch(
  () => live.selectedSource?.room_id,
  async (roomId) => {
    if (!roomId) { localRoomInfo.value = null; return; }
    localRoomInfoLoading.value = true;
    try {
      const r = await live.roomInfo(roomId);
      localRoomInfo.value = r;
    } finally {
      localRoomInfoLoading.value = false;
    }
  },
  { immediate: true },
);

async function refreshRoomInfo() {
  if (!live.selectedSource) return;
  localRoomInfoLoading.value = true;
  try {
    const r = await live.roomInfo(live.selectedSource.room_id, { force: true });
    localRoomInfo.value = r;
  } finally {
    localRoomInfoLoading.value = false;
  }
}

// B 站同步状态：从 dashboard 的 lastCheckAt / monitor 派生
const biliSyncLabel = computed(() => {
  if (!live.lastCheckAt) return 'B站状态：等待检查';
  const t = new Date(live.lastCheckAt);
  if (isNaN(t.getTime())) return 'B站状态：未知';
  return `B站状态：${t.toLocaleTimeString()}`;
});
const biliSyncTitle = computed(() => {
  if (!live.lastCheckAt) return '尚未拉取过直播 dashboard';
  return `最近一次同步：${new Date(live.lastCheckAt).toLocaleString()}`;
});
const biliSyncState = computed(() => {
  if (!live.lastCheckAt) return 'warn';
  // 超过 2 分钟没同步算异常
  const diff = Date.now() - new Date(live.lastCheckAt).getTime();
  if (diff < 0 || diff > 2 * 60 * 1000) return 'error';
  return 'ok';
});

// "需要处理"面板的计数：合并中 / 失败 / 等待续录 等
const attentionCount = computed(() => {
  let n = 0;
  for (const r of live.recordings) {
    if (['merging', 'failed', 'merge_failed'].includes(r.status as string)) n++;
  }
  // 加上有"等待手动合并"历史的：history 里 status=stopped 的也算
  for (const h of live.history) {
    if (h.status === 'failed') n++;
  }
  return n;
});

function selectSource(id: number | null) {
  live.selectSource(id);
}

async function openAddModal() {
  showAdd.value = true;
  addRoomInput.value = '';
}

async function confirmAdd() {
  const text = addRoomInput.value.trim();
  if (!text) { toast.warn('请输入房间号或链接'); return; }
  // 批量解析：用换行 / 空格 / 逗号 / 分号 / 短链 拆分
  const tokens = text.split(/[\s,;]+/).map(s => s.trim()).filter(Boolean);
  const ids: number[] = [];
  for (const tok of tokens) {
    // 提取 "live.bilibili.com/123" 或纯数字
    const m = tok.match(/(\d{2,12})/);
    if (!m) { toast.warn(`无法解析房间号: ${tok}`); return; }
    const id = Number(m[1]);
    if (id > 0) ids.push(id);
  }
  if (ids.length === 0) { toast.warn('请输入有效的房间号'); return; }
  adding.value = true;
  let ok = 0;
  try {
    for (const id of ids) {
      const r = await live.addSource(id, addConfig.value);
      if (r) ok++;
    }
    toast.success(`已添加 ${ok}/${ids.length}`);
    if (ok > 0) showAdd.value = false;
  } catch (e: any) {
    toast.error(e?.message || '添加失败');
  } finally {
    adding.value = false;
  }
}

async function startRecord(sourceId: number) {
  try {
    await live.startRecording(sourceId);
    toast.success('已开始录制');
    await refreshRoomInfo();
  } catch (e: any) { toast.error(e?.message || '启动录制失败'); }
}
async function stopRecord(recId: string) {
  if (!await confirmDialog({ title: '停止录制', message: '确认停止该录制任务？', tone: 'danger' })) return;
  try {
    await live.stopRecording(recId);
    toast.success('已停止');
  } catch (e: any) { toast.error(e?.message || '失败'); }
}

// 合并任务本地 Map 仍保留（live.mergeJob 轮询用），并额外消费全局 mergeJobs
const mergeJobs = ref<Map<string, { status: string; progress?: number; error?: string; output_path?: string }>>(new Map());
let pollTimer: number | null = null;

async function startMergeJob(recId: string) {
  try {
    const r: any = await live.startMerge(recId);
    if (!r || !r.job_id) { toast.error('后端未返回 job_id'); return; }
    mergeJobs.value.set(r.job_id, { status: 'pending' });
    mergeJobs.value = new Map(mergeJobs.value);
    pollMergeJob(r.job_id);
    toast.success('合并任务已创建');
  } catch (e: any) { toast.error(e?.message || '创建合并失败'); }
}

function pollMergeJob(jobId: string) {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = window.setInterval(async () => {
    try {
      const j: any = await live.mergeJob(jobId);
      if (!j) return;
      mergeJobs.value.set(jobId, j);
      mergeJobs.value = new Map(mergeJobs.value);
      if (j.status === 'completed' || j.status === 'failed' || j.status === 'cancelled') {
        if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
        // 终态后异步刷一次 dashboard，把 mergeJobs 全局状态回填
        live.refreshDashboard();
      }
    } catch { /* ignore */ }
  }, 1500);
}

async function cancelMergeJob(jobId: string) {
  try { await live.cancelMerge(jobId); } catch (e: any) { toast.error(e?.message || '取消失败'); }
}

async function removeSource(id: number) {
  if (!await confirmDialog({ title: '移除房间', message: '确认移除该直播间？历史录制不会被删除。', tone: 'danger' })) return;
  try {
    await live.deleteSource(id);
    toast.success('已移除');
  } catch (e: any) { toast.error(e?.message || '失败'); }
}

// ===== 工具函数 =====
function formatDuration(secs?: number): string {
  if (!secs || !Number.isFinite(secs) || secs <= 0) return '0:00';
  const total = Math.floor(secs);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
  return `${m}:${String(s).padStart(2, '0')}`;
}

function formatFileSize(bytes?: number): string {
  if (!bytes || !Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(v < 10 && i > 0 ? 2 : 0)} ${units[i]}`;
}

function qualityLabel(qn?: number): string {
  switch (qn) {
    case 10000: return '原画';
    case 400: return '蓝光';
    case 250: return '超清';
    case 150: return '高清';
    case 80: return '流畅';
    default: return qn != null ? String(qn) : '—';
  }
}

function streamStatusLabel(r: any): string {
  if (r.status === 'recording') return '录制中';
  if (r.status === 'merging') return '合并中';
  if (r.status === 'stopped') return '已停止';
  if (r.status === 'merged') return '已合并';
  if (r.status === 'failed') return '失败';
  if (r.status === 'merge_failed') return '合并失败';
  return r.status || '—';
}

function streamStatusClass(r: any): string {
  if (r.status === 'recording') return 'live';
  if (r.status === 'merging') return 'starting';
  if (r.status === 'failed' || r.status === 'merge_failed') return 'warn';
  return 'completed';
}
</script>

<template>
  <section class="tab-panel">
    <!-- 直播录制看板：布局对齐博主监控看板（侧边栏 + 详情） -->
    <div class="card">
      <div class="card-title">
        <span><i class="fa-solid fa-tower-broadcast"></i> 直播录制看板</span>
        <div class="live-sync-group" id="live-sync-group" aria-live="polite">
          <span class="live-sync-item" id="live-sync-page" data-state="ok" title="本地页面与服务的连接状态">
            <span class="aria2-dot connected"></span>页面已连接
          </span>
          <span class="live-sync-item" id="live-sync-monitor" data-state="ok" :data-state-fallback="live.monitorRunning ? 'ok' : 'error'" title="后端开播监控 worker 的运行状态">
            <span class="aria2-dot" :class="live.monitorRunning ? 'connected' : 'disconnected'"></span>
            {{ live.monitorRunning ? '监控运行中' : '监控未运行' }}
          </span>
          <span class="live-sync-item" id="live-sync-bili" :data-state="biliSyncState" :title="biliSyncTitle">
            <span class="aria2-dot" :class="biliSyncState === 'ok' ? 'connected' : (biliSyncState === 'error' ? 'disconnected' : 'warn')"></span>
            {{ biliSyncLabel }}
          </span>
          <button class="btn btn-sm btn-ghost" id="live-refresh-btn" data-network-required="true" title="手动刷新直播状态" @click="live.refreshDashboard()">
            <i class="fa-solid fa-arrows-rotate"></i> 刷新
          </button>
        </div>
      </div>
      <!-- 风险提示条：与原版 live-risk-notice 同构，运行时由后端信号动态展示。 -->
      <div v-if="app.riskNotice" id="live-risk-notice" class="live-alert warn" role="alert">
        <i class="fa-solid fa-triangle-exclamation"></i> {{ app.riskNotice }}
        <button class="live-alert-close" @click="app.dismissRiskNotice()">×</button>
      </div>
      <div class="live-dashboard">
        <aside class="live-sidebar">
          <div class="live-sidebar-title">
            <span><i class="fa-solid fa-list"></i> 关注房间（<span id="live-source-count">{{ live.sources.length }}</span>）</span>
            <button class="btn btn-sm btn-ghost" id="live-show-add-btn" data-network-required="true" @click="openAddModal">
              <i class="fa-solid fa-plus"></i> 添加
            </button>
          </div>
          <div id="live-room-list" aria-live="polite">
            <div v-for="s in live.sources" :key="s.id"
                 :class="['live-room-item', { active: live.selectedSourceId === s.id }]"
                 @click="selectSource(s.id)">
              <span :class="['live-room-dot', s.live_status === 1 ? 'live' : 'warn']"></span>
              <div class="live-room-info">
                <div class="live-room-name">{{ s.uname || `房间 ${s.room_id}` }}</div>
                <div class="live-room-meta">
                  <span>房间 {{ s.room_id }}</span>
                  <span v-if="s.live_status === 1" class="live-room-pill live"><i class="fa-solid fa-circle"></i> 直播中</span>
                  <span v-else-if="s.live_status === 2" class="live-room-pill warn"><i class="fa-solid fa-circle"></i> 轮播中</span>
                  <span v-else class="live-room-pill">未开播</span>
                </div>
              </div>
            </div>
          </div>
          <div class="live-sidebar-actions">
            <button class="btn btn-primary btn-block" id="live-refresh-list-btn" data-network-required="true" @click="live.refreshDashboard()">
              <i class="fa-solid fa-rotate"></i> 刷新列表
            </button>
          </div>
        </aside>

        <div class="live-detail-panel" id="live-detail-panel">
          <div v-if="!live.selectedSource" class="live-empty-state" id="live-empty-state">
            <i class="fa-solid fa-tower-broadcast"></i>
            <p>暂无直播源</p>
            <p class="empty-hint">点击上方"添加"输入房间号或直播链接，查询后即可关注</p>
          </div>
          <div v-else id="live-detail-content">
            <div class="live-detail-header">
              <img v-if="(live.selectedSource as any).cover" :src="(live.selectedSource as any).cover" class="live-cover-thumb" />
              <div class="live-detail-main">
                <div class="live-detail-title-row">
                  <span :class="['live-badge', live.selectedSource.live_status === 1 ? 'live' : 'warn']">
                    {{ live.selectedSource.live_status === 1 ? '直播中' : (live.selectedSource.live_status === 2 ? '轮播中' : '未开播') }}
                  </span>
                  <span class="live-detail-title">{{ live.selectedSource.uname || `房间 ${live.selectedSource.room_id}` }}</span>
                </div>
                <div class="live-detail-meta">
                  <span>房间 {{ live.selectedSource.room_id }}</span>
                  <span v-if="live.selectedSource.runtime?.online">· 在线 {{ live.selectedSource.runtime.online.toLocaleString() }}</span>
                </div>
                <div class="live-strategy-summary">
                  <i class="fa-solid fa-sliders-h"></i>
                  {{ live.selectedSource.auto_record ? '已开启自动录制' : '未开启自动录制' }}
                </div>
                <div class="live-detail-actions">
                  <button class="btn btn-primary" :disabled="localRoomInfo?.is_recording" @click="startRecord(live.selectedSource.id)">
                    <i class="fa-solid fa-record-vinyl"></i> {{ localRoomInfo?.is_recording ? '录制中' : '开始录制' }}
                  </button>
                  <button class="btn btn-danger" @click="removeSource(live.selectedSource.id)">
                    <i class="fa-solid fa-trash"></i> 移除
                  </button>
                </div>
              </div>
            </div>

            <!-- 实时直播信息卡片：标题/分区/开播时间/清晰度 -->
            <div v-if="localRoomInfo" class="live-room-info-card">
              <div class="live-section-title">
                <span><i class="fa-solid fa-circle-info"></i> 实时直播信息</span>
                <button class="btn btn-sm btn-ghost" :disabled="localRoomInfoLoading" @click="refreshRoomInfo">
                  <i class="fa-solid fa-arrows-rotate" :class="{ 'fa-spin': localRoomInfoLoading }"></i> 刷新
                </button>
              </div>
              <div class="live-info-grid">
                <div class="live-info-item">
                  <span class="live-info-label">标题</span>
                  <span class="live-info-value" :title="localRoomInfo.title || '—'">{{ localRoomInfo.title || '—' }}</span>
                </div>
                <div class="live-info-item">
                  <span class="live-info-label">分区</span>
                  <span class="live-info-value">
                    {{ localRoomInfo.parent_area_name || '' }}{{ localRoomInfo.area_name ? ' / ' + localRoomInfo.area_name : '' }}
                  </span>
                </div>
                <div class="live-info-item">
                  <span class="live-info-label">开播时间</span>
                  <span class="live-info-value">
                    {{ localRoomInfo.live_time ? new Date(localRoomInfo.live_time).toLocaleString() : '—' }}
                  </span>
                </div>
                <div class="live-info-item">
                  <span class="live-info-label">清晰度</span>
                  <span class="live-info-value">{{ qualityLabel(live.selectedSource.quality) }}</span>
                </div>
                <div class="live-info-item" v-if="localRoomInfo.tags">
                  <span class="live-info-label">标签</span>
                  <span class="live-info-value live-info-tags">
                    <span v-for="t in String(localRoomInfo.tags).split(',').filter(Boolean)" :key="t" class="live-info-tag">{{ t }}</span>
                  </span>
                </div>
                <div class="live-info-item">
                  <span class="live-info-label">录制状态</span>
                  <span class="live-info-value">
                    <span v-if="localRoomInfo.is_recording" class="live-badge recording">
                      <i class="fa-solid fa-record-vinyl"></i> 录制中（{{ localRoomInfo.recording_status || '' }}）
                    </span>
                    <span v-else-if="localRoomInfo.can_start" class="live-badge live">可开始录制</span>
                    <span v-else class="live-badge">未在录制</span>
                  </span>
                </div>
              </div>
              <!-- 与当前录制 session 关联的实时进度（来自 dashboard sessions） -->
              <div v-if="live.selectedSource && recordingList.find(r => r.room_id === live.selectedSource!.room_id)" class="live-room-info-progress">
                <div class="live-section-subtitle">录制实时进度</div>
                <div v-for="r in recordingList.filter(r => r.room_id === live.selectedSource!.room_id)" :key="r.recording_id" class="live-room-info-progress-row">
                  <div class="live-progress-line">
                    <span class="live-progress-label">已录制</span>
                    <span class="live-progress-value">{{ formatDuration(r.duration_secs) }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">文件大小</span>
                    <span class="live-progress-value">{{ formatFileSize(r.file_size) }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">分段</span>
                    <span class="live-progress-value">{{ r.segment_count ?? 0 }}</span>
                  </div>
                  <div class="live-progress-line">
                    <span class="live-progress-label">弹幕</span>
                    <span class="live-progress-value">{{ (r.danmaku_count ?? 0).toLocaleString() }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">独立用户</span>
                    <span class="live-progress-value">{{ (r.unique_user_count ?? 0).toLocaleString() }}</span>
                    <span v-if="(r.peak_watched ?? 0) > 0" class="live-progress-sep">·</span>
                    <span v-if="(r.peak_watched ?? 0) > 0" class="live-progress-label">峰值观看</span>
                    <span v-if="(r.peak_watched ?? 0) > 0" class="live-progress-value">{{ (r.peak_watched ?? 0).toLocaleString() }}</span>
                  </div>
                  <div v-if="(r.sc_count ?? 0) > 0 || (r.guard_count ?? 0) > 0" class="live-progress-line">
                    <span class="live-progress-label">SC</span>
                    <span class="live-progress-value">{{ r.sc_count ?? 0 }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">上船</span>
                    <span class="live-progress-value">{{ r.guard_count ?? 0 }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">礼物价值</span>
                    <span class="live-progress-value">¥{{ (r.estimated_paid_value ?? 0).toFixed(2) }}</span>
                  </div>
                  <div v-if="r.error_msg" class="live-progress-error">
                    <i class="fa-solid fa-triangle-exclamation"></i> {{ r.error_msg }}
                  </div>
                </div>
              </div>
            </div>

            <div class="live-section-title">直播源设置</div>
            <div class="live-settings-form">
              <div class="form-group form-full">
                <label class="choice-row">
                  <span>自动录制（检测到开播时自动开始）</span>
                  <span class="toggle-switch">
                    <input type="checkbox" :checked="live.selectedSource.auto_record" @change="live.updateSource(live.selectedSource.id, { auto_record: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </span>
                </label>
              </div>
              <div class="form-group">
                <label for="live-source-quality"><i class="fa-solid fa-film"></i> 清晰度上限</label>
                <select id="live-source-quality" class="form-control" :value="live.selectedSource.quality" @change="live.updateSource(live.selectedSource.id, { quality: Number(($event.target as HTMLSelectElement).value) })">
                  <option value="10000">原画（10000，推荐）</option>
                  <option value="400">蓝光（400）</option>
                  <option value="250">超清（250）</option>
                  <option value="150">高清（150）</option>
                  <option value="80">流畅（80）</option>
                </select>
              </div>
              <div class="form-group">
                <label for="live-source-segments"><i class="fa-solid fa-clock"></i> 分段时长（秒）</label>
                <input id="live-source-segments" type="number" class="form-control" :value="live.selectedSource.segment_seconds"
                       @change="live.updateSource(live.selectedSource.id, { segment_seconds: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">单个 FLV 段的目标时长；到点后会自动切片并续录。</div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 录制任务看板：布局对齐下载管理（顶栏 + 子 tab） -->
    <div class="card">
      <div class="board-top-bar">
        <div class="board-top-bar-left">
          <i class="fa-solid fa-record-vinyl"></i>
          <span>录制任务</span>
          <span id="live-disk-hint" class="last-pull-time" title="录制目录所在磁盘余量">{{ live.diskFree ? '磁盘余量 ' + live.diskFree : '' }}</span>
        </div>
      </div>
      <div class="board-sub-tabs" id="live-board-tabs">
        <div :class="['board-sub-tab', { active: subTab === 'recording' }]" data-live-board="recording" data-action="switch-live-board" @click="subTab = 'recording'">
          录制中 <span class="board-tab-count" id="live-count-recording">{{ recordingList.length }}</span>
        </div>
        <div :class="['board-sub-tab', { active: subTab === 'history' }]" data-live-board="history" data-action="switch-live-board" @click="subTab = 'history'; live.refreshHistory()">
          最近录制 <span class="board-tab-count" id="live-count-history">{{ live.history.length }}</span>
        </div>
        <div :class="['board-sub-tab', { active: subTab === 'attention' }]" data-live-board="attention" data-action="switch-live-board" @click="subTab = 'attention'">
          需要处理 <span class="board-tab-count" id="live-count-attention">{{ attentionCount }}</span>
        </div>
      </div>
      <div :class="['live-board-panel', { active: subTab === 'recording' }]" id="live-panel-recording">
        <div v-if="recordingList.length === 0" id="live-recording-list">
          <p class="empty-hint">暂无录制中的任务</p>
        </div>
        <div v-else id="live-recording-list">
          <div v-for="r in recordingList" :key="r.recording_id" class="live-recording-row live-recording-row-detailed">
            <div class="live-recording-main">
              <div class="live-recording-title">
                <span class="live-room-name">{{ r.uname || r.room_id }}</span>
                <span :class="['live-badge', streamStatusClass(r)]">
                  {{ streamStatusLabel(r) }}
                </span>
                <span v-if="r.trigger" class="live-recording-trigger" :title="'触发方式：' + r.trigger">
                  <i class="fa-solid fa-bolt"></i> {{ r.trigger }}
                </span>
                <span v-if="r.capture_mode" class="live-recording-mode" :title="'采集模式：' + r.capture_mode">
                  <i class="fa-solid fa-camera"></i> {{ r.capture_mode }}
                </span>
              </div>
              <div class="live-recording-meta">
                <span>开始：<b>{{ r.started_at ? new Date(r.started_at * 1000).toLocaleString() : '—' }}</b></span>
                <span>已录制：<b>{{ formatDuration(r.duration_secs) }}</b></span>
                <span>大小：<b>{{ formatFileSize(r.file_size) }}</b></span>
                <span>分段：<b>{{ r.segment_count ?? 0 }}</b></span>
              </div>
              <!-- 进度条：基于 duration_secs 相对于默认 2h 时长做参考 -->
              <div class="live-recording-progress">
                <div class="progress">
                  <div class="bar" :style="{ width: Math.min(100, (r.duration_secs || 0) / 7200 * 100).toFixed(1) + '%' }"></div>
                </div>
                <div class="live-recording-progress-meta">
                  <span v-if="r.stream_protocol">流协议：<b>{{ r.stream_protocol }}</b></span>
                  <span v-if="r.stream_format">格式：<b>{{ r.stream_format }}</b></span>
                  <span v-if="r.stream_codec">编码：<b>{{ r.stream_codec }}</b></span>
                  <span v-if="r.stream_quality">清晰度：<b>{{ qualityLabel(r.stream_quality) }}</b></span>
                </div>
              </div>
              <!-- 互动统计行 -->
              <div v-if="(r.danmaku_count ?? 0) > 0 || (r.unique_user_count ?? 0) > 0 || (r.peak_watched ?? 0) > 0" class="live-recording-stats">
                <span class="live-recording-stat"><i class="fa-solid fa-comments"></i> 弹幕 <b>{{ (r.danmaku_count ?? 0).toLocaleString() }}</b></span>
                <span class="live-recording-stat"><i class="fa-solid fa-user-group"></i> 独立用户 <b>{{ (r.unique_user_count ?? 0).toLocaleString() }}</b></span>
                <span v-if="(r.peak_watched ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-eye"></i> 峰值 <b>{{ (r.peak_watched ?? 0).toLocaleString() }}</b></span>
                <span v-if="(r.sc_count ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-message-dollar"></i> SC <b>{{ r.sc_count ?? 0 }}</b></span>
                <span v-if="(r.guard_count ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-shield-halved"></i> 上船 <b>{{ r.guard_count ?? 0 }}</b></span>
                <span v-if="(r.estimated_paid_value ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-coins"></i> 礼物 ¥<b>{{ (r.estimated_paid_value ?? 0).toFixed(2) }}</b></span>
                <span v-if="r.danmu_unavailable" class="live-recording-stat warn"><i class="fa-solid fa-triangle-exclamation"></i> 弹幕采集异常</span>
                <span v-if="r.interaction_capture_status && r.interaction_capture_status !== 'ok' && r.interaction_capture_status !== 'running'" class="live-recording-stat warn" :title="r.interaction_error || ''">
                  <i class="fa-solid fa-circle-exclamation"></i> 互动采集 {{ r.interaction_capture_status }}
                </span>
              </div>
              <div v-if="r.error_msg" class="live-recording-error">
                <i class="fa-solid fa-triangle-exclamation"></i> {{ r.error_msg }}
              </div>
            </div>
            <div class="live-recording-actions">
              <button v-if="r.status === 'recording'" class="btn btn-sm btn-danger" @click="stopRecord(r.recording_id)">停止</button>
              <button v-if="r.status === 'stopped'" class="btn btn-sm btn-primary" @click="startMergeJob(r.recording_id)">合并</button>
            </div>
          </div>

          <div v-if="mergeJobs.size > 0" style="margin-top: 16px;">
            <div class="live-section-title"><i class="fa-solid fa-compress"></i> 合并任务</div>
            <table class="table">
              <thead><tr><th>任务 ID</th><th>状态</th><th>进度</th><th>操作</th></tr></thead>
              <tbody>
                <tr v-for="[jobId, j] in mergeJobs" :key="jobId">
                  <td><code>{{ jobId }}</code></td>
                  <td>{{ j.status }}</td>
                  <td>{{ j.progress || 0 }}%</td>
                  <td>
                    <button v-if="j.status === 'pending' || j.status === 'running'" class="btn btn-sm" @click="cancelMergeJob(jobId)">取消</button>
                    <span v-else-if="j.status === 'completed' && j.output_path" class="tone-success">{{ j.output_path }}</span>
                    <span v-else-if="j.status === 'failed'" class="tone-error">{{ j.error }}</span>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </div>
      <div :class="['live-board-panel', { active: subTab === 'history' }]" id="live-panel-history">
        <div v-if="live.history.length === 0" id="live-history-list" class="live-history-list">
          <p class="empty-hint">暂无最近录制</p>
        </div>
        <div v-else id="live-history-list" class="live-history-list">
          <div v-for="(h, hi) in live.history" :key="h.recording_id || hi" class="live-recording-row">
            <div class="live-recording-main">
              <div class="live-recording-title">
                <span class="live-room-name">{{ h.uname || h.room_id }}</span>
                <span class="live-badge completed">已完成</span>
              </div>
              <div class="live-recording-meta">
                <span>开始：<b>{{ h.started_at ? new Date(h.started_at * 1000).toLocaleString() : '—' }}</b></span>
                <span>大小：<b>{{ h.size || '—' }}</b></span>
              </div>
            </div>
            <div class="live-recording-actions">
              <button class="btn btn-sm" v-if="(h as any).file_path" @click="live.openRecording((h as any).id ?? h.recording_id)"><i class="fa-solid fa-folder-open"></i> 打开目录</button>
            </div>
          </div>
        </div>
      </div>
      <div :class="['live-board-panel', { active: subTab === 'attention' }]" id="live-panel-attention">
        <div id="live-attention-list">
          <div v-if="attentionCount === 0" class="empty-state">
            <i class="fa-solid fa-check-circle"></i>
            <p>暂无需要处理的项目</p>
          </div>
          <div v-else>
            <div v-for="r in live.recordings.filter(r => ['merging','failed','merge_failed'].includes(r.status || ''))" :key="r.recording_id" class="live-recording-row">
              <div class="live-recording-main">
                <div class="live-recording-title">
                  <span class="live-room-name">{{ r.uname || r.room_id }}</span>
                  <span :class="['live-badge', r.status === 'merging' ? 'starting' : 'warn']">
                    {{ r.status === 'merging' ? '合并中' : r.status === 'failed' ? '失败' : r.status }}
                  </span>
                </div>
                <div class="live-recording-meta">
                  <span>开始：<b>{{ r.started_at ? new Date(r.started_at * 1000).toLocaleString() : '—' }}</b></span>
                </div>
              </div>
              <div class="live-recording-actions">
                <button v-if="r.status === 'stopped'" class="btn btn-sm btn-primary" @click="startMergeJob(r.recording_id)">合并</button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 添加直播源弹窗：与原版 live-add-modal 1:1 同构 -->
    <div v-if="showAdd" id="live-add-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="live-add-title" @click.self="showAdd = false">
      <div class="modal-container">
        <div class="modal-header">
          <i class="fa-solid fa-satellite-dish"></i>
          <span id="live-add-title">添加直播源</span>
          <button type="button" class="modal-close-btn" id="live-add-close-btn" aria-label="关闭" @click="showAdd = false">
            <i class="fa-solid fa-times"></i>
          </button>
        </div>
        <div class="form-section">
          <div class="form-group form-full">
            <label for="live-add-input"><i class="fa-solid fa-link"></i> 房间号或直播链接（支持批量粘贴）</label>
            <textarea id="live-add-input" v-model="addRoomInput" class="form-control live-add-textarea" rows="3" placeholder="例如 123456 或 https://live.bilibili.com/123456，多个房间用换行 / 空格 / 逗号分隔"></textarea>
            <div class="form-note">添加后默认关闭自动录制；请在房间详情的"设置"中开启自动录制并配置周排期。短号会自动解析为长号。</div>
          </div>
          <div class="form-group form-full policy-grid">
            <div class="form-group">
              <label for="live-add-quality"><i class="fa-solid fa-film"></i> 清晰度上限</label>
              <select id="live-add-quality" v-model.number="addConfig.quality" class="form-control">
                <option :value="10000">原画（10000，推荐）</option>
                <option :value="400">蓝光（400）</option>
                <option :value="250">超清（250）</option>
                <option :value="150">高清（150）</option>
                <option :value="80">流畅（80）</option>
              </select>
            </div>
            <div class="form-group">
              <label for="live-add-segments"><i class="fa-solid fa-clock"></i> 分段时长（秒）</label>
              <input id="live-add-segments" type="number" v-model.number="addConfig.segment_seconds" class="form-control" />
            </div>
            <label class="choice-row">
              <span>自动录制（检测到开播时自动开始）</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="addConfig.auto_record" />
                <span class="slider"></span>
              </span>
            </label>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" id="live-add-cancel-btn" @click="showAdd = false">
            <i class="fa-solid fa-times"></i> 取消
          </button>
          <button class="btn btn-primary" id="live-add-confirm-btn" data-network-required="true" :disabled="adding" @click="confirmAdd">
            <i class="fa-solid fa-check"></i> {{ adding ? '查询中…' : '查询并添加' }}
          </button>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.live-room-pill {
  display: inline-block;
  font-size: 11px;
  padding: 1px 6px;
  border-radius: 999px;
  background: var(--bg-tag, #eef2ff);
  color: var(--text-muted, #6b7280);
  margin-left: 6px;
}
.live-room-pill.live {
  background: #fee2e2;
  color: #b91c1c;
}
.live-room-pill.warn {
  background: #fef3c7;
  color: #92400e;
}
.live-room-info-card {
  margin-top: 12px;
  border: 1px solid var(--border, #e5e7eb);
  border-radius: 8px;
  padding: 12px;
  background: var(--bg-soft, #f7f8fa);
}
.live-info-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: 10px 16px;
  margin-top: 8px;
}
.live-info-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}
.live-info-label {
  font-size: 11px;
  color: var(--text-muted, #6b7280);
}
.live-info-value {
  font-size: 13px;
  word-break: break-word;
  white-space: normal;
}
.live-info-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}
.live-info-tag {
  background: var(--bg-tag, #eef2ff);
  color: var(--text-tag, #4338ca);
  padding: 1px 6px;
  border-radius: 4px;
  font-size: 11px;
}
.live-section-subtitle {
  font-size: 12px;
  color: var(--text-muted, #6b7280);
  margin: 8px 0 4px;
}
.live-room-info-progress {
  margin-top: 8px;
  border-top: 1px dashed var(--border, #e5e7eb);
  padding-top: 8px;
}
.live-room-info-progress-row {
  background: var(--card-bg, #fff);
  border: 1px solid var(--border, #e5e7eb);
  border-radius: 6px;
  padding: 8px 10px;
}
.live-progress-line {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  align-items: baseline;
  font-size: 12px;
  margin-bottom: 4px;
}
.live-progress-label {
  color: var(--text-muted, #6b7280);
}
.live-progress-value {
  font-weight: 600;
}
.live-progress-sep {
  color: var(--text-muted, #d1d5db);
  margin: 0 4px;
}
.live-progress-error {
  color: #b91c1c;
  font-size: 12px;
  margin-top: 4px;
}
.live-recording-row-detailed {
  flex-direction: column;
  align-items: stretch;
}
.live-recording-trigger,
.live-recording-mode {
  font-size: 11px;
  color: var(--text-muted, #6b7280);
  background: var(--bg-tag, #eef2ff);
  padding: 1px 6px;
  border-radius: 4px;
  margin-left: 6px;
}
.live-recording-progress {
  margin-top: 6px;
}
.live-recording-progress-meta {
  font-size: 11px;
  color: var(--text-muted, #6b7280);
  margin-top: 4px;
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}
.live-recording-stats {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 6px;
  font-size: 12px;
  color: var(--text-muted, #4b5563);
}
.live-recording-stat {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.live-recording-stat.warn {
  color: #b91c1c;
}
.live-recording-error {
  margin-top: 6px;
  color: #b91c1c;
  font-size: 12px;
}
.live-badge.recording {
  background: #fee2e2;
  color: #b91c1c;
}
.live-badge.starting {
  background: #fef3c7;
  color: #92400e;
}
.live-badge.completed {
  background: #d1fae5;
  color: #065f46;
}
.live-badge.warn {
  background: #fee2e2;
  color: #b91c1c;
}
.live-badge.live {
  background: #d1fae5;
  color: #065f46;
}
</style>
