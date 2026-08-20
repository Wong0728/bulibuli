/**
 * 直播 store：房间管理 + 录制任务看板 + 合并任务。
 * WS 推送的 `live:update` 统一收敛。
 *
 * 所有 action 内部自带 try/catch，**不向调用者抛 promise reject**，
 * 避免 unhandledrejection / pageerror。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { live as liveApi } from '@/api';
import type { LiveDashboard, LiveRecording, LiveSource, LiveRoom, LiveMergeJob } from '@/api/types';

/** 把后端 RecordingInfo 序列化的 session 节点转成 UI 用的 LiveRecording。
 *  started_at 是 RFC3339 字符串，转换成 unix 秒便于前端 formatDate 复用。 */
function fromBackendSession(s: any): LiveRecording {
  const startedAtUnix = s.started_at ? Math.floor(new Date(s.started_at).getTime() / 1000) : undefined;
  return {
    recording_id: s.recording_id != null ? String(s.recording_id) : `room-${s.room_id}`,
    room_id: Number(s.room_id),
    uname: s.title || undefined,
    title: s.title,
    started_at: startedAtUnix,
    status: (typeof s.status === 'string' ? s.status : undefined) as LiveRecording['status'],
    segment_count: s.segment_count ?? undefined,
    // 后端真正给的录制进度字段：
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
  };
}

export const useLiveStore = defineStore('live', () => {
  const sources = ref<LiveSource[]>([]);
  const recordings = ref<LiveRecording[]>([]);
  const history = ref<LiveRecording[]>([]);
  const mergeJobs = ref<LiveMergeJob[]>([]);
  const selectedSourceId = ref<number | null>(null);
  const selectedRecordingId = ref<string | null>(null);
  const monitorRunning = ref(true);
  const diskFree = ref('');
  const lastCheckAt = ref<number | null>(null);
  const loading = ref(false);
  /** 选中房间的实时信息缓存：room_id -> LiveRoom。 */
  const roomInfoCache = ref<Map<number, LiveRoom>>(new Map());
  /** 房间实时信息加载中（防重入）。 */
  const roomInfoLoading = ref<Set<number>>(new Set());

  const selectedSource = computed(() => sources.value.find(s => s.id === selectedSourceId.value) || null);
  const selectedRecording = computed(() => recordings.value.find(r => r.recording_id === selectedRecordingId.value) || null);
  const selectedRoomInfo = computed<LiveRoom | null>(() => {
    if (!selectedSource.value) return null;
    return roomInfoCache.value.get(selectedSource.value.room_id) || null;
  });

  function applyWsUpdate(data: { type: string; payload: any }) {
    if (data.type === 'source.update') {
      const idx = sources.value.findIndex(s => s.id === data.payload.id);
      if (idx >= 0) sources.value[idx] = { ...sources.value[idx], ...data.payload };
      else sources.value.push(data.payload);
      sources.value = [...sources.value];
    } else if (data.type === 'recording.update') {
      // 后端推的是 RecordingInfo 节点，字段名/类型与 dashboard sessions 一致。
      const next = fromBackendSession(data.payload);
      const idx = recordings.value.findIndex(r => r.recording_id === next.recording_id);
      if (idx >= 0) recordings.value[idx] = { ...recordings.value[idx], ...next };
      else recordings.value.unshift(next);
      recordings.value = [...recordings.value];
    } else if (data.type === 'monitor') {
      monitorRunning.value = !!data.payload.running;
    }
  }

  async function refreshDashboard() {
    loading.value = true;
    try {
      const d: any = await liveApi.dashboard();
      if (d) {
        // 后端字段：sources (含 runtime)、sessions (录制中的)、monitor、risk_notice、
        //   can_open_directory、synced_at、server_now、server_timezone、poll_interval_secs、
        //   merge_jobs、recovery、disk { available_bytes, total_bytes, path_hidden }
        sources.value = (d.sources || []).map((s: any) => ({
          id: s.id,
          room_id: s.room_id,
          uid: s.uid,
          uname: s.anchor_name,
          face: s.face,
          title: s.title,
          cover: s.cover,
          live_status: s.runtime?.live_status ?? 0,
          enabled: s.auto_record_enabled,
          auto_record: s.auto_record_enabled,
          quality: s.max_qn,
          // 后端没有 segment_seconds/max_segments/danmaku_mode 概念
          segment_seconds: 0,
          max_segments: 0,
          danmaku_mode: 'off',
          // 原始 runtime 节点也保留，便于高级展示
          runtime: s.runtime,
        })) as LiveSource[];
        recordings.value = (d.sessions || []).map(fromBackendSession);
        // 合并任务全局列表（dashboard 里有）
        mergeJobs.value = Array.isArray(d.merge_jobs) ? d.merge_jobs : [];
        // 磁盘余量：后端是字节数，转成人类可读
        const freeBytes = d.disk?.available_bytes;
        diskFree.value = typeof freeBytes === 'number' ? formatBytes(freeBytes) : '';
        monitorRunning.value = d.monitor?.running ?? d.monitor_running ?? true;
        lastCheckAt.value = d.monitor?.last_check_at ?? d.synced_at ?? null;
      }
    } catch { /* 静默 */ }
    finally { loading.value = false; }
  }

  async function refreshHistory(page = 1) {
    try {
      const r = await liveApi.history(page);
      if (r && Array.isArray((r as any).items)) history.value = (r as any).items as LiveRecording[];
    } catch { /* 静默 */ }
  }

  async function roomInfo(room_id: number, opts: { force?: boolean } = {}): Promise<LiveRoom | null> {
    if (!room_id || room_id <= 0) return null;
    if (!opts.force && roomInfoCache.value.has(room_id)) return roomInfoCache.value.get(room_id)!;
    if (roomInfoLoading.value.has(room_id)) return null;
    roomInfoLoading.value.add(room_id);
    try {
      const r = (await liveApi.roomInfo(room_id)) as unknown as LiveRoom | null;
      if (r) {
        roomInfoCache.value.set(room_id, r);
        roomInfoCache.value = new Map(roomInfoCache.value);
      }
      return r;
    } catch { return null; }
    finally {
      roomInfoLoading.value.delete(room_id);
    }
  }

  async function addSource(room_id: number, config: Partial<LiveSource> = {}) {
    try {
      // 后端 AddSourceBody 字段：room_id, auto_record_enabled (不是 auto_record),
      //   weekly_schedule?, capture_mode?, max_qn (不是 quality)
      // segment_seconds / max_segments / danmaku_mode 不在 source 层面，live 端忽略
      const payload: any = {
        auto_record_enabled: config.auto_record ?? config.enabled ?? false,
        max_qn: config.quality ?? 10000,
      };
      const s = await liveApi.sourceAdd(room_id, payload);
      await refreshDashboard();
      return s;
    } catch { return null; }
  }

  async function updateSource(id: number, patch: Partial<LiveSource>) {
    try {
      // 后端 UpdateLiveSource 字段：room_id (必填), auto_record_enabled, weekly_schedule,
      //   clear_schedule, capture_mode, max_qn
      // 前端用 id 索引 source → 拿 room_id
      const source = sources.value.find(x => x.id === id);
      const room_id = source?.room_id;
      if (!room_id) return null;
      const payload: any = {
        room_id,
        auto_record_enabled: patch.auto_record ?? patch.enabled,
        max_qn: patch.quality,
      };
      // 清理 undefined
      Object.keys(payload).forEach(k => payload[k] === undefined && delete payload[k]);
      const s = await liveApi.sourceUpdate(id, payload);
      if (s) {
        const idx = sources.value.findIndex(x => x.id === id);
        if (idx >= 0) sources.value[idx] = s as any;
      }
      return s;
    } catch { return null; }
  }

  async function deleteSource(id: number) {
    try {
      await liveApi.sourceDelete(id);
      if (selectedSourceId.value === id) selectedSourceId.value = null;
      await refreshDashboard();
    } catch { /* 静默 */ }
  }

  async function startRecording(source_id: number) {
    try {
      const r = await liveApi.start(source_id);
      await refreshDashboard();
      return r;
    } catch { return null; }
  }

  async function stopRecording(recording_id: string) {
    try { await liveApi.stop(recording_id); await refreshDashboard(); } catch { /* 静默 */ }
  }

  async function startMerge(recording_id: string) {
    try { return await liveApi.startMerge(recording_id); } catch { return null; }
  }

  async function mergeJob(job_id: string) {
    try { return await liveApi.mergeJob(job_id); } catch { return null; }
  }

  async function cancelMerge(job_id: string) {
    try { return await liveApi.cancelMerge(job_id); } catch { return null; }
  }

  function selectSource(id: number | null) { selectedSourceId.value = id; }
  function selectRecording(id: string | null) { selectedRecordingId.value = id; }

  function formatBytes(b: number): string {
    if (!Number.isFinite(b) || b <= 0) return '';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let v = b;
    while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
    return `${v.toFixed(v < 10 && i > 0 ? 2 : 0)} ${units[i]}`;
  }

  /** 打开历史录制的本地目录：调 /api/live/history/{id}/open-directory */
  async function openRecording(recordingId: number | string) {
    try {
      // 后端路径参数是 i32
      const id = Number(recordingId);
      if (!Number.isFinite(id)) return;
      await fetch(`/api/live/history/${id}/open-directory`, {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
      });
    } catch { /* 静默 */ }
  }

  return {
    sources, recordings, history, mergeJobs, selectedSourceId, selectedRecordingId,
    monitorRunning, diskFree, lastCheckAt, loading,
    roomInfoCache, roomInfoLoading,
    selectedSource, selectedRecording, selectedRoomInfo,
    applyWsUpdate, refreshDashboard, refreshHistory,
    roomInfo, addSource, updateSource, deleteSource,
    startRecording, stopRecording, startMerge, mergeJob, cancelMerge,
    openRecording, selectSource, selectRecording,
  };
});
