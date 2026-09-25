<script setup lang="ts">
/**
 * 录制任务看板（子 tab：录制中 / 最近录制 / 需要处理）。
 * 从 TabLive.vue 拆出：布局对齐下载管理（顶栏 + 子 tab），携带原组件内对应 scoped 样式。
 * 直播数据读 live store；进行中的动作经 emits 上抛由父组件统一处理（pending 状态由 props 注入）。
 */
import { ref, computed } from 'vue';
import {
  useLiveStore, videoStatusText, videoStatusClass, interactionStateText, interactionStateClass,
  captureModeText, qualityText, formatFileSize, formatLiveDuration,
} from '@/stores/live';
import { useLiveTick } from '@/composables/useLiveTick';
import { mergeJobStatusText, historyBadgeClass, historyBadgeText } from '@/utils/liveEvents';

defineProps<{
  /** 录制/停止进行中的房间集合（pending 按钮态对齐 live.js pendingRooms）。 */
  pendingRooms: Set<number>;
  /** 重试合并进行中的录制 ID 集合（对齐 live.js mergePending）。 */
  mergePending: Set<string>;
}>();

const emit = defineEmits<{
  (e: 'stop-record', roomId: number): void;
  (e: 'view-detail', roomId: number): void;
  (e: 'start-merge', recordingId: string | number): void;
  (e: 'burn-danmaku', recording: any): void;
  (e: 'open-recording', recordingId: number | string): void;
  (e: 'cancel-merge', jobId: string): void;
}>();

const live = useLiveStore();
const { liveDurationText } = useLiveTick();

type LiveBoard = 'recording' | 'history' | 'attention';
const subTab = ref<LiveBoard>('recording');

const recordingList = computed(() => [...live.recordings]
  .sort((a, b) => String(a.started_at ?? '').localeCompare(String(b.started_at ?? ''))));

const activeMergeJobs = computed(() => live.mergeJobs.filter(j => ['queued', 'running', 'cancelling'].includes(j.status)));
/** 「需要处理」计数（对齐老框架 renderBoard）：进行中的合并任务 + 可恢复项。 */
const attentionCount = computed(() => activeMergeJobs.value.length + live.recovery.length);
</script>

<template>
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
                <span v-if="(r.sc_count ?? 0) > 0" class="live-recording-stat"><i class="fa-solid fa-comment-dollar"></i> SC <b>{{ r.sc_count ?? 0 }}</b></span>
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
              <button class="btn btn-sm btn-ghost" @click="$emit('view-detail', r.room_id)">查看详情</button>
              <button class="btn btn-sm btn-danger" data-mutating
                      :disabled="pendingRooms.has(r.room_id)"
                      @click="$emit('stop-record', r.room_id)">
                {{ pendingRooms.has(r.room_id) ? '处理中…' : '停止并合并' }}
              </button>
            </div>
          </div>
          <!-- 合并任务（数据来自 dashboard.merge_jobs，30s 轮询刷新） -->
          <div v-if="live.mergeJobs.length > 0" class="live-merge-jobs-block">
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
                            class="btn btn-sm" data-mutating @click="$emit('cancel-merge', String(job.id))">取消</button>
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
                      :disabled="mergePending.has(String(h.id))" @click="$emit('start-merge', String(h.id))">
                {{ mergePending.has(String(h.id)) ? '创建中...' : '重试合并' }}
              </button>
              <button v-if="h.has_output && h.has_events && !h.has_burned" class="btn btn-sm btn-ghost" data-mutating
                      title="把录到的弹幕和 SC 烧录进视频，生成弹幕版" @click="$emit('burn-danmaku', h)">烧录弹幕</button>
              <button v-if="live.canOpenDirectory && h.has_output" class="btn btn-sm btn-ghost" data-mutating
                      @click="$emit('open-recording', h.id!)"><i class="fa-solid fa-folder-open"></i> 打开目录</button>
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
              <button class="btn btn-sm btn-ghost" data-mutating :disabled="!!job.cancel_requested" @click="$emit('cancel-merge', String(job.id))">取消</button>
            </div>
          </div>
          <div v-for="r in live.recovery" :key="`recovery-${r.recording_id}`" class="live-recording-row">
            <div class="live-recording-main">
              <div class="live-recording-title">#{{ r.recording_id }} {{ r.title || '' }} <span class="live-badge failed">失败可恢复</span></div>
              <div class="live-recording-meta"><span>保留源分段 {{ r.segment_count || 0 }} 个</span><span>{{ r.error_msg || '可恢复' }}</span></div>
            </div>
            <div class="live-recording-actions">
              <button class="btn btn-sm btn-primary" data-mutating
                      :disabled="mergePending.has(String(r.recording_id))" @click="$emit('start-merge', r.recording_id)">
                {{ mergePending.has(String(r.recording_id)) ? '创建中...' : '重试合并' }}
              </button>
              <button v-if="live.canOpenDirectory && r.has_output" class="btn btn-sm btn-ghost" data-mutating
                      @click="$emit('open-recording', r.recording_id)">打开目录</button>
            </div>
          </div>
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
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
