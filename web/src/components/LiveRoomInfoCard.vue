<script setup lang="ts">
/**
 * 实时直播信息卡片（/api/live/room-info 数据展示 + 与当前录制 session 关联的实时进度）。
 * 从 TabLive.vue 拆出的展示子组件，同时携带原组件内对应的 scoped 样式。
 */
import { useLiveStore, qualityText, videoStatusText, formatFileSize } from '@/stores/live';
import { useLiveTick } from '@/composables/useLiveTick';

defineProps<{
  /** room-info 接口返回的实时数据（已判空，由父组件控制渲染时机）。 */
  roomInfo: any;
  loading: boolean;
  /** 当前录制 session（来自 dashboard sessions），用于展示实时进度。 */
  session: any | null;
}>();

const emit = defineEmits<{
  (e: 'refresh'): void;
}>();

const live = useLiveStore();
const { liveDurationText } = useLiveTick();

function onRefresh() { emit('refresh'); }
</script>

<template>
  <div class="live-room-info-card">
    <div class="live-section-title">
      <span><i class="fa-solid fa-circle-info"></i> 实时直播信息</span>
      <button class="btn btn-sm btn-ghost" :disabled="loading" @click="onRefresh">
        <i class="fa-solid fa-arrows-rotate" :class="{ 'fa-spin': loading }"></i> 刷新
      </button>
    </div>
    <div class="live-info-grid">
      <div class="live-info-item">
        <span class="live-info-label">标题</span>
        <span class="live-info-value" :title="roomInfo.title || '—'">{{ roomInfo.title || '—' }}</span>
      </div>
      <div class="live-info-item">
        <span class="live-info-label">分区</span>
        <span class="live-info-value">{{ roomInfo.parent_area_name || '' }}{{ roomInfo.area_name ? ' / ' + roomInfo.area_name : '' }}</span>
      </div>
      <div class="live-info-item">
        <span class="live-info-label">开播时间</span>
        <span class="live-info-value">{{ roomInfo.live_time ? new Date(roomInfo.live_time).toLocaleString() : '—' }}</span>
      </div>
      <div class="live-info-item">
        <span class="live-info-label">清晰度</span>
        <span class="live-info-value">{{ qualityText(live.selectedSource?.quality || 10000) }}</span>
      </div>
      <div class="live-info-item" v-if="roomInfo.tags">
        <span class="live-info-label">标签</span>
        <span class="live-info-value live-info-tags">
          <span v-for="t in String(roomInfo.tags).split(',').filter(Boolean)" :key="t" class="live-info-tag">{{ t }}</span>
        </span>
      </div>
      <div class="live-info-item">
        <span class="live-info-label">录制状态</span>
        <span class="live-info-value">
          <span v-if="roomInfo.is_recording" class="live-badge recording">
            <i class="fa-solid fa-record-vinyl"></i> 录制中（{{ videoStatusText(roomInfo.recording_status) }}）
          </span>
          <span v-else-if="roomInfo.can_start" class="live-badge live">可开始录制</span>
          <span v-else class="live-badge">未在录制</span>
        </span>
      </div>
    </div>
    <!-- 与当前录制 session 关联的实时进度（来自 dashboard sessions） -->
    <div v-if="session" class="live-room-info-progress">
      <div class="live-section-subtitle">录制实时进度</div>
      <div class="live-room-info-progress-row">
        <div class="live-progress-line">
          <span class="live-progress-label">已录制</span>
          <span class="live-progress-value">{{ liveDurationText(session) }}</span>
          <span class="live-progress-sep">·</span>
          <span class="live-progress-label">文件大小</span>
          <span class="live-progress-value">{{ formatFileSize(session.file_size) }}</span>
        </div>
        <div class="live-progress-line">
          <span class="live-progress-label">弹幕</span>
          <span class="live-progress-value">{{ (session.danmaku_count ?? 0).toLocaleString() }}</span>
          <span class="live-progress-sep">·</span>
          <span class="live-progress-label">互动人数</span>
          <span class="live-progress-value">{{ (session.unique_user_count ?? 0).toLocaleString() }}</span>
          <span v-if="(session.peak_watched ?? 0) > 0" class="live-progress-sep">·</span>
          <span v-if="(session.peak_watched ?? 0) > 0" class="live-progress-label">累计看过</span>
          <span v-if="(session.peak_watched ?? 0) > 0" class="live-progress-value">{{ (session.peak_watched ?? 0).toLocaleString() }}</span>
        </div>
        <div v-if="(session.sc_count ?? 0) > 0 || (session.guard_count ?? 0) > 0" class="live-progress-line">
          <span class="live-progress-label">SC</span>
          <span class="live-progress-value">{{ session.sc_count ?? 0 }}</span>
          <span class="live-progress-sep">·</span>
          <span class="live-progress-label">上舰</span>
          <span class="live-progress-value">{{ session.guard_count ?? 0 }}</span>
          <span class="live-progress-sep">·</span>
          <span class="live-progress-label">礼物价值</span>
          <span class="live-progress-value">¥{{ (session.estimated_paid_value ?? 0).toFixed(2) }}</span>
        </div>
        <div v-if="session.error_msg" class="live-progress-error">
          <i class="fa-solid fa-triangle-exclamation"></i> {{ session.error_msg }}
        </div>
      </div>
    </div>
  </div>
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
</style>
