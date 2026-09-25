<script setup lang="ts">
/**
 * 直播页：保留迁移前已验证的交互语义。
 * - dashboard 轮询 30s（后端 poll_interval_secs）、事件轮询 2s、时长 tick 1s；
 *   页面隐藏时暂停，恢复可见立即刷新（对齐 polling.js / live.js visibilitychange）。
 * - 状态映射、文案、确认弹窗、toast 均以 live.js 逐字对齐。
 * 结构：房间详情 + 实时互动（本组件）；弹窗 / 实时信息卡 / 互动面板 / 录制看板
 * 分别拆在 LiveSourceModals / LiveRoomInfoCard / LiveInteractionPanel / LiveRecordingBoard。
 */
import { ref, computed, onUnmounted, onActivated, onDeactivated } from 'vue';
import {
  useLiveStore, videoStatusText, videoStatusClass, interactionStateText, interactionStateClass,
  captureModeText, qualityText, sourceState, formatFileSize, relativeTime, scheduleSummary,
} from '@/stores/live';
import type { LiveSource } from '@/api/types';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { imageUrl, imageError } from '@/utils/image';
import { useLiveEvents } from '@/composables/useLiveEvents';
import { useLiveRoomInfo } from '@/composables/useLiveRoomInfo';
import { useLiveBurnTracking } from '@/composables/useLiveBurnTracking';
import { useLiveTick } from '@/composables/useLiveTick';
import LiveSourceModals from '@/components/LiveSourceModals.vue';
import LiveRoomInfoCard from '@/components/LiveRoomInfoCard.vue';
import LiveInteractionPanel from '@/components/LiveInteractionPanel.vue';
import LiveRecordingBoard from '@/components/LiveRecordingBoard.vue';

const live = useLiveStore();
const toast = useToastStore();

/** toast 文案：优先取后端 result.message，无（或 'success'）时用老框架兜底文案。 */
function messageOr(message: string | undefined, fallback: string): string {
  return message && message !== 'success' ? message : fallback;
}

// ===== 选中房间 / 当前录制 session =====

const currentSession = computed(() => live.selectedSource
  ? live.recordings.find(r => r.room_id === live.selectedSource!.room_id) || null
  : null);

function isRecordingRoom(roomId: number): boolean {
  return live.recordings.some(item => item.room_id === roomId);
}

function roomStateText(s: LiveSource): string { return sourceState(s)[1]; }
/** 侧边栏指示点（对齐 live.js renderSidebar）：录制 > 直播 > 异常 > 无。 */
function roomDotClass(s: LiveSource): string {
  const [stateCls] = sourceState(s);
  if (isRecordingRoom(s.room_id)) return 'recording';
  if (stateCls === 'live') return 'live';
  if (['risk', 'stale', 'unknown'].includes(stateCls)) return 'warn';
  return '';
}

function selectRoomByRoomId(roomId: number, force = false) {
  const source = live.sources.find(s => s.room_id === roomId);
  if (!source) return;
  if (!force && live.selectedSourceId === source.id) return;
  live.selectSource(source.id);
  // 对齐老框架 selectRoom：切换房间时重置互动事件流。
  resetEventStream();
}

function viewRoomDetail(roomId: number) {
  selectRoomByRoomId(roomId);
  document.getElementById('live-detail-panel')?.scrollIntoView({ behavior: 'smooth', block: 'start' });
}

// ===== 操作（pending 按钮态对齐 live.js pendingRooms / mergePending）=====

const pendingRooms = ref<Set<number>>(new Set());
const mergePending = ref<Set<string>>(new Set());

function addPending(set: typeof pendingRooms | typeof mergePending, id: number | string) {
  const next = new Set(set.value as Set<any>) as any;
  next.add(id);
  set.value = next;
}
function removePending(set: typeof pendingRooms | typeof mergePending, id: number | string) {
  const next = new Set(set.value as Set<any>) as any;
  next.delete(id);
  set.value = next;
}

async function startRecord(roomId: number) {
  if (pendingRooms.value.has(roomId)) return;
  addPending(pendingRooms, roomId);
  try {
    const r = await live.startRecording(roomId);
    toast.success(messageOr(r?.message, '录制已开始'));
    void refreshRoomInfo();
  } catch (e: any) {
    toast.error(`开始录制失败：${e?.message || '未知错误'}`);
  } finally {
    removePending(pendingRooms, roomId);
  }
}

async function stopRecord(roomId: number) {
  if (pendingRooms.value.has(roomId)) return;
  const confirmed = await confirmDialog({
    title: '停止录制',
    message: '停止会先收尾互动数据，再合并并校验录制文件；该过程可能需要一些时间，期间本场不会自动重新拉起。继续吗？',
    confirmText: '停止并合并',
    tone: 'danger',
  });
  if (!confirmed) return;
  addPending(pendingRooms, roomId);
  try {
    const r = await live.stopRecording(roomId);
    const operationId = r?.data?.operation_id;
    toast.success(operationId
      ? `停止请求已接受，后台任务正在收尾（${String(operationId).slice(0, 8)}）`
      : '录制已停止');
  } catch (e: any) {
    toast.error(`停止录制失败：${e?.message || '未知错误'}`);
  } finally {
    removePending(pendingRooms, roomId);
  }
}

async function removeSource(id: number) {
  const confirmed = await confirmDialog({
    title: '删除直播源',
    message: '确定删除这个直播源吗？仅取消关注与自动录制策略，已录制的文件不会被删除，可在"最近录制"中找到。',
    confirmText: '删除',
    tone: 'danger',
  });
  if (!confirmed) return;
  try {
    const r = await live.deleteSource(id);
    toast.success(messageOr(r?.message, '直播源已删除'));
  } catch (e: any) {
    toast.error(`删除失败：${e?.message || '未知错误'}`);
  }
}

// ===== 烧录弹幕（确认框 + burn/status 3s 轮询至 30 分钟 + 终态 toast）=====

const { trackBurnTask } = useLiveBurnTracking();

async function burnHistoryDanmaku(recording: any) {
  const id = recording.id ?? recording.recording_id;
  if (id == null || recording.has_burned) return;
  const confirmed = await confirmDialog({
    title: '烧录互动弹幕',
    message: '将把本场录到的弹幕和 SC 烧录进视频，生成一个新的"弹幕版"文件，原视频不变。烧录耗时取决于视频长度，期间可继续使用其他功能。继续吗？',
    confirmText: '开始烧录',
    tone: 'default',
  });
  if (!confirmed) return;
  try {
    const r = await live.burnDanmaku(id);
    // 对齐 live.js:885：启动 toast 硬编码，不取后端 message（后端文案与老框架不同）。
    toast.success('烧录任务已排队，完成后会提示');
    const taskId = r?.data?.task_id;
    if (taskId) trackBurnTask(String(taskId));
  } catch (e: any) {
    toast.error(`启动烧录失败：${e?.message || '未知错误'}`);
  }
}

/** 重试合并（对齐 live.js history-merge）：无成功 toast，仅刷新与错误 toast。 */
async function startMergeJob(recordingId: string | number) {
  const key = String(recordingId);
  if (mergePending.value.has(key)) return;
  addPending(mergePending, key);
  try {
    await live.startMerge(key);
    await live.refreshDashboard();
  } catch (e: any) {
    toast.error(`合并任务创建失败：${e?.message || '未知错误'}`);
  } finally {
    removePending(mergePending, key);
  }
}

/** 取消合并（对齐 live.js merge-cancel）：无成功 toast。 */
async function cancelMergeJob(jobId: string) {
  try {
    await live.cancelMerge(jobId);
    await live.refreshDashboard();
  } catch (e: any) {
    toast.error(`取消合并失败：${e?.message || '未知错误'}`);
  }
}

/** 打开目录（对齐 live.js history-open）：无成功 toast。 */
async function openRecording(recordingId: number | string) {
  try {
    await live.openRecording(recordingId);
  } catch (e: any) {
    toast.error(`打开目录失败：${e?.message || '未知错误'}`);
  }
}

// ===== 直播源弹窗（显隐在父级控制，表单与保存逻辑在 LiveSourceModals）=====

const showAdd = ref(false);
const showSettings = ref(false);

function openAddModal() { showAdd.value = true; }
function openSettingsModal() { showSettings.value = true; }

// ===== 轮询（P1-6：dashboard 30s / 事件 2s / tick 1s，页面隐藏暂停）=====

const { eventFilter, eventStatus, displayEvents, scEvents, heatBuckets,
        pollLiveEvents, startEventPolling, stopEventPolling, resetEventStream } = useLiveEvents();
const { localRoomInfo, localRoomInfoError, localRoomInfoLoading, refreshRoomInfo } = useLiveRoomInfo({
  onLoaded: () => { void pollLiveEvents(); },
});
const { startTick, stopTick, liveDurationText } = useLiveTick();

let tabActive = false;
let dashboardTimer: number | null = null;
let visibilityHandler: (() => void) | null = null;

function startDashboardPolling() {
  if (dashboardTimer) return;
  dashboardTimer = window.setInterval(() => {
    if (document.visibilityState !== 'hidden') void live.refreshDashboard();
  }, 30000);
}
function stopDashboardPolling() {
  if (!dashboardTimer) return;
  clearInterval(dashboardTimer);
  dashboardTimer = null;
}

async function refreshOnEnter() {
  await live.refreshDashboard();
  startDashboardPolling();
  startEventPolling();
  startTick();
}

onActivated(() => {
  tabActive = true;
  void refreshOnEnter();
  if (!visibilityHandler) {
    // 对齐老框架 visibilitychange：恢复可见且直播页激活时立即刷新。
    visibilityHandler = () => {
      if (document.visibilityState !== 'hidden' && tabActive) {
        void live.refreshDashboard();
        void pollLiveEvents();
      }
    };
    document.addEventListener('visibilitychange', visibilityHandler);
  }
});
onDeactivated(() => {
  tabActive = false;
  stopDashboardPolling();
  stopEventPolling();
  stopTick();
  if (visibilityHandler) {
    document.removeEventListener('visibilitychange', visibilityHandler);
    visibilityHandler = null;
  }
});
onUnmounted(() => {
  stopDashboardPolling();
  stopEventPolling();
  stopTick();
  if (visibilityHandler) {
    document.removeEventListener('visibilitychange', visibilityHandler);
    visibilityHandler = null;
  }
});

async function refreshDashboardFromUi() {
  if (!await live.refreshDashboard()) toast.error(`直播状态同步失败：${live.dashboardError || '未知错误'}`);
}

// ===== B 站同步状态组（对齐 live.js setSyncStates 文案与指示点）=====

const monitorState = computed(() => (live.monitorRunning === null ? 'stale' : live.monitorRunning ? 'ok' : 'error'));
const monitorLabel = computed(() => {
  if (live.monitorRunning === null) return '监控：等待数据';
  if (live.monitorRunning) return `监控运行中${live.lastHeartbeatAt ? ` · 心跳 ${relativeTime(live.lastHeartbeatAt)}` : ''}`;
  return '监控未运行';
});
const biliSyncState = computed(() => {
  if (!live.dashboardLoaded) return 'stale';
  if (live.riskNotice) return 'stale';
  return live.lastSuccessAt ? 'ok' : 'stale';
});
const biliSyncLabel = computed(() => {
  if (!live.dashboardLoaded) return 'B站状态：等待检查';
  if (live.riskNotice) return 'B站检查受限，退避中';
  if (live.lastSuccessAt) return `B站状态：${relativeTime(live.lastSuccessAt)}更新`;
  return 'B站状态：尚未成功检查';
});
const biliSyncDot = computed(() => {
  if (!live.dashboardLoaded) return '';
  // 对齐 live.js setSyncStates：risk_notice 分支先于 lastSuccess，dot 恒为 connecting。
  if (live.riskNotice) return 'connecting';
  return live.lastSuccessAt ? 'connected' : 'connecting';
});

const nextScheduleText = computed(() => {
  const next = (live.selectedSource as any)?.runtime?.next_schedule_at;
  if (!next) return '';
  return `下次自动开始：${relativeTime(next).replace('前', '')}${new Date(next).toLocaleString('zh-CN', { hour12: false })}`;
});
</script>

<template>
  <section class="tab-panel">
    <!-- 直播录制看板：布局对齐博主监控看板（侧边栏 + 详情） -->
    <div class="card">
      <div class="card-title">
        <span><i class="fa-solid fa-tower-broadcast"></i> 直播录制看板</span>
        <div class="live-sync-group" id="live-sync-group" aria-live="polite">
          <span class="live-sync-item" id="live-sync-monitor" :data-state="monitorState" title="后端开播监控 worker 的运行状态">
            <span class="aria2-dot" :class="monitorState === 'ok' ? 'connected' : monitorState === 'error' ? 'failed' : ''"></span>{{ monitorLabel }}
          </span>
          <span class="live-sync-item" id="live-sync-bili" :data-state="biliSyncState" title="最近一次成功向 B 站检查开播状态的时间">
            <span class="aria2-dot" :class="biliSyncDot"></span>{{ biliSyncLabel }}
          </span>
          <button class="btn btn-sm btn-ghost" id="live-refresh-btn" data-network-required="true" title="手动刷新直播状态" @click="refreshDashboardFromUi">
            <i class="fa-solid fa-arrows-rotate"></i> 刷新
          </button>
        </div>
      </div>
      <div v-if="live.dashboardError" class="live-alert error" role="alert">
        <i class="fa-solid fa-triangle-exclamation"></i> 直播看板刷新失败：{{ live.dashboardError }}
      </div>
      <!-- 风险提示条（P2-9）：数据源 dashboard.risk_notice，对齐老框架文案，无关闭按钮、不与全局横幅重复。 -->
      <div v-if="live.riskNotice" id="live-risk-notice" class="live-alert warn" role="alert">
        <i class="fa-solid fa-triangle-exclamation"></i> B 站限制了状态检查：{{ live.riskNotice }} 期间显示的是最后一次成功结果，系统会自动重试并在恢复后降低退避等级。
      </div>
      <div class="live-dashboard">
        <!-- 房间列表侧边栏 -->
        <aside class="live-sidebar">
          <div class="live-sidebar-title">
            <span><i class="fa-solid fa-list"></i> 关注房间（<span id="live-source-count">{{ live.sources.length }}</span>）</span>
            <button class="btn btn-sm btn-ghost" id="live-show-add-btn" data-network-required="true" data-mutating @click="openAddModal">
              <i class="fa-solid fa-plus"></i> 添加
            </button>
          </div>
          <div id="live-room-list" aria-live="polite">
            <div v-if="!live.dashboardLoaded" class="live-skeleton" aria-label="正在加载直播面板">
              <span class="skeleton skeleton-avatar"></span><span class="skeleton skeleton-line"></span><span class="skeleton skeleton-line short"></span>
            </div>
            <p v-else-if="!live.sources.length" class="empty-hint">暂无关注房间</p>
            <template v-else>
              <div v-for="s in live.sources" :key="s.id"
                   :class="['live-room-item', { active: live.selectedSourceId === s.id }]"
                   role="button" tabindex="0"
                   @click="selectRoomByRoomId(s.room_id)"
                   @keydown.enter.prevent="selectRoomByRoomId(s.room_id)"
                   @keydown.space.prevent="selectRoomByRoomId(s.room_id)">
                <template v-if="s.face">
                  <img :src="imageUrl(s.face)" class="live-room-avatar" alt="" loading="lazy" @error="imageError" />
                  <span class="live-room-avatar live-room-avatar-fallback" hidden>{{ (s.uname || '直').slice(0, 1) }}</span>
                </template>
                <span v-else class="live-room-avatar live-room-avatar-fallback">{{ (s.uname || '直').slice(0, 1) }}</span>
                <div class="live-room-info">
                  <div class="live-room-name">
                    <span>{{ s.uname || `UID ${s.uid}` }}</span>
                    <span :class="['live-room-dot', roomDotClass(s)]"></span>
                  </div>
                  <div class="live-room-meta">#{{ s.room_id }} · {{ roomStateText(s) }}{{ isRecordingRoom(s.room_id) ? ' · 录制中' : s.auto_record ? ' · 自动录制' : '' }}</div>
                </div>
              </div>
            </template>
          </div>
          <div class="live-sidebar-actions">
            <button class="btn btn-primary btn-block" id="live-refresh-list-btn" data-network-required="true" @click="refreshDashboardFromUi">
              <i class="fa-solid fa-rotate"></i> 刷新列表
            </button>
          </div>
        </aside>

        <!-- 房间详情面板 -->
        <div class="live-detail-panel" id="live-detail-panel">
          <div v-if="!live.sources.length && !live.selectedSource" class="live-empty-state" id="live-empty-state">
            <i class="fa-solid fa-tower-broadcast"></i>
            <p>暂无直播源</p>
            <p class="empty-hint">点击上方"添加"输入房间号或直播链接，查询后即可关注</p>
          </div>
          <div v-else-if="!live.selectedSource" class="live-empty-state">
            <i class="fa-solid fa-tower-broadcast"></i>
            <p>请选择左侧房间</p>
          </div>
          <div v-else id="live-detail-content">
            <div class="live-detail-header">
              <template v-if="(live.selectedSource as any).cover">
                <img :src="imageUrl((live.selectedSource as any).cover)" class="live-cover-thumb" alt="" loading="lazy" @error="imageError" />
                <div class="live-cover-thumb live-cover-placeholder" hidden><i class="fa-solid fa-tower-broadcast"></i></div>
              </template>
              <div v-else class="live-cover-thumb live-cover-placeholder"><i class="fa-solid fa-tower-broadcast"></i></div>
              <div class="live-detail-main">
                <div class="live-detail-title-row">
                  <span class="live-detail-title">{{ live.selectedSource.title || '（未开播，暂无标题）' }}</span>
                  <span :class="['live-badge', sourceState(live.selectedSource)[0]]">{{ sourceState(live.selectedSource)[1] }}</span>
                  <span v-if="currentSession" :class="['live-badge', videoStatusClass(currentSession.status)]">视频 {{ videoStatusText(currentSession.status) }}</span>
                  <span v-if="currentSession" :class="['live-badge', interactionStateClass(currentSession.interaction_capture_status || 'off')]">互动 {{ interactionStateText(currentSession.interaction_capture_status || 'off') }}</span>
                </div>
                <div class="live-detail-meta">
                  <span>{{ live.selectedSource.uname || `UID ${live.selectedSource.uid}` }}</span>
                  <span>房间 {{ live.selectedSource.room_id }}<template v-if="(live.selectedSource as any).short_id">（短号 {{ (live.selectedSource as any).short_id }}）</template></span>
                  <span>最近检查：{{ live.selectedSource.runtime?.last_checked_at ? relativeTime(live.selectedSource.runtime.last_checked_at) : '尚未' }}</span>
                  <span v-if="live.selectedSource.runtime?.error">检查异常：{{ live.selectedSource.runtime.error }}</span>
                </div>
                <div class="live-detail-actions">
                  <button v-if="currentSession" class="btn btn-danger" data-mutating
                          :disabled="pendingRooms.has(live.selectedSource.room_id)"
                          @click="stopRecord(live.selectedSource.room_id)">
                    {{ pendingRooms.has(live.selectedSource.room_id) ? '处理中…' : '停止并合并' }}
                  </button>
                  <button v-else class="btn btn-primary" data-mutating
                          :disabled="live.selectedSource.runtime?.live_status !== 1 || pendingRooms.has(live.selectedSource.room_id)"
                          @click="startRecord(live.selectedSource.room_id)">
                    {{ pendingRooms.has(live.selectedSource.room_id) ? '处理中…' : '手动录制' }}
                  </button>
                  <button class="btn btn-ghost" data-mutating
                          :disabled="pendingRooms.has(live.selectedSource.room_id)"
                          @click="openSettingsModal">
                    <i class="fa-solid fa-sliders"></i> 设置
                  </button>
                  <button class="btn btn-ghost" data-mutating
                          :disabled="!!currentSession || pendingRooms.has(live.selectedSource.room_id)"
                          :title="currentSession ? '录制中不可删除，请先停止' : ''"
                          @click="removeSource(live.selectedSource.id)">
                    <i class="fa-solid fa-trash"></i> 删除源
                  </button>
                </div>
              </div>
            </div>

            <!-- 告警条（对齐 live.js renderDetail alerts） -->
            <div v-if="live.selectedSource.runtime?.risk_limited" class="live-alert error">
              <i class="fa-solid fa-triangle-exclamation"></i>
              <span>B 站当前限制状态检查：下方显示的是最后一次成功结果，不代表当前真实开播状态。{{ live.selectedSource.runtime?.next_retry_at ? `预计 ${relativeTime(live.selectedSource.runtime.next_retry_at)}自动重试。` : '系统会自动重试。' }}</span>
            </div>
            <div v-else-if="currentSession?.interaction_capture_status === 'degraded'" class="live-alert warn">
              <i class="fa-solid fa-circle-exclamation"></i>
              <span>互动采集已降级：{{ currentSession?.error_msg || '弹幕连接不可用' }}。视频录制不受影响。</span>
            </div>
            <div v-else-if="currentSession?.interaction_capture_status === 'unavailable'" class="live-alert error">
              <i class="fa-solid fa-triangle-exclamation"></i>
              <span>互动采集不可用：{{ currentSession?.error_msg || '弹幕连接失败' }}。视频录制不受影响。</span>
            </div>
            <div v-else-if="currentSession?.error_msg" class="live-alert warn">
              <i class="fa-solid fa-circle-exclamation"></i>
              <span>{{ currentSession?.error_msg }}</span>
            </div>
            <div v-if="(live.selectedSource as any).manual_stop_latched" class="live-alert warn">
              <i class="fa-solid fa-lock"></i>
              <span>本场已手动停止，等待真正下播后才会解除，期间不会自动重新拉起。</span>
            </div>
            <div v-if="live.selectedSource.runtime?.schedule_overrun" class="live-alert warn">
              <i class="fa-solid fa-clock"></i>
              <span>已超过排期结束时间，当前策略是录制至下播。</span>
            </div>

            <!-- 实时直播信息卡片（room-info 实时数据） -->
            <div v-if="localRoomInfoError" class="live-alert error" role="alert">{{ localRoomInfoError }}</div>
            <LiveRoomInfoCard v-if="localRoomInfo"
                              :room-info="localRoomInfo"
                              :loading="localRoomInfoLoading"
                              :session="currentSession"
                              @refresh="refreshRoomInfo" />

            <!-- 状态摘要行 + 策略摘要（对齐 live.js renderDetail） -->
            <div class="blogger-status-display">
              <div class="blogger-status-row">
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-circle"></i> B站开播状态</span>
                  <span :class="['status-value', sourceState(live.selectedSource)[0] === 'live' ? 'running' : '']">{{ sourceState(live.selectedSource)[1] }}</span>
                </div>
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-record-vinyl"></i> 视频录制</span>
                  <span :class="['status-value', currentSession ? 'running' : '']">{{ currentSession ? videoStatusText(currentSession.status) : '未在录制' }}</span>
                </div>
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-comments"></i> 互动采集</span>
                  <span class="status-value">{{ currentSession ? interactionStateText(currentSession.interaction_capture_status || 'off') : captureModeText(live.selectedSource.capture_mode) }}</span>
                </div>
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-clock"></i> 最近检查</span>
                  <span class="status-value">{{ live.selectedSource.runtime?.last_checked_at ? relativeTime(live.selectedSource.runtime.last_checked_at) : '--' }}</span>
                </div>
              </div>
              <div class="live-strategy-summary">
                <i class="fa-solid fa-calendar-week"></i>
                <span>自动录制：{{ live.selectedSource.auto_record ? '开' : '关' }} · {{ scheduleSummary(live.selectedSource) }}{{ nextScheduleText ? ` · ${nextScheduleText}` : '' }} · 互动采集：{{ captureModeText(live.selectedSource.capture_mode) }} · 清晰度上限：{{ qualityText(live.selectedSource.quality || 10000) }}</span>
              </div>
            </div>

            <!-- 录制信息（对齐 live.js recordingInfoMarkup，无 segment_count） -->
            <div v-if="currentSession" class="blogger-status-display">
              <div class="blogger-status-row">
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-hourglass-half"></i> 录制时长</span>
                  <span class="status-value">{{ liveDurationText(currentSession) }}</span>
                </div>
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-file"></i> 文件大小</span>
                  <span class="status-value">{{ formatFileSize(currentSession.file_size) }}</span>
                </div>
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-bolt"></i> 触发方式</span>
                  <span class="status-value">{{ currentSession.trigger === 'auto' ? '自动' : '手动' }}</span>
                </div>
                <div v-if="currentSession.stream_quality" class="status-item">
                  <span class="status-label"><i class="fa-solid fa-film"></i> 清晰度</span>
                  <span class="status-value">{{ qualityText(currentSession.stream_quality) }}</span>
                </div>
                <div v-if="(currentSession as any).dropped_event_count" class="status-item">
                  <span class="status-label"><i class="fa-solid fa-triangle-exclamation"></i> 丢失事件</span>
                  <span class="status-value paused">{{ (currentSession as any).dropped_event_count }}</span>
                </div>
              </div>
            </div>

            <!-- 实时互动（对齐 live.js interactionMarkup：session 且互动采集未关闭时显示） -->
            <LiveInteractionPanel v-if="currentSession && currentSession.interaction_capture_status !== 'off'"
                                  :session="currentSession"
                                  :event-status="eventStatus"
                                  :event-filter="eventFilter"
                                  :display-events="displayEvents"
                                  :sc-events="scEvents"
                                  :heat-buckets="heatBuckets"
                                  @update:event-filter="eventFilter = $event" />
          </div>
        </div>
      </div>
    </div>

    <!-- 录制任务看板：布局对齐下载管理（顶栏 + 子 tab） -->
    <LiveRecordingBoard :pending-rooms="pendingRooms"
                        :merge-pending="mergePending"
                        @stop-record="stopRecord"
                        @view-detail="viewRoomDetail"
                        @start-merge="startMergeJob"
                        @burn-danmaku="burnHistoryDanmaku"
                        @open-recording="openRecording"
                        @cancel-merge="cancelMergeJob" />

    <!-- 添加直播源 / 直播源设置弹窗 -->
    <LiveSourceModals v-model:show-add="showAdd"
                      v-model:show-settings="showSettings"
                      @added="selectRoomByRoomId($event, true)" />
  </section>
</template>
