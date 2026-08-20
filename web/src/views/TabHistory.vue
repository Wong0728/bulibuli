<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { useDownloadStore } from '@/stores/download';
import { useHistoryStore } from '@/stores/history';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { openDrawer } from '@/composables/drawer';
import type { HistoryEntry, HistoryGroup } from '@/api/types';

const download = useDownloadStore();
const history = useHistoryStore();
const toast = useToastStore();

const subTab = ref<'downloading' | 'completed' | 'failed'>('completed');
/** 折叠：uid -> 是否展开。默认全部展开。 */
const expanded = ref<Record<string, boolean>>({});

onMounted(async () => {
  await download.refreshHealth();
  // 进入时一次拉齐三 tab：downloading 让"下载中"子 tab 有数据可显示，
  // 避免用户从已完成切过去时再等请求；后端 supports tab=downloading 持久记录。
  await Promise.all([
    history.loadBoard('downloading'),
    history.loadBoard('completed'),
    history.loadBoard('failed'),
  ]);
});

async function switchTab(t: 'downloading' | 'completed' | 'failed') {
  subTab.value = t;
  await history.loadBoard(t);
}

const downloadingList = computed(() => download.downloadingTasks);
const pendingList = computed(() => download.pendingTasks);
const failedList = computed(() => download.failedTasks);
const currentGroups = computed<HistoryGroup[]>(() => {
  if (subTab.value === 'completed') return history.completedGroups;
  if (subTab.value === 'failed') return history.failedGroups;
  return history.downloadingGroups;
});
const currentList = computed<HistoryEntry[]>(() => {
  if (subTab.value === 'completed') return history.completed;
  if (subTab.value === 'failed') return history.failed;
  return history.downloading;
});
const currentCount = computed(() => {
  if (subTab.value === 'completed') return history.completedTotal;
  if (subTab.value === 'failed') return history.failedTotal;
  // downloading 总数取 store 的 + 内存活跃任务（任务还没建 history 记录时优先看内存）
  return history.downloadingTotal + download.downloadingTasks.length + download.pendingTasks.length;
});

async function pauseAll() {
  if (!await confirmDialog({ title: '全部暂停', message: '确认暂停所有下载任务？', tone: 'danger' })) return;
  try {
    const r = await download.pauseAll();
    toast.success(`已暂停 ${r.count} 个任务`);
  } catch (e: any) { toast.error(e?.message || '操作失败'); }
}
async function resumeAll() {
  try {
    const r = await download.resumeAll();
    toast.success(`已恢复 ${r.count} 个任务`);
  } catch (e: any) { toast.error(e?.message || '操作失败'); }
}
async function retryAll() {
  try {
    const r = await download.retryAll();
    toast.success(`已重试 ${r.count} 个失败任务`);
  } catch (e: any) { toast.error(e?.message || '操作失败'); }
}

async function pauseTask(id: number) {
  try { await download.pauseTask(id); } catch (e: any) { toast.error(e?.message || '失败'); }
}
async function resumeTask(id: number) {
  try { await download.resumeTask(id); } catch (e: any) { toast.error(e?.message || '失败'); }
}
async function removeTask(id: number) {
  if (!await confirmDialog({ title: '删除任务', message: '确认从队列中删除此任务？', tone: 'danger' })) return;
  try { await download.removeTask(id); } catch (e: any) { toast.error(e?.message || '失败'); }
}
async function adjustPriority(id: number, current: number, delta: number) {
  try { await download.setPriority(id, current + delta); } catch (e: any) { toast.error(e?.message || '失败'); }
}

async function deleteHistory(id: number) {
  if (!await confirmDialog({ title: '删除下载记录', message: '确认删除该下载记录？本地文件不会删除。', tone: 'danger' })) return;
  try { await history.deleteEntry(id); toast.success('已删除'); } catch (e: any) { toast.error(e?.message || '失败'); }
}
async function openDirectory(id: number) {
  try { await history.openDirectory(id); } catch (e: any) { toast.error(e?.message || '失败'); }
}

function showVideo(bvid: string, historyId?: number) {
  openDrawer({ bvid, history_id: historyId, source: 'history' });
}

function formatSize(bytes?: number) {
  if (!bytes) return '';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatDate(ts?: number) {
  if (!ts) return '';
  return new Date(ts * 1000).toLocaleString();
}

/** 失败原因：根据后端 kind 映射成友好中文；message 优先。 */
function describeFailure(failure?: { message?: string; kind?: string; fallback_reason?: string } | null): string {
  if (!failure) return '';
  const kindMap: Record<string, string> = {
    Paywall: '大会员/付费',
    PermissionDenied: '权限不足',
    NotFound: '资源不存在',
    NetworkError: '网络错误',
    CodecError: '编码错误',
    FFmpegError: 'FFmpeg 错误',
    MergeError: '合并失败',
  };
  const reason = failure.fallback_reason ? `（${failure.fallback_reason}）` : '';
  if (failure.message) return failure.message + reason;
  if (failure.kind) return (kindMap[failure.kind] || failure.kind) + reason;
  return reason || '未知失败';
}

function isGroupOpen(g: HistoryGroup): boolean {
  // 默认展开；用户手动折叠后记忆
  const key = String(g.uid);
  return expanded.value[key] !== false;
}
function toggleGroup(g: HistoryGroup) {
  const key = String(g.uid);
  expanded.value[key] = !isGroupOpen(g);
}
</script>

<template>
  <section class="tab-panel">
    <div class="card">
      <div class="board-top-bar">
        <div class="board-top-bar-left">
          <i class="fa-solid fa-download"></i>
          <span>下载管理</span>
          <span id="last-pull-time" class="last-pull-time" title="上次从后端拉取看板数据的时间">{{ download.queueSummary }}</span>
        </div>
        <div class="board-top-bar-right">
          <span id="aria2-status-dot-history" class="aria2-dot" :class="download.health.aria2_ok ? 'connected' : 'disconnected'" :title="download.health.aria2_ok ? 'aria2 已连接' : 'aria2 未连接'"></span>
          <button class="btn btn-sm btn-ghost" id="board-refresh-btn" title="手动刷新看板数据（5 秒内防抖）" @click="history.loadBoard(subTab)">
            <i class="fa-solid fa-sync-alt"></i> 刷新
          </button>
        </div>
      </div>

      <div class="board-sub-tabs">
        <div :class="['board-sub-tab', { active: subTab === 'downloading' }]" data-board-tab="downloading" data-action="switch-board-tab" @click="switchTab('downloading')">
          下载中 <span class="board-tab-count" id="board-count-downloading">{{ currentCount }}</span>
        </div>
        <div :class="['board-sub-tab', { active: subTab === 'completed' }]" data-board-tab="completed" data-action="switch-board-tab" @click="switchTab('completed')">
          已下载 <span class="board-tab-count" id="board-count-completed">{{ history.completedTotal }}</span>
        </div>
        <div :class="['board-sub-tab', { active: subTab === 'failed' }]" data-board-tab="failed" data-action="switch-board-tab" @click="switchTab('failed')">
          下载失败 <span class="board-tab-count" id="board-count-failed">{{ history.failedTotal }}</span>
        </div>
      </div>

      <div class="history-board" id="history-board">
        <!-- 下载中：合并内存活跃任务（实时进度） + history 持久记录 -->
        <template v-if="subTab === 'downloading'">
          <div v-if="downloadingList.length + pendingList.length + currentList.length === 0" class="empty-state-grid">
            <i class="fa-solid fa-inbox"></i>
            <p>暂无下载任务</p>
            <p class="empty-hint">请先在"博主搜索"或"手动查询"中添加视频</p>
          </div>
          <div v-else class="history-board-list">
            <table class="table">
              <thead>
                <tr><th>标题</th><th>进度</th><th>大小</th><th>速度</th><th>状态</th><th>操作</th></tr>
              </thead>
              <tbody>
                <!-- 内存活跃任务：实时进度来源 -->
                <tr v-for="t in [...downloadingList, ...pendingList]" :key="`live-${t.id}`">
                  <td><a href="#" @click.prevent="showVideo(t.bvid)">{{ t.title }}</a></td>
                  <td>
                    <div class="progress"><div class="bar" :style="{ width: `${t.progress || 0}%` }"></div></div>
                    <span style="font-size: 12px;">{{ (t.progress || 0).toFixed(1) }}%</span>
                  </td>
                  <td>{{ formatSize(t.downloaded_size) }} / {{ formatSize(t.total_size) }}</td>
                  <td>{{ t.speed ? formatSize(t.speed) + '/s' : '—' }}</td>
                  <td>{{ t.status }}</td>
                  <td>
                    <div class="btn-row">
                      <button v-if="t.status === 'downloading'" class="btn btn-sm" @click="pauseTask(t.id)">暂停</button>
                      <button v-else class="btn btn-sm btn-primary" @click="resumeTask(t.id)">恢复</button>
                      <button class="btn btn-sm" @click="adjustPriority(t.id, t.priority || 0, 10)">↑</button>
                      <button class="btn btn-sm" @click="adjustPriority(t.id, t.priority || 0, -10)">↓</button>
                      <button class="btn btn-sm btn-danger" @click="removeTask(t.id)">删除</button>
                    </div>
                  </td>
                </tr>
                <!-- history 持久记录：作为兜底展示 -->
                <tr v-for="e in currentList.filter(e => !downloadingList.some(d => d.bvid === e.bvid) && !pendingList.some(d => d.bvid === e.bvid))" :key="`hist-${e.id}`">
                  <td><a href="#" @click.prevent="showVideo(e.bvid, e.id)">{{ e.title }}</a></td>
                  <td>
                    <div class="progress"><div class="bar" :style="{ width: `${e.task?.progress_percent || 0}%` }"></div></div>
                    <span style="font-size: 12px;">{{ (e.task?.progress_percent || 0).toFixed(1) }}%</span>
                  </td>
                  <td>{{ formatSize(e.task?.downloaded_size) }} / {{ formatSize(e.task?.total_size) }}</td>
                  <td>{{ e.task?.speed ? formatSize(e.task.speed) + '/s' : '—' }}</td>
                  <td>{{ e.task?.status || e.status }}</td>
                  <td>
                    <div class="btn-row">
                      <button v-if="e.task?.task_id && e.task.status === 'downloading'" class="btn btn-sm" @click="pauseTask(e.task.task_id)">暂停</button>
                      <button v-else-if="e.task?.task_id" class="btn btn-sm btn-primary" @click="resumeTask(e.task.task_id)">恢复</button>
                      <button v-if="e.task?.task_id" class="btn btn-sm" @click="adjustPriority(e.task.task_id, e.task?.priority || 0, 10)">↑</button>
                      <button v-if="e.task?.task_id" class="btn btn-sm" @click="adjustPriority(e.task.task_id, e.task?.priority || 0, -10)">↓</button>
                      <button v-if="e.task?.task_id" class="btn btn-sm btn-danger" @click="removeTask(e.task.task_id)">删除</button>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </template>

        <!-- 已下载 / 失败：按博主分组渲染 -->
        <template v-else>
          <div v-if="currentGroups.length === 0" class="empty-state-grid">
            <i class="fa-solid fa-inbox"></i>
            <p>{{ subTab === 'completed' ? '暂无已下载视频' : '暂无失败记录' }}</p>
            <p class="empty-hint">请先在"博主搜索"或"手动查询"中添加视频</p>
          </div>
          <div v-else class="board-groups">
            <div v-for="g in currentGroups" :key="`group-${g.uid}`" class="board-group">
              <div class="board-group-header" @click="toggleGroup(g)">
                <i :class="['fa-solid', isGroupOpen(g) ? 'fa-chevron-down' : 'fa-chevron-right']" class="board-group-caret"></i>
                <img v-if="g.face" :src="g.face" class="board-group-avatar" :alt="g.name" />
                <span v-else class="board-group-avatar board-group-avatar-fallback"><i class="fa-solid fa-user"></i></span>
                <span class="board-group-name">{{ g.name || `UID ${g.uid}` }}</span>
                <span class="board-group-meta">
                  <span v-if="g.notice_visible" class="board-group-notice" title="该博主近期修改过昵称/头像">
                    <i class="fa-solid fa-circle-exclamation"></i>
                  </span>
                  <span class="board-group-count">{{ g.videos.length }} 个</span>
                </span>
              </div>
              <div v-if="isGroupOpen(g)" class="board-group-body">
                <table class="table">
                  <thead>
                    <tr>
                      <th>标题</th>
                      <th v-if="subTab === 'failed'">失败原因</th>
                      <th>下载时间</th>
                      <th>本地路径</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr v-for="e in g.videos" :key="e.id">
                      <td>
                        <a href="#" @click.prevent="showVideo(e.bvid, e.id)">{{ e.title }}</a>
                        <div v-if="e.part_title" class="history-part-title">{{ e.part_title }}</div>
                      </td>
                      <td v-if="subTab === 'failed'" class="history-failure-cell">
                        <span :class="['history-failure-badge', e.failure?.kind ? `kind-${e.failure.kind}` : '']" :title="e.failure?.kind || ''">
                          {{ describeFailure(e.failure) || '—' }}
                        </span>
                      </td>
                      <td>{{ formatDate(e.downloaded_at) }}</td>
                      <td><code style="font-size: 11px;">{{ e.local_path || '—' }}</code></td>
                      <td>
                        <div class="btn-row">
                          <button class="btn btn-sm" @click="openDirectory(e.id)" :disabled="!e.local_path"><i class="fa-solid fa-folder-open"></i> 打开目录</button>
                          <button class="btn btn-sm btn-danger" @click="deleteHistory(e.id)">删除</button>
                        </div>
                      </td>
                    </tr>
                  </tbody>
                </table>
              </div>
            </div>
          </div>
          <div v-if="currentList.length < currentCount" style="text-align: center; margin-top: 12px;">
            <button class="btn" @click="history.loadMore">加载更多</button>
          </div>
        </template>
      </div>
    </div>
  </section>
</template>

<style scoped>
.board-groups {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.board-group {
  border: 1px solid var(--border, #e5e7eb);
  border-radius: 8px;
  overflow: hidden;
  background: var(--card-bg, #fff);
}
.board-group-header {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  cursor: pointer;
  user-select: none;
  background: var(--bg-soft, #f7f8fa);
}
.board-group-header:hover {
  background: var(--bg-hover, #eef0f4);
}
.board-group-caret {
  width: 12px;
  text-align: center;
  color: var(--text-muted, #6b7280);
}
.board-group-avatar {
  width: 28px;
  height: 28px;
  border-radius: 50%;
  object-fit: cover;
  background: var(--bg-soft, #f3f4f6);
}
.board-group-avatar-fallback {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--text-muted, #9ca3af);
}
.board-group-name {
  font-weight: 600;
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.board-group-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--text-muted, #6b7280);
  font-size: 12px;
}
.board-group-notice {
  color: #f59e0b;
}
.board-group-count {
  background: var(--bg-tag, #eef2ff);
  color: var(--text-tag, #4338ca);
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 11px;
}
.board-group-body {
  padding: 4px 0 8px;
}
.history-part-title {
  font-size: 11px;
  color: var(--text-muted, #6b7280);
  margin-top: 2px;
}
.history-failure-cell {
  max-width: 320px;
}
.history-failure-badge {
  display: inline-block;
  padding: 2px 8px;
  border-radius: 4px;
  background: var(--bg-danger-soft, #fee2e2);
  color: var(--text-danger, #b91c1c);
  font-size: 12px;
  word-break: break-word;
  white-space: normal;
}
.history-failure-badge.kind-Paywall {
  background: #fef3c7;
  color: #92400e;
}
.history-failure-badge.kind-PermissionDenied {
  background: #fee2e2;
  color: #b91c1c;
}
</style>
