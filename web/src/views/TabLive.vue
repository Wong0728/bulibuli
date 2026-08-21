<script setup lang="ts">
/**
 * 直播页：行为对齐老框架 static/js/live.js（唯一基准）。
 * - dashboard 轮询 30s（后端 poll_interval_secs）、事件轮询 2s、时长 tick 1s；
 *   页面隐藏时暂停，恢复可见立即刷新（对齐 polling.js / live.js visibilitychange）。
 * - 状态映射、文案、确认弹窗、toast 均以 live.js 逐字对齐。
 */
import { ref, computed, onUnmounted, onActivated, onDeactivated, watch } from 'vue';
import {
  useLiveStore, videoStatusText, videoStatusClass, interactionStateText, interactionStateClass,
  captureModeText, qualityText, stopReasonText, sourceState, formatLiveDuration, formatMediaTime,
  formatFileSize, relativeTime, scheduleSummary, parseRoomTokens, validateScheduleStrict,
  mergeLiveEvents, LIVE_WEEKDAYS,
} from '@/stores/live';
import type { LiveSource } from '@/api/types';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { video as videoApi, live as liveApi, download as downloadApi } from '@/api';
import { useModalFocus } from '@/composables/modalFocus';

const live = useLiveStore();
const toast = useToastStore();
const weekdays = LIVE_WEEKDAYS;

function imageUrl(url?: string) { return url ? videoApi.proxyImage(url) : ''; }
function imageError(event: Event) {
  const image = event.target as HTMLImageElement;
  image.hidden = true;
  image.nextElementSibling?.removeAttribute('hidden');
}

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
  eventRecordingId.value = null;
  eventSeq.value = 0;
  liveEvents.value = [];
  eventRequestId += 1;
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

// ===== 烧录弹幕（P0-2：确认框 + burn/status 3s 轮询至 30 分钟 + 终态 toast）=====

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

/** 对齐 live.js trackBurnTask：每 3s 查询一次，最长 30 分钟；完成/失败/超时都有 toast。 */
function trackBurnTask(taskId: string) {
  const startedAt = Date.now();
  const poll = async () => {
    try {
      const response: any = await downloadApi.burnStatus(taskId);
      const status = response?.status;
      if (status === 'completed') {
        toast.success('弹幕烧录完成，已生成弹幕版视频');
        await live.refreshDashboard();
      } else if (status === 'failed') {
        toast.error(`弹幕烧录失败：${response?.message || '未知错误'}`);
      } else if (Date.now() - startedAt > 30 * 60 * 1000) {
        toast.warn('弹幕烧录超时，请到录制目录确认结果');
      } else {
        window.setTimeout(poll, 3000);
      }
    } catch (error) {
      console.error('[live] 查询烧录任务状态失败：', error);
      // 网络异常同样受 30 分钟上限约束，避免后端不可达时无限轮询
      if (Date.now() - startedAt > 30 * 60 * 1000) toast.warn('弹幕烧录超时，请到录制目录确认结果');
      else window.setTimeout(poll, 3000);
    }
  };
  void poll();
}

// ===== 添加直播源弹窗（P1-4：解析规则逐字对齐 + 串行添加 + 成功后选中）=====

const showAdd = ref(false);
const addRoomInput = ref('');
const adding = ref(false);
const addModalRoot = ref<HTMLElement | null>(null);

async function openAddModal() {
  showAdd.value = true;
  addRoomInput.value = '';
}
function closeAddModal() { showAdd.value = false; }

async function confirmAdd() {
  if (adding.value) return;
  const roomIds = parseRoomTokens(addRoomInput.value || '');
  if (!roomIds.length) {
    toast.warn('请输入有效的房间号或 live.bilibili.com 链接');
    return;
  }
  adding.value = true;
  const results = { ok: [] as number[], fail: [] as string[] };
  try {
    // 对齐老框架：串行逐个添加，失败记录「房间号：原因」。
    for (const roomId of roomIds) {
      try {
        await live.addSource(roomId);
        results.ok.push(roomId);
      } catch (error: any) {
        results.fail.push(`${roomId}：${error?.message || '未知错误'}`);
      }
    }
    if (results.ok.length) {
      toast.success(`成功添加 ${results.ok.length} 个直播源（自动录制默认关闭）`);
      closeAddModal();
      await live.refreshDashboard();
      selectRoomByRoomId(results.ok[0], true);
    }
    if (results.fail.length) {
      toast.error(`添加失败 ${results.fail.length} 个：${results.fail.join('；')}`);
    }
  } finally {
    adding.value = false;
  }
}

// ===== 直播源设置弹窗（P2-8：跨天排期校验 + 空排期兜底 + 实时校验提示）=====

const showSettings = ref(false);
const settingsAutoRecord = ref(false);
const settingsQuality = ref(10000);
const settingsCaptureMode = ref<'off' | 'standard' | 'full'>('standard');
const settingsScheduleMode = ref<'all-day' | 'weekly'>('all-day');
const settingsSchedule = ref<Record<string, string[][]>>({});
const savingSettings = ref(false);
const settingsModalRoot = ref<HTMLElement | null>(null);
const scheduleError = ref('');

function emptySchedule(): Record<string, string[][]> {
  return Object.fromEntries(weekdays.map(([key]) => [key, [['', ''], ['', '']]]));
}

function clearScheduleEditor() {
  for (const [day] of weekdays) settingsSchedule.value[day] = [['', ''], ['', '']];
}

function readSettingsSchedule(): Record<string, string[]> {
  return Object.fromEntries(Object.entries(settingsSchedule.value).map(([day, windows]) => [
    day,
    windows.map(([start, end]) => start && end ? `${start}-${end}` : '').filter(Boolean),
  ]));
}

function updateScheduleValidation() {
  if (settingsScheduleMode.value === 'all-day') { scheduleError.value = ''; return; }
  scheduleError.value = validateScheduleStrict(readSettingsSchedule());
}

// 输入实时校验（对齐老框架 updateScheduleValidation），切到全天时清空编辑器。
watch([settingsSchedule, settingsScheduleMode], () => updateScheduleValidation(), { deep: true });
watch(settingsScheduleMode, mode => {
  if (mode === 'all-day') clearScheduleEditor();
});

function openSettingsModal() {
  const source = live.selectedSource;
  if (!source) return;
  const schedule = emptySchedule();
  for (const [day, windows] of Object.entries(source.weekly_schedule || {})) {
    if (!schedule[day]) continue;
    windows.slice(0, 2).forEach((window, index) => {
      const [start, end] = String(window).split('-');
      schedule[day][index] = [start || '', end || ''];
    });
  }
  settingsAutoRecord.value = !!source.auto_record;
  settingsQuality.value = source.quality || 10000;
  settingsCaptureMode.value = (source.capture_mode || 'standard') as 'off' | 'standard' | 'full';
  settingsScheduleMode.value = source.schedule_all_day === false ? 'weekly' : 'all-day';
  settingsSchedule.value = schedule;
  updateScheduleValidation();
  showSettings.value = true;
}

function closeSettingsModal() { showSettings.value = false; }

async function saveSettingsModal() {
  if (savingSettings.value) return;
  const source = live.selectedSource;
  if (!source) return;
  const schedule = readSettingsSchedule();
  if (settingsScheduleMode.value !== 'all-day') {
    const validationError = validateScheduleStrict(schedule);
    if (validationError) { toast.warn(validationError); return; }
    // 对齐老框架：自动录制开启但排期为空时兜底确认「改为全天」。
    if (settingsAutoRecord.value && !Object.values(schedule).some(windows => windows.length)) {
      const confirmed = await confirmDialog({
        title: '排期为空',
        message: '自动录制已开启，但排期为空，这样永远不会自动开始。要改为"全天允许"吗？',
        confirmText: '改为全天',
        tone: 'default',
      });
      if (!confirmed) return;
      settingsScheduleMode.value = 'all-day';
      clearScheduleEditor();
    }
  }
  const finalAllDay = settingsScheduleMode.value === 'all-day';
  savingSettings.value = true;
  try {
    const r = await live.updateSource(source.id, {
      auto_record: settingsAutoRecord.value,
      quality: settingsQuality.value,
      capture_mode: settingsCaptureMode.value,
      clear_schedule: finalAllDay,
      weekly_schedule: finalAllDay ? null : schedule,
    } as any);
    toast.success(messageOr(r?.message, '直播源设置已保存'));
    closeSettingsModal();
  } catch (e: any) {
    toast.error(`保存失败：${e?.message || '未知错误'}`);
  } finally {
    savingSettings.value = false;
  }
}

useModalFocus(showAdd, addModalRoot, closeAddModal);
useModalFocus(showSettings, settingsModalRoot, closeSettingsModal);

// ===== 轮询（P1-6：dashboard 30s / 事件 2s / tick 1s，页面隐藏暂停）=====

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

// ===== 选中房间实时信息（/api/live/room-info，切换时拉取）=====

const localRoomInfo = ref<any | null>(null);
const localRoomInfoError = ref('');
const localRoomInfoLoading = ref(false);
let roomInfoGeneration = 0;

watch(
  () => live.selectedSource?.room_id,
  async (roomId) => {
    const generation = ++roomInfoGeneration;
    if (!roomId) { localRoomInfo.value = null; return; }
    localRoomInfoLoading.value = true;
    localRoomInfoError.value = '';
    try {
      const r = await live.roomInfo(roomId);
      if (generation === roomInfoGeneration && live.selectedSource?.room_id === roomId) {
        localRoomInfo.value = r;
        localRoomInfoError.value = live.roomInfoError || '';
      }
      void pollLiveEvents();
    } finally {
      if (generation === roomInfoGeneration) localRoomInfoLoading.value = false;
    }
  },
  { immediate: true },
);

async function refreshRoomInfo() {
  if (!live.selectedSource) return;
  const roomId = live.selectedSource.room_id;
  const generation = ++roomInfoGeneration;
  localRoomInfoLoading.value = true;
  localRoomInfoError.value = '';
  try {
    const r = await live.roomInfo(roomId, { force: true });
    if (generation === roomInfoGeneration && live.selectedSource?.room_id === roomId) {
      localRoomInfo.value = r;
      localRoomInfoError.value = live.roomInfoError || '';
    }
  } finally {
    if (generation === roomInfoGeneration) localRoomInfoLoading.value = false;
  }
}

// ===== B 站同步状态组（对齐 live.js setSyncStates 文案与指示点）=====

const pageSyncState = computed(() => (live.dashboardFailedAt ? 'error' : 'ok'));
const pageSyncLabel = computed(() => (live.dashboardFailedAt ? '页面同步中断，保留上次数据' : '页面已连接'));
const pageSyncDot = computed(() => (live.dashboardFailedAt ? 'failed' : 'connected'));
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

// ===== 实时互动事件（对齐 live.js pollEvents / renderEvents / renderHeatBar）=====

const eventFilter = ref<'user' | 'stats' | 'all'>('user');
const liveEvents = ref<any[]>([]);
const eventSeq = ref(0);
const eventRecordingId = ref<string | null>(null);
const eventStatus = ref('实时互动状态：等待检查');
let eventTimer: number | null = null;
let eventInFlight = false;
let eventRequestId = 0;

async function pollLiveEvents() {
  const session = currentSession.value;
  if (!live.selectedSource || !session || eventInFlight || document.visibilityState === 'hidden') return;
  const roomId = live.selectedSource.room_id;
  const sessionRecordingId = session.recording_id != null ? String(session.recording_id) : null;
  if (sessionRecordingId !== eventRecordingId.value) {
    eventRecordingId.value = sessionRecordingId;
    eventSeq.value = 0;
    liveEvents.value = [];
  }
  const requestId = ++eventRequestId;
  const recordingId = eventRecordingId.value;
  eventInFlight = true;
  try {
    const numeric = recordingId != null ? Number(recordingId) : NaN;
    const response: any = await liveApi.events(roomId, Number.isFinite(numeric) ? numeric : undefined, eventSeq.value, 100);
    if (requestId !== eventRequestId || roomId !== live.selectedSource?.room_id) return;
    const responseRecordingId = response?.recording_id != null ? String(response.recording_id) : null;
    if (responseRecordingId !== eventRecordingId.value) {
      eventRecordingId.value = responseRecordingId;
      eventSeq.value = 0;
      liveEvents.value = [];
    }
    const events = Array.isArray(response?.events) ? response.events : [];
    if (events.length) {
      if (response?.next_seq != null) eventSeq.value = Number(response.next_seq);
      liveEvents.value = [...liveEvents.value, ...events].slice(-100);
    }
    eventStatus.value = '实时互动状态：正常';
  } catch (e: any) {
    // 对齐老框架：409 录制实例切换只重置事件流，不改状态文案。
    if (e?.status === 409) {
      if (requestId === eventRequestId) {
        eventRecordingId.value = null;
        eventSeq.value = 0;
        liveEvents.value = [];
      }
      return;
    }
    eventStatus.value = `实时互动状态：暂时不可用（${e?.message || '未知错误'}），下方保留最近数据`;
    console.error('[live] 轮询互动事件失败：', e);
  } finally {
    eventInFlight = false;
  }
}

function startEventPolling() {
  if (eventTimer) return;
  eventTimer = window.setInterval(() => { void pollLiveEvents(); }, 2000);
}
function stopEventPolling() {
  if (eventTimer) clearInterval(eventTimer);
  eventTimer = null;
}

// ===== 事件渲染（分类 / 文案 / 礼物连击合并 / 热度条）=====

function eventCategory(event: any): 'user' | 'stats' | 'system' | 'unknown' {
  if (event.event_category) return event.event_category as 'user' | 'stats' | 'system' | 'unknown';
  const cmd = String(event.cmd || '').split(':', 1)[0];
  if (['danmaku', 'gift', 'super_chat', 'guard', 'interact', 'like', 'entry', 'link_mic_pk'].includes(event.event_type)) return 'user';
  if (['watched', 'stats'].includes(event.event_type)
    || ['WATCHED_CHANGE', 'ONLINE_RANK_V3', 'ONLINE_RANK_V2', 'ONLINE_RANK_TOP3', 'ONLINE_RANK_COUNT', 'LIKE_INFO_V3_UPDATE', 'ROOM_REAL_TIME_MESSAGE_UPDATE', 'HOT_RANK_CHANGED', 'AREA_RANK_CHANGED'].includes(cmd)) return 'stats';
  if (['INTERACT_WORD', 'INTERACT_WORD_V2', 'INTERACT_WORD_V3', 'WELCOME', 'WELCOME_GUARD', 'ENTRY_EFFECT', 'LIKE_INFO_V3_CLICK'].includes(cmd)) return 'user';
  if (['system', 'capture_gap'].includes(event.event_type)
    || ['LIVE', 'PREPARING', 'ROOM_CHANGE', 'ROOM_LOCK', 'ROOM_BLOCK_MSG', 'ROOM_SILENT_ON', 'ROOM_SILENT_OFF', 'CUT_OFF', 'STOP_LIVE_ROOM_LIST', 'NOTICE_MSG', 'COMMON_NOTICE_DANMAKU', 'WIDGET_BANNER'].includes(cmd)) return 'system';
  if (['VOICE_JOIN', 'LINK_MIC', 'LIVE_MULTI_VIEW'].some(prefix => cmd.startsWith(prefix)) || cmd.startsWith('PK_')) return 'user';
  return 'unknown';
}

function eventTypeLabel(event: any): string {
  const map: Record<string, string> = {
    danmaku: '弹幕', gift: '礼物', super_chat: 'SC', guard: '上舰',
    link_mic_pk: '连麦 / PK', interact: '进场', watched: '看过人数',
    like: '点赞', entry: '进场特效', stats: '统计', system: '系统', unknown: '未识别命令',
  };
  if (map[event.event_type]) return map[event.event_type];
  if (event.event_label) return String(event.event_label);
  const categoryMap: Record<string, string> = { user: '用户互动', stats: '统计', system: '系统', unknown: '未识别命令' };
  return categoryMap[eventCategory(event)] || '事件';
}

function eventText(event: any): string {
  const d = event.data || {};
  const type = event.event_type;
  if (event.display_text) return String(event.display_text);
  if (type === 'danmaku') return String(d.text || '');
  if (type === 'gift') return `${d.gift_name || '礼物'} ×${d.num || 1}`;
  if (type === 'super_chat') return `SC ¥${d.price || 0}：${d.message || ''}`;
  if (type === 'guard') return `上舰 等级 ${d.guard_level || '-'}`;
  if (d.text) return String(d.text);
  const category = eventCategory(event);
  if (category === 'system') return '系统事件';
  if (category === 'unknown') return `未知命令：${event.cmd || '空命令'}`;
  return '互动事件';
}

const displayEvents = computed(() => mergeLiveEvents(liveEvents.value).filter(event => {
  const category = eventCategory(event);
  return eventFilter.value === 'all' || category === eventFilter.value
    || (eventFilter.value === 'stats' && category === 'system');
}));

const scEvents = computed(() => liveEvents.value.filter(e => e.event_type === 'super_chat').slice(-3).reverse());

const heatBuckets = computed(() => {
  const buckets: number[] = [];
  for (const event of liveEvents.value) {
    if (event.event_type !== 'danmaku') continue;
    const index = Math.floor((event.media_time_ms || 0) / 30000);
    buckets[index] = (buckets[index] || 0) + 1;
  }
  return buckets;
});
// 对齐 live.js:732：filter(Boolean) 过滤稀疏数组空洞，避免 Math.max 展开出 NaN
const heatMax = computed(() => Math.max(1, ...heatBuckets.value.filter(Boolean)));
function heatClass(count: number | undefined) {
  const value = Number(count || 0);
  const ratio = value / heatMax.value;
  return [`live-heat-level-${value ? Math.min(3, Math.max(1, Math.ceil(ratio * 3))) : 0}`, { hot: ratio > 0.7 }];
}
function heatTitle(index: number, count: number | undefined) { return `${formatMediaTime(index * 30000)} · ${count || 0} 条弹幕`; }
function heatStyle(count: number | undefined) { return { height: `${Math.max(4, Number(count || 0) / heatMax.value * 100)}%` }; }
/** 热度条点击跳转到对应时间附近的事件（对齐 live.js）。 */
function onHeatClick(bucketStart: number) {
  const rows = [...document.querySelectorAll('.live-event-row[data-time-ms]')];
  const target = rows.find(row => Math.abs(Number((row as HTMLElement).dataset.timeMs || 0) - bucketStart) < 30000);
  target?.scrollIntoView({ behavior: 'smooth', block: 'center' });
}

// ===== 1s UI tick：录制时长实时更新（对齐 live.js tickUi）=====

const nowTick = ref(Date.now());
let tickTimer: number | null = null;
function startTick() {
  if (tickTimer) return;
  tickTimer = window.setInterval(() => { nowTick.value = Date.now(); }, 1000);
}
function stopTick() {
  if (!tickTimer) return;
  clearInterval(tickTimer);
  tickTimer = null;
}
function liveDurationText(r: any): string {
  if (r.started_at) return formatLiveDuration((nowTick.value - r.started_at * 1000) / 1000);
  return formatLiveDuration(r.duration_secs || 0);
}

// ===== 录制任务看板（子 tab）=====

type LiveBoard = 'recording' | 'history' | 'attention';
const subTab = ref<LiveBoard>('recording');

const recordingList = computed(() => [...live.recordings]
  .sort((a, b) => String(a.started_at ?? '').localeCompare(String(b.started_at ?? ''))));

const activeMergeJobs = computed(() => live.mergeJobs.filter(j => ['queued', 'running', 'cancelling'].includes(j.status)));
/** 「需要处理」计数（对齐老框架 renderBoard）：进行中的合并任务 + 可恢复项。 */
const attentionCount = computed(() => activeMergeJobs.value.length + live.recovery.length);

function mergeJobStatusText(status?: string): string {
  const map: Record<string, string> = { queued: '排队中', running: '进行中', cancelling: '取消中', completed: '已完成', failed: '失败', cancelled: '已取消' };
  return map[status || ''] || status || '—';
}

function historyBadgeClass(h: any): string {
  return h.status === 'failed' ? 'failed' : h.status === 'completed' ? 'completed' : '';
}
function historyBadgeText(h: any): string {
  return stopReasonText(h.stop_reason) || videoStatusText(h.status);
}

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
          <span class="live-sync-item" id="live-sync-page" :data-state="pageSyncState" title="本地页面与服务的连接状态">
            <span class="aria2-dot" :class="pageSyncDot"></span>{{ pageSyncLabel }}
          </span>
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
                  <span class="live-info-value">{{ localRoomInfo.parent_area_name || '' }}{{ localRoomInfo.area_name ? ' / ' + localRoomInfo.area_name : '' }}</span>
                </div>
                <div class="live-info-item">
                  <span class="live-info-label">开播时间</span>
                  <span class="live-info-value">{{ localRoomInfo.live_time ? new Date(localRoomInfo.live_time).toLocaleString() : '—' }}</span>
                </div>
                <div class="live-info-item">
                  <span class="live-info-label">清晰度</span>
                  <span class="live-info-value">{{ qualityText(live.selectedSource.quality || 10000) }}</span>
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
                      <i class="fa-solid fa-record-vinyl"></i> 录制中（{{ videoStatusText(localRoomInfo.recording_status) }}）
                    </span>
                    <span v-else-if="localRoomInfo.can_start" class="live-badge live">可开始录制</span>
                    <span v-else class="live-badge">未在录制</span>
                  </span>
                </div>
              </div>
              <!-- 与当前录制 session 关联的实时进度（来自 dashboard sessions） -->
              <div v-if="currentSession" class="live-room-info-progress">
                <div class="live-section-subtitle">录制实时进度</div>
                <div class="live-room-info-progress-row">
                  <div class="live-progress-line">
                    <span class="live-progress-label">已录制</span>
                    <span class="live-progress-value">{{ liveDurationText(currentSession) }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">文件大小</span>
                    <span class="live-progress-value">{{ formatFileSize(currentSession.file_size) }}</span>
                  </div>
                  <div class="live-progress-line">
                    <span class="live-progress-label">弹幕</span>
                    <span class="live-progress-value">{{ (currentSession.danmaku_count ?? 0).toLocaleString() }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">互动人数</span>
                    <span class="live-progress-value">{{ (currentSession.unique_user_count ?? 0).toLocaleString() }}</span>
                    <span v-if="(currentSession.peak_watched ?? 0) > 0" class="live-progress-sep">·</span>
                    <span v-if="(currentSession.peak_watched ?? 0) > 0" class="live-progress-label">累计看过</span>
                    <span v-if="(currentSession.peak_watched ?? 0) > 0" class="live-progress-value">{{ (currentSession.peak_watched ?? 0).toLocaleString() }}</span>
                  </div>
                  <div v-if="(currentSession.sc_count ?? 0) > 0 || (currentSession.guard_count ?? 0) > 0" class="live-progress-line">
                    <span class="live-progress-label">SC</span>
                    <span class="live-progress-value">{{ currentSession.sc_count ?? 0 }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">上舰</span>
                    <span class="live-progress-value">{{ currentSession.guard_count ?? 0 }}</span>
                    <span class="live-progress-sep">·</span>
                    <span class="live-progress-label">礼物价值</span>
                    <span class="live-progress-value">¥{{ (currentSession.estimated_paid_value ?? 0).toFixed(2) }}</span>
                  </div>
                  <div v-if="currentSession.error_msg" class="live-progress-error">
                    <i class="fa-solid fa-triangle-exclamation"></i> {{ currentSession.error_msg }}
                  </div>
                </div>
              </div>
            </div>

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
            <div v-if="currentSession && currentSession.interaction_capture_status !== 'off'" class="live-interaction-panel">
              <div class="live-section-title">
                <span><i class="fa-solid fa-comments"></i> 实时互动（最近 100 条）</span>
                <span class="form-note">观看为 B 站累计"看过"口径；互动人数按可识别 UID 估算；金额为非结算估算值。</span>
              </div>
              <div class="live-stats-grid">
                <div class="live-stats-cell"><strong>{{ currentSession.danmaku_count || 0 }}</strong><span>弹幕</span></div>
                <div class="live-stats-cell" title="可识别互动事件的去重 UID"><strong>{{ currentSession.unique_user_count || 0 }}</strong><span>互动人数</span></div>
                <div class="live-stats-cell"><strong>{{ currentSession.free_gift_count || 0 }}</strong><span>免费礼物</span></div>
                <div class="live-stats-cell"><strong>{{ currentSession.paid_gift_count || 0 }}</strong><span>付费礼物</span></div>
                <div class="live-stats-cell"><strong>{{ currentSession.sc_count || 0 }}</strong><span>SC</span></div>
                <div class="live-stats-cell"><strong>{{ currentSession.guard_count || 0 }}</strong><span>上舰</span></div>
                <div class="live-stats-cell" title="B 站累计口径，非在线峰值"><strong>{{ currentSession.peak_watched || 0 }}</strong><span>累计看过</span></div>
                <div class="live-stats-cell" title="非结算估算值"><strong>¥{{ Number(currentSession.estimated_paid_value || 0).toFixed(2) }}</strong><span>估算付费价值</span></div>
              </div>
              <div v-if="scEvents.length" class="live-sc-pins">
                <div v-for="(event, index) in scEvents" :key="`sc-${index}`" class="live-sc-card">
                  <strong>{{ event.data?.uname || 'SC' }}</strong><span>¥{{ event.data?.price || 0 }}</span><p>{{ event.data?.message || '' }}</p>
                </div>
              </div>
              <div class="live-toolbar-row">
                <select v-model="eventFilter" class="form-control" aria-label="互动类型筛选">
                  <option value="user">用户互动</option><option value="stats">系统统计</option><option value="all">全部事件</option>
                </select>
                <span class="live-events-status" role="status">{{ eventStatus }}</span>
              </div>
              <div class="live-heat-wrap">
                <div class="live-heat-heading">
                  <span><i class="fa-solid fa-chart-column"></i> 弹幕热度</span>
                  <span>每格 30 秒 · 点击柱体定位到对应互动</span>
                </div>
                <div class="live-heat-bar" aria-label="30 秒弹幕热度">
                  <span v-if="!heatBuckets.length" class="live-heat-empty">暂无弹幕热度数据</span>
                  <button v-for="(count, index) in heatBuckets" :key="index" type="button"
                          :class="heatClass(count)" :title="heatTitle(index, count)" :aria-label="heatTitle(index, count)"
                          :data-time-ms="index * 30000" @click="onHeatClick(index * 30000)">
                    <i :style="heatStyle(count)"></i>
                  </button>
                </div>
              </div>
              <div class="live-event-timeline">
                <p v-if="!displayEvents.length" class="empty-hint">暂无符合条件的互动</p>
                <div v-for="(event, index) in displayEvents.slice().reverse()" :key="index"
                     class="live-event-row" :class="`live-event-${eventCategory(event)}`"
                     :data-time-ms="event.media_time_ms || 0">
                  <time>{{ formatMediaTime(event.media_time_ms) }}</time>
                  <span class="live-event-user">{{ event.data?.uname || '' }}</span>
                  <span class="live-event-text">{{ eventText(event) }}</span>
                  <em v-if="(event.merged_count || 0) > 1" :title="`合并了 ${event.merged_count} 个连续事件`">×{{ event.merged_count }}</em>
                  <span v-else class="live-event-type">{{ eventTypeLabel(event) }}</span>
                </div>
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
        <div :class="['board-sub-tab', { active: subTab === 'recording' }]" data-live-board="recording" @click="subTab = 'recording'">
          录制中 <span class="board-tab-count" id="live-count-recording">{{ recordingList.length }}</span>
        </div>
        <div :class="['board-sub-tab', { active: subTab === 'history' }]" data-live-board="history" @click="subTab = 'history'">
          最近录制 <span class="board-tab-count" id="live-count-history">{{ live.history.length }}</span>
        </div>
        <div :class="['board-sub-tab', { active: subTab === 'attention' }]" data-live-board="attention" @click="subTab = 'attention'">
          需要处理 <span class="board-tab-count" id="live-count-attention">{{ attentionCount }}</span>
        </div>
      </div>
      <div :class="['live-board-panel', { active: subTab === 'recording' }]" id="live-panel-recording">
        <div id="live-recording-list">
          <p v-if="recordingList.length === 0" class="empty-hint">暂无录制中的任务</p>
          <template v-else>
            <div v-for="r in recordingList" :key="r.recording_id" class="live-recording-row live-recording-row-detailed">
              <div class="live-recording-main">
                <div class="live-recording-title">
                  <span class="live-room-name">{{ r.title || `房间 ${r.room_id}` }}</span>
                  <span :class="['live-badge', videoStatusClass(r.status)]">{{ videoStatusText(r.status) }}</span>
                  <span :class="['live-badge', interactionStateClass(r.interaction_capture_status || 'off')]">互动 {{ interactionStateText(r.interaction_capture_status || 'off') }}</span>
                  <span v-if="r.trigger" class="live-recording-trigger" :title="'触发方式：' + (r.trigger === 'auto' ? '自动触发' : '手动触发')">
                    <i class="fa-solid fa-bolt"></i> {{ r.trigger === 'auto' ? '自动触发' : '手动触发' }}
                  </span>
                  <span v-if="r.capture_mode" class="live-recording-mode" :title="'采集模式：' + captureModeText(r.capture_mode)">
                    <i class="fa-solid fa-camera"></i> {{ captureModeText(r.capture_mode) }}
                  </span>
                </div>
                <div class="live-recording-meta">
                  <span>#{{ r.room_id }}</span>
                  <span>时长 <b>{{ liveDurationText(r) }}</b></span>
                  <span><b>{{ formatFileSize(r.file_size) }}</b></span>
                  <span>{{ r.trigger === 'auto' ? '自动触发' : '手动触发' }}</span>
                  <span v-if="(r as any).dropped_event_count">丢失 {{ (r as any).dropped_event_count }} 条互动</span>
                </div>
                <div class="live-recording-progress">
                  <div class="progress">
                    <div class="bar" :style="{ width: Math.min(100, (r.duration_secs || 0) / 7200 * 100).toFixed(1) + '%' }"></div>
                  </div>
                  <div class="live-recording-progress-meta">
                    <span v-if="r.stream_protocol">流协议：<b>{{ r.stream_protocol }}</b></span>
                    <span v-if="r.stream_format">格式：<b>{{ r.stream_format }}</b></span>
                    <span v-if="r.stream_codec">编码：<b>{{ r.stream_codec }}</b></span>
                    <span v-if="r.stream_quality">清晰度：<b>{{ qualityText(r.stream_quality) }}</b></span>
                  </div>
                </div>
                <div v-if="(r.danmaku_count ?? 0) > 0 || (r.unique_user_count ?? 0) > 0 || (r.peak_watched ?? 0) > 0" class="live-recording-stats">
                  <span class="live-recording-stat"><i class="fa-solid fa-comments"></i> 弹幕 <b>{{ (r.danmaku_count ?? 0).toLocaleString() }}</b></span>
                  <span class="live-recording-stat"><i class="fa-solid fa-user-group"></i> 独立用户 <b>{{ (r.unique_user_count ?? 0).toLocaleString() }}</b></span>
                  <span v-if="(r.peak_watched ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-eye"></i> 峰值 <b>{{ (r.peak_watched ?? 0).toLocaleString() }}</b></span>
                  <span v-if="(r.sc_count ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-message-dollar"></i> SC <b>{{ r.sc_count ?? 0 }}</b></span>
                  <span v-if="(r.guard_count ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-shield-halved"></i> 上舰 <b>{{ r.guard_count ?? 0 }}</b></span>
                  <span v-if="(r.estimated_paid_value ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-coins"></i> 礼物 ¥<b>{{ (r.estimated_paid_value ?? 0).toFixed(2) }}</b></span>
                  <span v-if="r.danmu_unavailable" class="live-recording-stat warn"><i class="fa-solid fa-triangle-exclamation"></i> 弹幕采集异常</span>
                  <span v-if="['degraded', 'unavailable'].includes(r.interaction_capture_status || '')" class="live-recording-stat warn" :title="r.interaction_error || ''">
                    <i class="fa-solid fa-circle-exclamation"></i> 互动采集{{ interactionStateText(r.interaction_capture_status) }}
                  </span>
                </div>
                <div v-if="r.error_msg" class="live-recording-error">
                  <i class="fa-solid fa-triangle-exclamation"></i> {{ r.error_msg }}
                </div>
              </div>
              <div class="live-recording-actions">
                <button class="btn btn-sm btn-ghost" @click="viewRoomDetail(r.room_id)">查看详情</button>
                <button class="btn btn-sm btn-danger" data-mutating
                        :disabled="pendingRooms.has(r.room_id)"
                        @click="stopRecord(r.room_id)">
                  {{ pendingRooms.has(r.room_id) ? '处理中…' : '停止并合并' }}
                </button>
              </div>
            </div>
            <!-- 合并任务（数据来自 dashboard.merge_jobs，30s 轮询刷新） -->
            <div v-if="live.mergeJobs.length > 0" style="margin-top: 16px;">
              <div class="live-section-title"><i class="fa-solid fa-compress"></i> 合并任务</div>
              <table class="table">
                <thead><tr><th>任务 ID</th><th>状态</th><th>进度</th><th>操作</th></tr></thead>
                <tbody>
                  <tr v-for="job in live.mergeJobs" :key="job.id">
                    <td><code>{{ String(job.id).slice(0, 8) }}</code></td>
                    <td>{{ mergeJobStatusText(job.status) }}</td>
                    <td>{{ job.progress || 0 }}%</td>
                    <td>
                      <button v-if="['queued', 'running', 'cancelling'].includes(job.status) && !job.cancel_requested"
                              class="btn btn-sm" data-mutating @click="cancelMergeJob(String(job.id))">取消</button>
                      <span v-else-if="job.status === 'completed' && (job as any).output_path" class="tone-success">{{ (job as any).output_path }}</span>
                      <span v-else-if="job.status === 'failed'" class="tone-error">{{ job.error }}</span>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </template>
        </div>
      </div>
      <div :class="['live-board-panel', { active: subTab === 'history' }]" id="live-panel-history">
        <div id="live-history-list" class="live-history-list">
          <div v-if="live.historyError" class="live-alert error" role="alert">直播历史加载失败：{{ live.historyError }}</div>
          <p v-if="live.history.length === 0" class="empty-hint">暂无已结束的录制</p>
          <template v-else>
            <div v-for="(h, hi) in live.history" :key="h.recording_id || hi" class="live-recording-row">
              <div class="live-recording-main">
                <div class="live-recording-title">
                  <span class="live-room-name">{{ h.title || `房间 ${h.room_id}` }}</span>
                  <span v-if="h.status === 'failed'" class="live-badge failed">{{ h.is_recoverable ? '失败可恢复' : '失败' }}</span>
                  <span v-else :class="['live-badge', historyBadgeClass(h)]">{{ historyBadgeText(h) }}</span>
                  <span v-if="h.has_burned" class="live-badge completed">已有弹幕版</span>
                </div>
                <div class="live-recording-meta">
                  <span>#{{ h.room_id }}</span>
                  <span>{{ ((h as any).started_at_text || '').replace('T', ' ').slice(0, 16) }}</span>
                  <span>{{ formatLiveDuration(h.duration) }}</span>
                  <span>{{ h.file_size ? formatFileSize(h.file_size) : '--' }}</span>
                  <span v-if="h.segment_index">分段 {{ h.segment_index + 1 }} 个</span>
                  <span v-if="h.restart_attempts">重启 {{ h.restart_attempts }} 次</span>
                  <span v-if="h.error_msg">{{ h.error_msg }}</span>
                </div>
              </div>
              <div class="live-recording-actions">
                <button v-if="h.is_recoverable" class="btn btn-sm btn-primary" data-mutating
                        :disabled="mergePending.has(String(h.id))" @click="startMergeJob(String(h.id))">
                  {{ mergePending.has(String(h.id)) ? '创建中...' : '重试合并' }}
                </button>
                <button v-if="h.has_output && h.has_events && !h.has_burned" class="btn btn-sm btn-ghost" data-mutating
                        title="把录到的弹幕和 SC 烧录进视频，生成弹幕版" @click="burnHistoryDanmaku(h)">烧录弹幕</button>
                <button v-if="live.canOpenDirectory && h.has_output" class="btn btn-sm btn-ghost" data-mutating
                        @click="openRecording(h.id!)"><i class="fa-solid fa-folder-open"></i> 打开目录</button>
              </div>
            </div>
          </template>
        </div>
      </div>
      <div :class="['live-board-panel', { active: subTab === 'attention' }]" id="live-panel-attention">
        <div id="live-attention-list">
          <p v-if="attentionCount === 0" class="empty-hint">暂无需要处理的项目</p>
          <template v-else>
            <div v-for="job in activeMergeJobs" :key="`job-${job.id}`" class="live-recording-row">
              <div class="live-recording-main">
                <div class="live-recording-title">后台合并 · 录制 #{{ job.recording_id }} <span class="live-badge starting">{{ job.status === 'cancelling' ? '取消中' : '进行中' }}</span></div>
                <div class="live-recording-meta"><span>任务 {{ String(job.id).slice(0, 8) }}</span><span>源分段 {{ job.source_segment_count || '-' }} 个</span></div>
              </div>
              <div class="live-recording-actions">
                <progress class="live-progress" max="100" :value="job.progress || 0"></progress>
                <span class="live-progress-text">{{ job.progress || 0 }}%</span>
                <button class="btn btn-sm btn-ghost" data-mutating :disabled="!!job.cancel_requested" @click="cancelMergeJob(String(job.id))">取消</button>
              </div>
            </div>
            <div v-for="r in live.recovery" :key="`recovery-${r.recording_id}`" class="live-recording-row">
              <div class="live-recording-main">
                <div class="live-recording-title">#{{ r.recording_id }} {{ r.title || '' }} <span class="live-badge failed">失败可恢复</span></div>
                <div class="live-recording-meta"><span>保留源分段 {{ r.segment_count || 0 }} 个</span><span>{{ r.error_msg || '可恢复' }}</span></div>
              </div>
              <div class="live-recording-actions">
                <button class="btn btn-sm btn-primary" data-mutating
                        :disabled="mergePending.has(String(r.recording_id))" @click="startMergeJob(r.recording_id)">
                  {{ mergePending.has(String(r.recording_id)) ? '创建中...' : '重试合并' }}
                </button>
                <button v-if="live.canOpenDirectory && r.has_output" class="btn btn-sm btn-ghost" data-mutating
                        @click="openRecording(r.recording_id)">打开目录</button>
              </div>
            </div>
          </template>
        </div>
      </div>
    </div>

    <!-- 添加直播源弹窗：与原版 live-add-modal 1:1 同构 -->
    <div v-if="showAdd" ref="addModalRoot" id="live-add-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="live-add-title" @click.self="closeAddModal">
      <div class="modal-container">
        <div class="modal-header">
          <i class="fa-solid fa-satellite-dish"></i>
          <span id="live-add-title">添加直播源</span>
          <button type="button" class="modal-close-btn" id="live-add-close-btn" aria-label="关闭" @click="closeAddModal">
            <i class="fa-solid fa-times"></i>
          </button>
        </div>
        <div class="form-section">
          <div class="form-group form-full">
            <label for="live-add-input"><i class="fa-solid fa-link"></i> 房间号或直播链接（支持批量粘贴）</label>
            <textarea id="live-add-input" v-model="addRoomInput" class="form-control live-add-textarea" rows="3"
                      placeholder="例如 123456 或 https://live.bilibili.com/123456，多个房间用换行 / 空格 / 逗号分隔"
                      @keydown.enter.exact.prevent="confirmAdd"></textarea>
            <div class="form-note">添加后默认关闭自动录制；请在房间详情的"设置"中开启自动录制并配置周排期。短号会自动解析为长号。</div>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" id="live-add-cancel-btn" @click="closeAddModal">
            <i class="fa-solid fa-times"></i> 取消
          </button>
          <button class="btn btn-primary" id="live-add-confirm-btn" data-network-required="true" data-mutating :disabled="adding" @click="confirmAdd">
            <i :class="adding ? 'fa-solid fa-spinner fa-spin' : 'fa-solid fa-check'"></i> {{ adding ? '添加中…' : '查询并添加' }}
          </button>
        </div>
      </div>
    </div>

    <!-- 直播源设置弹窗：与旧版保持独立弹窗和周排期编辑契约。 -->
    <div v-if="showSettings && live.selectedSource" ref="settingsModalRoot" id="live-source-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="live-settings-title" @click.self="closeSettingsModal">
      <div class="modal-container modal-container-wide">
        <div class="modal-header">
          <i class="fa-solid fa-sliders"></i>
          <span id="live-settings-title">直播源设置 · 房间 <span id="live-settings-room-id">{{ live.selectedSource.room_id }}</span></span>
          <button type="button" class="modal-close-btn" id="live-settings-close-btn" aria-label="关闭" @click="closeSettingsModal"><i class="fa-solid fa-times"></i></button>
        </div>
        <div class="form-section">
          <input type="hidden" id="live-source-room-id" :value="live.selectedSource.room_id" />
          <div class="form-group">
            <label class="choice-row" for="live-source-auto">
              <span>自动录制（检测到开播且在排期窗口内时自动开始）</span>
              <span class="toggle-switch"><input id="live-source-auto" v-model="settingsAutoRecord" type="checkbox" /><span class="slider"></span></span>
            </label>
          </div>
          <div class="form-group">
            <label for="live-source-mode"><i class="fa-solid fa-comments"></i> 互动采集</label>
            <select id="live-source-mode" v-model="settingsCaptureMode" class="form-control">
              <option value="standard">标准（推荐，弹幕/礼物/SC/上舰）</option>
              <option value="full">完整原始数据（含全部未知命令）</option>
              <option value="off">关闭（仅录制视频）</option>
            </select>
          </div>
          <div class="form-group">
            <label for="live-source-quality"><i class="fa-solid fa-film"></i> 清晰度上限</label>
            <select id="live-source-quality" v-model.number="settingsQuality" class="form-control">
              <option :value="10000">原画（10000，推荐）</option>
              <option :value="400">蓝光（400）</option>
              <option :value="250">超清（250）</option>
              <option :value="150">高清（150）</option>
              <option :value="80">流畅（80）</option>
            </select>
            <div class="form-note">实际清晰度受账号权限限制可能被 B 站降级，录制卡片会显示实际拿到的清晰度。</div>
          </div>
          <div class="form-group form-full live-schedule-mode-group">
            <label><i class="fa-solid fa-calendar-week"></i> 自动录制时间策略</label>
            <div class="live-schedule-mode" role="radiogroup" aria-label="自动录制时间策略">
              <label class="live-schedule-mode-option">
                <input id="live-source-schedule-all-day" v-model="settingsScheduleMode" type="radio" value="all-day" />
                <span><strong>全天允许</strong><small>开播后随时自动开始</small></span>
              </label>
              <label class="live-schedule-mode-option">
                <input id="live-source-schedule-weekly" v-model="settingsScheduleMode" type="radio" value="weekly" />
                <span><strong>按周排期</strong><small>只在下方时间窗口内自动开始</small></span>
              </label>
            </div>
          </div>
          <div class="form-group form-full">
            <label><i class="fa-solid fa-calendar-week"></i> 自动录制周排期 <span class="form-label-muted">每天最多 2 个时段，支持跨天如 22:00-02:00</span></label>
            <div id="live-tz-note" class="live-tz-note">
              排期按服务器时区生效{{ live.serverTimezone ? `：${live.serverTimezone}` : '' }}{{ live.serverNow ? ` · 服务器当前时间 ${new Date(live.serverNow).toLocaleTimeString('zh-CN', { hour12: false })}` : '' }}
            </div>
            <div id="live-weekly-schedule" class="live-schedule-grid">
              <div class="live-schedule-header" aria-hidden="true"><span>星期</span><span>时段 1</span><span>时段 2</span></div>
              <div v-for="([day, label]) in weekdays" :key="day" class="live-schedule-day">
                <span>{{ label }}</span>
                <div v-for="(window, index) in settingsSchedule[day]" :key="`${day}-${index}`" class="live-schedule-window">
                  <input :id="`live-schedule-${day}-${index}-start`" v-model="window[0]" type="time" :disabled="settingsScheduleMode === 'all-day'" :aria-label="`${label} 时段 ${index + 1} 开始`" />
                  <span aria-hidden="true">–</span>
                  <input :id="`live-schedule-${day}-${index}-end`" v-model="window[1]" type="time" :disabled="settingsScheduleMode === 'all-day'" :aria-label="`${label} 时段 ${index + 1} 结束`" />
                  <button type="button" class="live-schedule-clear" data-mutating :disabled="settingsScheduleMode === 'all-day'" :aria-label="`清空${label}时段${index + 1}`" title="清空该时段" @click="window[0] = ''; window[1] = ''"><i class="fa-solid fa-xmark"></i></button>
                </div>
              </div>
            </div>
            <div id="live-schedule-error" class="live-schedule-error" role="alert">{{ scheduleError }}</div>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" id="live-settings-cancel-btn" @click="closeSettingsModal"><i class="fa-solid fa-times"></i> 取消</button>
          <button class="btn btn-primary" id="live-source-save" data-network-required="true" data-mutating :disabled="savingSettings" @click="saveSettingsModal"><i class="fa-solid fa-check"></i> {{ savingSettings ? '保存中…' : '保存设置' }}</button>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
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
</style>
