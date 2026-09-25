<script setup lang="ts">
/**
 * 实时互动面板（对齐 live.js interactionMarkup：session 且互动采集未关闭时显示）。
 * 从 TabLive.vue 拆出的展示子组件：统计格、SC 卡片、类型筛选、弹幕热度条与事件时间线。
 * 事件数据经 props 注入，筛选条件经 v-model:event-filter 回传父级。
 */
import { computed } from 'vue';
import { formatMediaTime } from '@/stores/live';
import { eventCategory, eventTypeLabel, eventText } from '@/utils/liveEvents';

const props = defineProps<{
  /** 当前录制 session。 */
  session: any;
  eventStatus: string;
  eventFilter: 'user' | 'stats' | 'all';
  /** 已按筛选条件过滤 + 礼物连击合并后的事件列表。 */
  displayEvents: any[];
  /** 最近 3 条 SC（倒序）。 */
  scEvents: any[];
  /** 30 秒弹幕热度桶。 */
  heatBuckets: number[];
}>();

const emit = defineEmits<{
  (e: 'update:eventFilter', value: 'user' | 'stats' | 'all'): void;
}>();

function onFilterChange(event: Event) {
  emit('update:eventFilter', (event.target as HTMLSelectElement).value as 'user' | 'stats' | 'all');
}

const heatMax = computed(() => Math.max(1, ...props.heatBuckets.filter(Boolean)));
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

/** 时间线倒序（最新在上）在 computed 里算一次：写在模板里会在每次渲染时
 *  复制整个数组，配合 :key="index" 会让 100 行列表每 2s 按下标整体重建。 */
const timelineEvents = computed(() => [...props.displayEvents].reverse());

/** 事件在 store 里是 any[]，可用字段做稳定键；缺失时才退回下标。 */
function timelineKey(event: any, index: number) {
  const id = event?.id;
  if (id !== undefined && id !== null && id !== '') return String(id);
  return `${event?.event_type || 'event'}-${event?.media_time_ms ?? index}`;
}
</script>

<template>
  <div class="live-interaction-panel">
    <div class="live-section-title">
      <span><i class="fa-solid fa-comments"></i> 实时互动（最近 100 条）</span>
      <span class="form-note">观看为 B 站累计"看过"口径；互动人数按可识别 UID 估算；金额为非结算估算值。</span>
    </div>
    <div class="live-stats-grid">
      <div class="live-stats-cell"><strong>{{ session.danmaku_count || 0 }}</strong><span>弹幕</span></div>
      <div class="live-stats-cell" title="可识别互动事件的去重 UID"><strong>{{ session.unique_user_count || 0 }}</strong><span>互动人数</span></div>
      <div class="live-stats-cell"><strong>{{ session.free_gift_count || 0 }}</strong><span>免费礼物</span></div>
      <div class="live-stats-cell"><strong>{{ session.paid_gift_count || 0 }}</strong><span>付费礼物</span></div>
      <div class="live-stats-cell"><strong>{{ session.sc_count || 0 }}</strong><span>SC</span></div>
      <div class="live-stats-cell"><strong>{{ session.guard_count || 0 }}</strong><span>上舰</span></div>
      <div class="live-stats-cell" title="B 站累计口径，非在线峰值"><strong>{{ session.peak_watched || 0 }}</strong><span>累计看过</span></div>
      <div class="live-stats-cell" title="非结算估算值"><strong>¥{{ Number(session.estimated_paid_value || 0).toFixed(2) }}</strong><span>估算付费价值</span></div>
    </div>
    <div v-if="scEvents.length" class="live-sc-pins">
      <div v-for="(event, index) in scEvents" :key="`sc-${index}`" class="live-sc-card">
        <strong>{{ event.data?.uname || 'SC' }}</strong><span>¥{{ event.data?.price || 0 }}</span><p>{{ event.data?.message || '' }}</p>
      </div>
    </div>
    <div class="live-toolbar-row">
      <select :value="eventFilter" class="form-control" aria-label="互动类型筛选" @change="onFilterChange">
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
      <div v-for="(event, index) in timelineEvents" :key="timelineKey(event, index)"
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
</template>
