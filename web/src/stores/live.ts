/**
 * 直播 store：保留迁移前已验证的行为语义。
 * - 写操作一律不带 expected_version（老框架 apiPost 全部不传，不引入 409 冲突路径）。
 * - 写操作走本地 postFull 封装，透出后端 message 供 toast 优先消费（P3-13）。
 * - refreshDashboard 成功后顺带拉 /api/live/history?limit=30（老框架同款调用时机），
 *   并在选中房间失效时自动选中第一个房间（老框架 refreshDashboard 尾部逻辑）。
 * - 所有 action 内部自带 try/catch，**不向调用者抛 promise reject**。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { postFull } from '@/api/client';
import { live as liveApi } from '@/api';
import type { LiveRecording, LiveSource, LiveRoom, LiveMergeJob } from '@/api/types';

// ===== 文案 / 格式化映射（对齐 live.js 顶部，供组件复用）=====

export const LIVE_WEEKDAYS: ReadonlyArray<readonly [string, string]> = [
  ['mon', '周一'], ['tue', '周二'], ['wed', '周三'], ['thu', '周四'],
  ['fri', '周五'], ['sat', '周六'], ['sun', '周日'],
];

/** 录制 session 状态（后端 RecordingStatus 8 态，src/services/live_recorder/state.rs）。 */
export function videoStatusText(status?: string): string {
  const map: Record<string, string> = {
    starting: '启动中',
    recording: '录制中',
    stopping: '停止中',
    finalizing: '收尾合并中',
    stopped: '已停止',
    completed: '已完成',
    failed: '失败',
    cancelled: '已取消',
  };
  return map[status || ''] || '未知';
}

export function videoStatusClass(status?: string): string {
  if (status === 'recording') return 'recording';
  if (['starting', 'stopping', 'finalizing'].includes(status || '')) return 'starting';
  if (status === 'failed') return 'failed';
  if (['stopped', 'completed'].includes(status || '')) return 'completed';
  return '';
}

export function interactionStateText(state?: string): string {
  const map: Record<string, string> = {
    off: '关闭',
    connecting: '连接中',
    capturing: '采集中',
    degraded: '已降级',
    unavailable: '不可用',
    completed: '已完成',
  };
  return map[state || ''] || state || '关闭';
}

export function interactionStateClass(state?: string): string {
  if (state === 'capturing') return 'capturing';
  if (state === 'connecting') return 'connecting';
  if (state === 'degraded') return 'degraded';
  if (state === 'unavailable') return 'unavailable';
  if (state === 'completed') return 'completed';
  return '';
}

export function captureModeText(mode?: string): string {
  const map: Record<string, string> = { standard: '标准', full: '完整原始数据', off: '关闭' };
  return map[mode || ''] || mode || '标准';
}

export function qualityText(qn?: number): string {
  if (!qn) return '';
  const map: Record<number, string> = { 10000: '原画', 400: '蓝光', 250: '超清', 150: '高清', 80: '流畅' };
  return map[qn] ? `${map[qn]} (${qn})` : String(qn);
}

export function stopReasonText(reason?: string): string {
  const map: Record<string, string> = {
    manual_stop: '手动停止',
    stream_ended_after_offline_confirmation: '自然下播',
    ffmpeg_exit_while_live_or_unconfirmed: 'FFmpeg 异常退出',
    recording_failed: '录制失败',
    recording_completed: '已完成',
  };
  return map[reason || ''] || '';
}

/** 房间源状态（对齐 live.js sourceState）：返回 [badge class, 中文文案]。 */
export function sourceState(source: LiveSource): [string, string] {
  const runtime = (source as any).runtime || {};
  if (runtime.risk_limited) return ['risk', 'B站检查受限'];
  if (runtime.stale) return ['stale', '状态已过期'];
  if (runtime.error) return ['unknown', '状态未知'];
  if (runtime.live_status === 1) return ['live', '直播中'];
  if (runtime.live_status === 2) return ['live', '轮播中'];
  if (runtime.live_status == null) return ['waiting', '等待首次检查'];
  return ['offline', '未开播'];
}

export function formatLiveDuration(value = 0): string {
  const seconds = Math.max(0, Math.floor(value));
  return [Math.floor(seconds / 3600), Math.floor((seconds % 3600) / 60), seconds % 60]
    .map(v => String(v).padStart(2, '0'))
    .join(':');
}

export function formatMediaTime(ms = 0): string {
  return formatLiveDuration(Math.floor(ms / 1000));
}

export function formatFileSize(bytes = 0): string {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  if (!bytes) return '0 B';
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), 4);
  return `${(bytes / 1024 ** i).toFixed(1)} ${units[i]}`;
}

export function relativeTime(rfc3339?: string | null): string {
  if (!rfc3339) return '尚未';
  const time = Date.parse(rfc3339);
  if (Number.isNaN(time)) return rfc3339;
  const delta = Math.max(0, Math.floor((Date.now() - time) / 1000));
  if (delta < 60) return `${delta} 秒前`;
  if (delta < 3600) return `${Math.floor(delta / 60)} 分钟前`;
  return `${Math.floor(delta / 3600)} 小时前`;
}

export function scheduleSummary(source: LiveSource): string {
  if (source.schedule_all_day) return '全天自动';
  const schedule = source.weekly_schedule || {};
  const parts = LIVE_WEEKDAYS
    .filter(([key]) => (schedule[key] || []).length)
    .map(([key, label]) => `${label} ${(schedule[key] || []).join('、')}`);
  return parts.length ? `按周排期：${parts.join('；')}` : '排期为空（永不自动开始）';
}

/** 添加房间输入解析（对齐 live.js parseRoomTokens 逐字移植）。 */
export function parseRoomTokens(raw: string): number[] {
  const tokens = raw.split(/[\s,，、;；]+/).filter(Boolean);
  const roomIds = new Set<number>();
  for (const token of tokens) {
    const match = token.match(/live\.bilibili\.com\/(?:h5\/)?(\d+)/i) || token.match(/^(\d+)$/);
    if (match) roomIds.add(Number(match[1]));
  }
  return [...roomIds].filter(id => Number.isSafeInteger(id) && id > 0);
}

export function dayLabel(key: string): string {
  return LIVE_WEEKDAYS.find(([value]) => value === key)?.[1] || key;
}

/** 跨天排期重叠校验（对齐 live.js validateScheduleStrict 逐字移植）。返回空串表示通过。 */
export function validateScheduleStrict(schedule: Record<string, string[]>): string {
  const intervals: Array<{ begin: number; finish: number }> = [];
  const toMinutes = (value: string): number => {
    if (!/^\d{2}:\d{2}$/.test(value)) return NaN;
    // 解构缺位时给 NaN：NaN 比较恒为 false，等价"格式非法"。
    const [hour = NaN, minute = NaN] = value.split(':').map(Number);
    return hour < 24 && minute < 60 ? hour * 60 + minute : NaN;
  };
  for (const [day, windows] of Object.entries(schedule)) {
    const dayIndex = LIVE_WEEKDAYS.findIndex(([key]) => key === day);
    for (const value of windows) {
      const [start = '', end = ''] = value.split('-');
      const beginValue = toMinutes(start);
      const endValue = toMinutes(end);
      if (dayIndex < 0 || !Number.isFinite(beginValue) || !Number.isFinite(endValue) || beginValue === endValue) {
        return `${dayLabel(day)}: 时间格式应为 HH:MM-HH:MM，且开始和结束不能相同`;
      }
      const begin = dayIndex * 1440 + beginValue;
      let finish = dayIndex * 1440 + endValue;
      if (finish <= begin) finish += 1440;
      [-10080, 0, 10080].forEach(offset => intervals.push({ begin: begin + offset, finish: finish + offset }));
    }
  }
  intervals.sort((left, right) => left.begin - right.begin);
  for (let index = 1; index < intervals.length; index += 1) {
    const previous = intervals[index - 1];
    const current = intervals[index];
    if (previous && current && previous.finish > current.begin) return '排期窗口存在重叠，请调整后再保存';
  }
  return '';
}

/** 礼物连击合并展示（对齐 live-contract.js mergeLiveEvents，不改归档数据）。 */
export function mergeLiveEvents(events: any[] = []): any[] {
  const merged: any[] = [];
  for (const event of events) {
    const previous = merged.at(-1);
    const sameGift = event.event_type === 'gift' && previous?.event_type === 'gift'
      && event.data?.uid === previous.data?.uid && event.data?.gift_name === previous.data?.gift_name;
    const freeGiftBucket = sameGift && event.data?.coin_type !== 'gold' && previous.data?.coin_type !== 'gold';
    const windowMs = freeGiftBucket ? 5000 : 2000;
    if (sameGift && (event.media_time_ms - previous.media_time_ms) <= windowMs) {
      previous.merged_count = (previous.merged_count || 1) + 1;
      previous.data = { ...previous.data, num: Number(previous.data?.num || 0) + Number(event.data?.num || 0) };
    } else merged.push({ ...event, data: { ...(event.data || {}) }, merged_count: 1 });
  }
  return merged;
}

// ===== 后端数据映射 =====

/** 把后端 RecordingInfo 序列化的 session 节点转成 UI 用的 LiveRecording。
 *  started_at 是 RFC3339 字符串，转换成 unix 秒便于时长实时 tick 复用。 */
function toUnixSeconds(value: any): number | undefined {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value > 10_000_000_000 ? Math.floor(value / 1000) : Math.floor(value);
  }
  if (typeof value !== 'string' || !value) return undefined;
  const timestamp = new Date(value).getTime();
  return Number.isFinite(timestamp) ? Math.floor(timestamp / 1000) : undefined;
}

function fromBackendSession(s: any): LiveRecording {
  const startedAtUnix = toUnixSeconds(s.started_at);
  return {
    recording_id: s.recording_id != null ? String(s.recording_id) : `room-${s.room_id}`,
    room_id: Number(s.room_id),
    uname: s.title || undefined,
    title: s.title,
    started_at: startedAtUnix,
    status: (typeof s.status === 'string' ? s.status : undefined) as LiveRecording['status'],
    // 后端真正给的录制进度字段（session 无 segment_count，recovery 才有）：
    duration_secs: Number(s.duration_secs ?? 0),
    file_size: Number(s.file_size ?? 0),
    danmaku_count: Number(s.danmaku_count ?? 0),
    unique_user_count: Number(s.unique_user_count ?? 0),
    free_gift_count: Number(s.free_gift_count ?? 0),
    paid_gift_count: Number(s.paid_gift_count ?? 0),
    sc_count: Number(s.sc_count ?? 0),
    guard_count: Number(s.guard_count ?? 0),
    peak_watched: Number(s.peak_watched ?? 0),
    estimated_paid_value: Number(s.estimated_paid_value ?? 0),
    error_msg: s.error_msg || undefined,
    stream_quality: s.stream_quality ?? undefined,
    stream_protocol: s.stream_protocol || undefined,
    stream_format: s.stream_format || undefined,
    stream_codec: s.stream_codec || undefined,
    capture_mode: s.capture_mode || undefined,
    trigger: s.trigger || undefined,
    interaction_capture_status: s.interaction_capture_status || undefined,
    interaction_error: s.interaction_error || undefined,
    danmu_unavailable: !!s.danmu_unavailable,
    has_output: !!s.has_output,
    can_open_directory: !!s.can_open_directory,
    has_events: !!s.has_events,
    has_burned: !!s.has_burned,
    is_recoverable: !!s.is_recoverable,
    segment_index: s.segment_index,
    restart_attempts: s.restart_attempts,
    // 扩展字段（LiveRecording 类型外的原始字段，组件用 as any 消费）：
    dropped_event_count: s.dropped_event_count,
    started_at_text: s.started_at,
  } as LiveRecording;
}

/** /api/live/history 与 dashboard.recovery 是数据库历史视图，字段与 RecordingInfo 不同。 */
function fromBackendHistory(s: any): LiveRecording {
  const recording = fromBackendSession({
    ...s,
    recording_id: s.id ?? s.recording_id,
    duration_secs: s.duration_secs ?? s.duration,
    interaction_capture_status: s.interaction_status,
  });
  return {
    ...recording,
    id: Number(s.id ?? 0),
    recording_id: String(s.id ?? s.recording_id ?? ''),
    uname: s.uname ?? s.title,
    duration: Number(s.duration ?? s.duration_secs ?? 0),
    size: Number(s.file_size ?? s.size ?? 0),
    file_size: Number(s.file_size ?? s.size ?? 0),
    ended_at: s.ended_at,
    has_output: !!s.has_output,
    can_open_directory: !!s.can_open_directory,
    has_events: !!s.has_events,
    has_burned: !!s.has_burned,
    // 历史视图独有：停止原因（文案映射用）；recovery 项独有：segment_count（真实存在）。
    stop_reason: s.stop_reason || undefined,
    segment_count: s.segment_count,
  } as LiveRecording;
}

export const useLiveStore = defineStore('live', () => {
  const sources = ref<LiveSource[]>([]);
  const recordings = ref<LiveRecording[]>([]);
  const history = ref<LiveRecording[]>([]);
  const mergeJobs = ref<LiveMergeJob[]>([]);
  const recovery = ref<LiveRecording[]>([]);
  const selectedSourceId = ref<number | null>(null);
  const selectedRecordingId = ref<string | null>(null);
  const monitorRunning = ref<boolean | null>(null);
  const diskFree = ref('');
  const lastHeartbeatAt = ref<string | null>(null);
  const lastSuccessAt = ref<string | null>(null);
  const monitorError = ref<string | null>(null);
  const dashboardError = ref<string | null>(null);
  /** 老框架 liveState.dashboardFailedAt：失败后保留上次数据并显示“页面同步中断”。 */
  const dashboardFailedAt = ref(0);
  /** 首次成功加载标志：侧边栏骨架 / 空态切换用。 */
  const dashboardLoaded = ref(false);
  /** 老框架 renderRiskNotice 数据源：dashboard.risk_notice（非 WS 风控）。 */
  const riskNotice = ref<string | null>(null);
  /** 老框架打开目录按钮条件：dashboard.can_open_directory。 */
  const canOpenDirectory = ref(false);
  const historyError = ref<string | null>(null);
  const roomInfoError = ref<string | null>(null);
  const serverNow = ref('');
  const serverTimezone = ref('');
  const loading = ref(false);
  let dashboardRequest: Promise<boolean> | null = null;
  /** 选中房间的实时信息缓存：room_id -> LiveRoom。 */
  const roomInfoCache = ref<Map<number, LiveRoom>>(new Map());
  const roomInfoCachedAt = ref<Map<number, number>>(new Map());
  /** 房间实时信息加载中（防重入）。 */
  const roomInfoLoading = ref<Set<number>>(new Set());

  const selectedSource = computed(() => sources.value.find(s => s.id === selectedSourceId.value) || null);
  const selectedRecording = computed(() => recordings.value.find(r => r.recording_id === selectedRecordingId.value) || null);
  const selectedRoomInfo = computed<LiveRoom | null>(() => {
    if (!selectedSource.value) return null;
    return roomInfoCache.value.get(selectedSource.value.room_id) || null;
  });

  async function refreshDashboard(): Promise<boolean> {
    if (dashboardRequest) return dashboardRequest;
    const request = (async () => {
      loading.value = true;
      const d: any = await liveApi.dashboard();
      if (d) {
        // 后端字段：sources (含 runtime)、sessions (录制中的)、monitor、risk_notice、
        //   can_open_directory、synced_at、server_now、server_timezone、poll_interval_secs:30、
        //   merge_jobs、recovery、disk { available_bytes, total_bytes, path_hidden }
        sources.value = (d.sources || []).map((s: any) => ({
          id: s.id,
          version: Number.isFinite(Number(s.version)) ? Number(s.version) : undefined,
          room_id: s.room_id,
          short_id: s.short_id,
          manual_stop_latched: !!s.manual_stop_latched,
          uid: s.uid,
          uname: s.anchor_name,
          face: s.face,
          title: s.title,
          cover: s.cover,
          live_status: s.runtime?.live_status ?? 0,
          enabled: s.auto_record_enabled,
          auto_record: s.auto_record_enabled,
          quality: s.max_qn,
          capture_mode: s.capture_mode,
          schedule_all_day: s.schedule_all_day,
          weekly_schedule: s.weekly_schedule,
          // 原始 runtime 节点也保留（risk_limited / stale / live_status / last_checked_at 等）
          runtime: s.runtime,
        })) as LiveSource[];
        recordings.value = (d.sessions || []).map(fromBackendSession);
        mergeJobs.value = Array.isArray(d.merge_jobs) ? d.merge_jobs : [];
        recovery.value = Array.isArray(d.recovery) ? d.recovery.map(fromBackendHistory) : [];
        // 磁盘余量：对齐老框架 renderDiskHint 的「可用 / 总量」双值展示。
        const disk = d.disk || {};
        diskFree.value = typeof disk.available_bytes === 'number'
          ? `${formatFileSize(disk.available_bytes)} / ${formatFileSize(disk.total_bytes || 0)}`
          : '';
        const monitor = d.monitor || {};
        monitorRunning.value = typeof monitor.running === 'boolean' ? monitor.running : null;
        lastHeartbeatAt.value = monitor.last_heartbeat_at ?? null;
        lastSuccessAt.value = monitor.last_success_at ?? null;
        monitorError.value = monitor.last_error ?? null;
        dashboardError.value = null;
        dashboardFailedAt.value = 0;
        dashboardLoaded.value = true;
        riskNotice.value = d.risk_notice || null;
        canOpenDirectory.value = !!d.can_open_directory;
        serverNow.value = d.server_now || '';
        serverTimezone.value = d.server_timezone || '';
        // 老框架：选中房间不在 sessions / sources 时自动选第一个（silent）。
        const selectedRoomId = selectedSource.value?.room_id ?? null;
        if (!recordings.value.some(item => item.room_id === selectedRoomId)
          && !sources.value.some(item => item.room_id === selectedRoomId)) {
          const nextRoom = recordings.value[0]?.room_id || sources.value[0]?.room_id || 0;
          const nextSource = sources.value.find(item => item.room_id === nextRoom) || null;
          selectedSourceId.value = nextSource ? nextSource.id : null;
        }
        // 老框架 refreshDashboard 成功后顺带拉最近录制（失败静默，不弹 toast）。
        await refreshHistory();
        return true;
      }
      dashboardError.value = '后端返回了空的直播状态';
      dashboardFailedAt.value = dashboardFailedAt.value || Date.now();
      return false;
    })().catch((error) => {
      dashboardError.value = error instanceof Error ? error.message : '直播状态刷新失败';
      dashboardFailedAt.value = dashboardFailedAt.value || Date.now();
      return false;
    }).finally(() => { loading.value = false; });
    dashboardRequest = request;
    try {
      return await request;
    } finally {
      if (dashboardRequest === request) dashboardRequest = null;
    }
  }

  async function refreshHistory(limit = 30) {
    try {
      const r = await liveApi.history(limit);
      if (r && Array.isArray((r as any).items)) {
        history.value = (r as any).items.map(fromBackendHistory);
        historyError.value = null;
      }
    } catch (error) {
      historyError.value = error instanceof Error ? error.message : '直播历史加载失败';
    }
  }

  async function roomInfo(room_id: number, opts: { force?: boolean } = {}): Promise<LiveRoom | null> {
    if (!room_id || room_id <= 0) return null;
    const cachedAt = roomInfoCachedAt.value.get(room_id) || 0;
    if (!opts.force && roomInfoCache.value.has(room_id) && Date.now() - cachedAt < 5 * 60 * 1000) {
      return roomInfoCache.value.get(room_id)!;
    }
    if (roomInfoLoading.value.has(room_id)) return null;
    roomInfoLoading.value.add(room_id);
    roomInfoLoading.value = new Set(roomInfoLoading.value);
    try {
      const r = (await liveApi.roomInfo(room_id)) as unknown as LiveRoom | null;
      if (r) {
        roomInfoCache.value.set(room_id, r);
        roomInfoCache.value = new Map(roomInfoCache.value);
        roomInfoCachedAt.value.set(room_id, Date.now());
        roomInfoCachedAt.value = new Map(roomInfoCachedAt.value);
      }
      roomInfoError.value = null;
      return r;
    } catch (error) {
      roomInfoError.value = error instanceof Error ? error.message : '直播间信息加载失败';
      return null;
    }
    finally {
      roomInfoLoading.value.delete(room_id);
      roomInfoLoading.value = new Set(roomInfoLoading.value);
    }
  }

  // ===== 写操作：本地 postFull 封装，一律不传 expected_version（对齐老框架 apiPost body）=====

  /** 添加直播源：固定默认值（自动录制关、标准采集），不自动刷新（调用方循环结束后统一刷）。 */
  async function addSource(room_id: number) {
    return postFull<any>('/api/live/source/add', {
      room_id,
      auto_record_enabled: false,
      capture_mode: 'standard',
    });
  }

  async function updateSource(id: number, patch: Partial<LiveSource> & Record<string, any>) {
    const source = sources.value.find(x => x.id === id);
    const room_id = source?.room_id;
    if (!room_id) throw new Error('未找到直播源');
    const payload: any = {
      room_id,
      auto_record_enabled: patch.auto_record ?? patch.enabled,
      max_qn: patch.quality,
      capture_mode: patch.capture_mode,
      clear_schedule: patch.clear_schedule,
      weekly_schedule: patch.weekly_schedule,
    };
    Object.keys(payload).forEach(k => payload[k] === undefined && delete payload[k]);
    const r = await postFull<any>('/api/live/source/update', payload);
    await refreshDashboard();
    return r;
  }

  async function deleteSource(id: number) {
    const source = sources.value.find(x => x.id === id);
    if (!source) throw new Error('未找到直播源');
    const r = await postFull<any>('/api/live/source/delete', { room_id: source.room_id });
    if (selectedSourceId.value === id) selectedSourceId.value = null;
    await refreshDashboard();
    return r;
  }

  async function startRecording(room_id: number) {
    const r = await postFull<any>('/api/live/start', { room_id });
    await refreshDashboard();
    return r;
  }

  async function stopRecording(room_id: number) {
    const r = await postFull<any>('/api/live/stop', { room_id });
    await refreshDashboard();
    return r;
  }

  /** 重试合并（老框架 history-merge）：无成功 toast，错误文案由组件处理。 */
  async function startMerge(recording_id: string | number) {
    return postFull<any>(`/api/live/history/${encodeURIComponent(String(recording_id))}/merge`, {});
  }

  async function cancelMerge(job_id: string) {
    return postFull<any>(`/api/live/merge/${encodeURIComponent(job_id)}/cancel`, {});
  }

  async function burnDanmaku(recording_id: string | number) {
    return postFull<any>(`/api/live/history/${encodeURIComponent(String(recording_id))}/burn-danmaku`, {});
  }

  /** 打开历史录制的本地目录：调 /api/live/history/{id}/open-directory（老框架无成功 toast）。 */
  async function openRecording(recordingId: number | string) {
    const id = Number(recordingId);
    if (!Number.isFinite(id)) throw new Error('录制记录 ID 无效');
    return postFull<any>(`/api/live/history/${id}/open-directory`, {});
  }

  function selectSource(id: number | null) { selectedSourceId.value = id; }
  function selectRecording(id: string | null) { selectedRecordingId.value = id; }

  /** 设备会话 401 失效时清空本会话的直播数据（app store 调用）。 */
  function reset() {
    sources.value = [];
    recordings.value = [];
    history.value = [];
    mergeJobs.value = [];
    recovery.value = [];
    selectedSourceId.value = null;
    selectedRecordingId.value = null;
    monitorRunning.value = null;
    diskFree.value = '';
    lastHeartbeatAt.value = null;
    lastSuccessAt.value = null;
    monitorError.value = null;
    dashboardError.value = null;
    dashboardFailedAt.value = 0;
    dashboardLoaded.value = false;
    riskNotice.value = null;
    canOpenDirectory.value = false;
    historyError.value = null;
    roomInfoError.value = null;
    roomInfoCache.value = new Map();
    roomInfoCachedAt.value = new Map();
    roomInfoLoading.value = new Set();
  }

  return {
    sources, recordings, history, mergeJobs, recovery, selectedSourceId, selectedRecordingId,
    monitorRunning, diskFree, lastHeartbeatAt, lastSuccessAt, monitorError,
    dashboardError, dashboardFailedAt, dashboardLoaded, riskNotice, canOpenDirectory,
    historyError, roomInfoError, serverNow, serverTimezone, loading,
    roomInfoCache, roomInfoCachedAt, roomInfoLoading,
    selectedSource, selectedRecording, selectedRoomInfo,
    refreshDashboard, refreshHistory,
    roomInfo, addSource, updateSource, deleteSource,
    startRecording, stopRecording, startMerge, cancelMerge, burnDanmaku,
    openRecording, selectSource, selectRecording, reset,
  };
});
