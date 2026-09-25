/**
 * 直播实时互动事件（对齐 live.js pollEvents / renderEvents / renderHeatBar）。
 * 从 TabLive.vue 抽出：事件流轮询、分类过滤、礼物连击合并与弹幕热度桶。
 */
import { ref, computed } from 'vue';
import { useLiveStore, mergeLiveEvents, formatMediaTime } from '@/stores/live';
import { live as liveApi } from '@/api';
import { eventCategory } from '@/utils/liveEvents';

export function useLiveEvents() {
  const live = useLiveStore();

  // 与 TabLive 主视图同一公式：选中房间的当前录制 session。
  const currentSession = computed(() => live.selectedSource
    ? live.recordings.find(r => r.room_id === live.selectedSource!.room_id) || null
    : null);

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

  /** 切换房间时重置互动事件流（对齐老框架 selectRoom）。 */
  function resetEventStream() {
    eventRecordingId.value = null;
    eventSeq.value = 0;
    liveEvents.value = [];
    eventRequestId += 1;
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

  return {
    eventFilter, eventStatus, liveEvents, displayEvents, scEvents,
    pollLiveEvents, startEventPolling, stopEventPolling, resetEventStream,
    heatBuckets, heatClass, heatTitle, heatStyle, onHeatClick,
  };
}
