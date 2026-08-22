<script setup lang="ts">
import { ref, computed, onUnmounted, onActivated, onDeactivated, watch, nextTick } from 'vue';
import { useBloggerStore } from '@/stores/blogger';
import { useAppStore } from '@/stores/app';
import { useAuthStore } from '@/stores/auth';
import { useToastStore } from '@/stores/toast';
import { video as videoApi } from '@/api';
import { get } from '@/api/client';
import type { Blogger } from '@/api/types';
import BloggerConfigForm, { type BloggerConfigFormModel } from '@/components/BloggerConfigForm.vue';
import { confirmDialog } from '@/composables/confirm';
import { useModalFocus } from '@/composables/modalFocus';

const blogger = useBloggerStore();
const app = useAppStore();
const auth = useAuthStore();
const toast = useToastStore();

function imageUrl(url?: string) { return url ? videoApi.proxyImage(url) : ''; }
function imageError(event: Event) {
  const image = event.target as HTMLImageElement;
  image.hidden = true;
  image.nextElementSibling?.removeAttribute('hidden');
}

const editBlogger = ref<Blogger | null>(null);
const editForm = ref<BloggerConfigFormModel>(createForm());
const showEditModal = ref(false);

// 添加博主弹窗：老框架在自动任务页就地打开 blogger-modal（add 模式），不跳转页面。
const showAddModal = ref(false);
const addForm = ref<BloggerConfigFormModel>(createAddForm());
const adding = ref(false);
const addModalRoot = ref<HTMLElement | null>(null);

let countdownTimer: number | null = null;
let statusTimer: number | null = null;
const now = ref(Date.now());

// --- 博主日志（对齐老框架 loadBloggerLogs / renderBloggerLogs） ---
// 老框架日志面板只显示选中博主日志：HTTP 拉 /api/logs/blogger（100 条）+ 每 2s 刷新；
// 不混入无 uid 的全局日志与磁盘事件。
interface BloggerLogEntry {
  time?: string;
  timestamp?: number;
  level?: string;
  msg?: string;
  message?: string;
}
const bloggerLogs = ref<BloggerLogEntry[]>([]);
const logsPanel = ref<HTMLElement | null>(null);
let logTimer: number | null = null;
let logRequestInFlight = false;

function createForm(): BloggerConfigFormModel {
  return {
    uid: '', name: '', min_interval: 60, max_interval: 300, all_day: true, active_windows: [],
    download_video: true, download_danmaku: true, download_comments: true, download_cover: true,
    burn_danmaku: false, burn_subtitle: false, series_filter_regex: '', start_monitoring: false,
  };
}

function createAddForm(uid = ''): BloggerConfigFormModel {
  return {
    uid, name: '', min_interval: 60, max_interval: 300, all_day: true, active_windows: [],
    download_video: true, download_danmaku: true, download_comments: true, download_cover: true,
    burn_danmaku: false, burn_subtitle: false, series_filter_regex: '', start_monitoring: true,
  };
}

const selectedKnownBlogger = computed(() =>
  blogger.savedBloggers.find(b => String(b.uid) === addForm.value.uid) || null,
);

function openAddModal() {
  addForm.value = createAddForm();
  showAddModal.value = true;
  // 与老框架一致：打开弹窗时异步填充"从现有博主载入配置"下拉。
  void blogger.refreshSaved().catch(() => {});
}
function closeAddModal() { showAddModal.value = false; }

function loadKnownBlogger(event: Event) {
  const uid = (event.target as HTMLSelectElement).value;
  if (!uid) return;
  const saved = blogger.savedBloggers.find(b => String(b.uid) === uid);
  const monitor = blogger.bloggers.find(b => String(b.uid) === uid);
  addForm.value = monitor ? formFromBlogger(monitor) : createAddForm(uid);
  if (!monitor) {
    // 老框架：无自动任务配置时提示并使用默认值。
    toast.info('该博主暂无自动任务配置，已使用默认值');
    if (saved) addForm.value.name = saved.name || '';
  }
}

function validateAddForm(form: BloggerConfigFormModel) {
  if (!form.uid || !/^\d+$/.test(form.uid)) return '请输入博主 UID，或从现有博主列表中选择';
  return validateForm(form);
}

async function confirmAdd() {
  const validation = validateAddForm(addForm.value);
  if (validation) { toast.error(validation); return; }
  adding.value = true;
  toast.info('正在添加博主...');
  try {
    const form = addForm.value;
    const name = form.name.trim() || selectedKnownBlogger.value?.name || '';
    const config = {
      name,
      min_interval: form.min_interval,
      max_interval: form.max_interval,
      download_video: form.download_video,
      download_danmaku: form.download_danmaku,
      download_comments: form.download_comments,
      download_cover: form.download_cover,
      burn_danmaku: form.burn_danmaku,
      burn_subtitle: form.burn_subtitle,
      series_filter_regex: form.series_filter_regex.trim(),
      active_windows: form.all_day ? [] : form.active_windows,
    };
    // 与老框架一致：自动任务列表已存在该 uid 时更新配置（不传 expected_version），否则新建。
    const existing = blogger.bloggers.find(b => String(b.uid) === form.uid);
    if (existing) {
      await blogger.updateBlogger(existing.id, {
        ...config,
        monitor_enabled: form.start_monitoring,
      });
    } else {
      await blogger.addBlogger(Number(form.uid), {
        ...config,
        start_monitoring: form.start_monitoring,
      } as Partial<Blogger>);
    }
    closeAddModal();
    toast.success(`博主 ${name || form.uid} 的监控配置已保存`);
  } catch (e: any) {
    if (e?.code !== 0) toast.error(e?.message || '添加博主失败');
  } finally {
    adding.value = false;
  }
}

function formFromBlogger(b: Blogger): BloggerConfigFormModel {
  const windows = Array.isArray(b.active_windows) ? b.active_windows : [];
  return {
    ...createForm(), uid: String(b.uid), name: b.name || '',
    min_interval: b.min_interval ?? 60, max_interval: b.max_interval ?? 300,
    all_day: windows.length === 0, active_windows: windows.slice(),
    download_video: b.download_video !== false, download_danmaku: b.download_danmaku !== false,
    download_comments: b.download_comments !== false, download_cover: b.download_cover !== false,
    burn_danmaku: b.burn_danmaku === true, burn_subtitle: b.burn_subtitle === true,
    series_filter_regex: b.series_filter_regex || '',
    start_monitoring: b.monitor_enabled === true || b.is_running === true,
  };
}

function validateForm(form: BloggerConfigFormModel) {
  if (form.min_interval < 30 || form.min_interval > 3600) return '最小检查间隔必须在 30-3600 秒之间';
  if (form.max_interval < form.min_interval || form.max_interval > 7200) return '最大检查间隔必须在最小间隔与 7200 秒之间';
  if (!form.all_day && form.active_windows.some(window => {
    const [start, end] = window.split('-');
    return !start || !end || start === end;
  })) return '活跃时段的起止时间不能为空或相同';
  if (!form.all_day && form.active_windows.length === 0) return '请添加至少一个监测时段，或选择全天监测';
  return null;
}

function startCountdown() {
  if (countdownTimer) return;
  countdownTimer = window.setInterval(() => {
    now.value = Date.now();
  }, 1000);
  // 对齐老框架 startStatusPolling：每 2 秒拉一次 /api/task/next-check（页面隐藏时跳过）。
  if (!statusTimer) {
    statusTimer = window.setInterval(() => {
      if (!document.hidden) void blogger.refreshAllStatus();
    }, 2000);
  }
}

function stopCountdown() {
  if (countdownTimer) clearInterval(countdownTimer);
  if (statusTimer) clearInterval(statusTimer);
  countdownTimer = null;
  statusTimer = null;
}

// --- 博主日志加载（对齐老框架 loadBloggerLogs：GET /api/logs/blogger?uid=...&limit=100） ---
async function loadBloggerLogs(uid: number | string) {
  if (logRequestInFlight) return;
  logRequestInFlight = true;
  try {
    const r = await get<{ logs: BloggerLogEntry[] }>('/api/logs/blogger', { uid: String(uid), limit: 100 });
    const logs = Array.isArray(r?.logs) ? r.logs : [];
    // 对齐老框架 renderBloggerLogs：按 timestamp 升序（最新在最后），降级用 time 字段。
    bloggerLogs.value = [...logs].sort((a, b) => {
      const ta = a.timestamp || 0;
      const tb = b.timestamp || 0;
      if (ta && tb) return ta - tb;
      return String(a.time || '').localeCompare(String(b.time || ''));
    });
  } catch (e) {
    console.error('加载日志失败:', e);
  } finally {
    logRequestInFlight = false;
  }
}

/** 对齐老框架 startLogRefresh：每 2 秒刷新当前选中博主的日志（页面隐藏时跳过，同状态轮询守卫）。 */
function startLogRefresh() {
  if (logTimer) return;
  logTimer = window.setInterval(() => {
    if (document.hidden) return;
    const selectedBlogger = blogger.selected;
    if (selectedBlogger) void loadBloggerLogs(selectedBlogger.uid);
  }, 2000);
}

function stopLogRefresh() {
  if (logTimer) clearInterval(logTimer);
  logTimer = null;
}

// 对齐老框架 renderBloggerLogs：日志最新在最后，容器滚到底部。
watch(bloggerLogs, () => {
  void nextTick(() => {
    if (logsPanel.value) logsPanel.value.scrollTop = logsPanel.value.scrollHeight;
  });
});

async function refreshOnEnter() {
  await blogger.refreshList();
  await blogger.refreshAllStatus();
  startCountdown();
  startLogRefresh();
  if (blogger.selected) await loadBloggerLogs(blogger.selected.uid);
}
onActivated(() => { void refreshOnEnter(); });
onDeactivated(() => { stopCountdown(); stopLogRefresh(); });
onUnmounted(() => { stopCountdown(); stopLogRefresh(); });

// 对齐老框架 selectBlogger：切换博主时清空并加载该博主日志 + 刷新状态。
watch(() => blogger.selectedBloggerId, async (id) => {
  if (!id) return;
  const b = blogger.bloggers.find(x => x.id === id);
  if (b) {
    bloggerLogs.value = [];
    await loadBloggerLogs(b.uid);
    await blogger.refreshStatus(b.uid);
  }
});

// --- 侧边栏渲染辅助（对齐老框架 renderBloggerSidebar / computeNextCheckDisplay） ---

function statusOf(b: Blogger) {
  return blogger.taskStatuses[b.uid];
}

function isWaitingWindow(b: Blogger) {
  return statusOf(b)?.runtime_state === 'waiting_window';
}

/** 老框架 displayName：`name (uid)` / `博主 uid`。 */
function displayName(b: Blogger) {
  return b.name ? `${b.name} (${b.uid})` : `博主 ${b.uid}`;
}

/** 头像兜底文本：老框架 (name || uid).slice(0, 2).toUpperCase()。 */
function avatarText(b: Blogger) {
  return (b.name || String(b.uid)).slice(0, 2).toUpperCase();
}

/** 状态点三态：waiting_window → paused（黄）、运行 → running（绿）、停止 → 无类（灰）。 */
function dotClass(b: Blogger) {
  if (isWaitingWindow(b)) return 'paused';
  return statusOf(b)?.running ? 'running' : '';
}

function dotTitle(b: Blogger) {
  if (isWaitingWindow(b)) return '时段外暂停，将自动恢复';
  return statusOf(b)?.running ? '监测中' : '已停止';
}

function statusText(b: Blogger) {
  if (isWaitingWindow(b)) return '时段外暂停';
  return statusOf(b)?.running ? '监测中' : '已停止';
}

function formatCountdown(diffSec: number) {
  const h = Math.floor(diffSec / 3600);
  const m = Math.floor((diffSec % 3600) / 60);
  const s = diffSec % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

/** 对齐老框架 computeNextCheckDisplay：侧边栏"下次检查"倒计时行（不运行时整行隐藏）。 */
function nextCheckDisplay(b: Blogger): { text: string; cls: string } | null {
  const state = statusOf(b);
  if (!state || !state.running) return null;
  const nowSec = Math.floor(now.value / 1000);
  if (state.runtime_state === 'waiting_window') {
    const diff = Math.max(0, (state.next_check || 0) - nowSec);
    if (diff > 0) return { text: `暂停 · ${formatCountdown(diff)} 后恢复`, cls: 'paused' };
    return { text: '等待监测窗口', cls: 'paused' };
  }
  if (state.next_check && state.next_check > 0) {
    const diff = state.next_check - nowSec;
    if (diff > 0) return { text: formatCountdown(diff), cls: diff < 60 ? 'warning' : '' };
    if (diff > -30) return { text: '检查中...', cls: 'checking' };
    return { text: '等待中', cls: 'waiting' };
  }
  return { text: '初始化...', cls: 'initializing' };
}

// --- 详情面板（对齐老框架 updateDetailPanel / updateBloggerCountdown） ---

const detailName = computed(() => {
  const b = blogger.selected;
  if (!b) return '';
  return b.name ? `${b.name} (${b.uid})` : (b.uid ? `博主 UID: ${b.uid}` : '未设置UID');
});

const selectedRunning = computed(() => !!blogger.selected && !!statusOf(blogger.selected)?.running);

/** 运行状态四态：时段外暂停（黄）/ 正在检查 / 监测中（绿）/ 已停止。 */
const runningStatus = computed(() => {
  const b = blogger.selected;
  if (!b) return '已停止';
  const state = statusOf(b);
  if (!state) return '已停止';
  if (state.runtime_state === 'waiting_window') return '时段外暂停';
  if (state.runtime_state === 'checking') return '正在检查';
  return state.running ? '监测中' : '已停止';
});

const runningStatusClass = computed(() => {
  const b = blogger.selected;
  if (!b) return 'stopped';
  const state = statusOf(b);
  if (!state) return 'stopped';
  if (state.runtime_state === 'waiting_window') return 'paused';
  if (state.runtime_state === 'checking') return 'checking';
  return state.running ? 'running' : 'stopped';
});

/** 老框架 detail-countdown-label：waiting_window 时显示"恢复监测"，否则"下次检查"。 */
const countdownLabel = computed(() =>
  blogger.selected && isWaitingWindow(blogger.selected) ? '恢复监测' : '下次检查');

const countdownText = computed(() => {
  const b = blogger.selected;
  if (!b) return '--:--:--';
  const state = statusOf(b);
  if (!state) return '--:--:--';
  const nowSec = Math.floor(now.value / 1000);
  if (state.runtime_state === 'waiting_window') {
    const diff = Math.max(0, (state.next_check || 0) - nowSec);
    if (diff > 0) return formatCountdown(diff);
    return '等待恢复';
  }
  if (!state.running) return '--:--:--';
  if (state.next_check && state.next_check > 0) {
    const diff = state.next_check - nowSec;
    if (diff > 0) return formatCountdown(diff);
    if (diff > -30) return '检查中...';
    return '等待中';
  }
  return '初始化...';
});

const countdownClass = computed(() => {
  const b = blogger.selected;
  if (!b) return 'stopped';
  const state = statusOf(b);
  if (!state) return 'stopped';
  const nowSec = Math.floor(now.value / 1000);
  if (state.runtime_state === 'waiting_window') return 'paused';
  if (!state.running) return 'stopped';
  if (state.next_check && state.next_check > 0) {
    const diff = state.next_check - nowSec;
    if (diff > 0) return 'running';
    if (diff > -30) return 'checking';
    return 'waiting';
  }
  return 'initializing';
});

/** 对齐老框架 updateDetailPanel 的策略摘要（" / "分隔，时段带 UTC 偏移）。 */
const strategySummary = computed(() => {
  const b = blogger.selected;
  if (!b) return '';
  const downloads: string[] = [];
  if (b.download_video !== false) downloads.push('视频');
  if (b.download_danmaku !== false) downloads.push('弹幕');
  if (b.download_comments !== false) downloads.push('评论');
  if (b.download_cover !== false) downloads.push('封面');
  const burns: string[] = [];
  if (b.burn_danmaku === true) burns.push('弹幕');
  if (b.burn_subtitle === true) burns.push('字幕');
  const downloadText = downloads.length > 0 ? downloads.join('·') : '不下载视频';
  const burnText = burns.length > 0 ? `自动烧录 ${burns.join('·')}` : '不自动烧录';
  const regexText = b.series_filter_regex ? ` / 合集正则：${b.series_filter_regex}` : '';
  const windowsText = b.active_windows?.length
    ? ` / 检查时段：${b.active_windows.join('、')}${blogger.serverUtcOffset ? `（UTC${blogger.serverUtcOffset}）` : ''}`
    : '';
  return `${downloadText} / ${burnText}${regexText}${windowsText}`;
});

// --- 操作（对齐老框架 startSelectedBlogger / stopSelectedBlogger / handleContextMenuDelete / saveBloggers） ---

async function startSelected() {
  const b = blogger.selected;
  if (!b) return;
  // 对齐老框架：网络检查 + UID 检查 + B 站登录检查。
  if (!app.checkNetworkBeforeAction()) return;
  if (!b.uid) { toast.error('请先设置博主UID'); return; }
  if (!auth.isCookieValid) {
    toast.error('请先在系统设置中登录 B 站账号');
    app.setTab('settings');
    return;
  }
  try {
    const { data, message } = await blogger.startTask(b.uid);
    const schedule = data?.schedule || {};
    if (schedule.runtime_state === 'waiting_window') {
      toast.success(`博主 ${b.uid} 监控已启用，将在下个时段自动恢复`);
    } else {
      toast.success(message || `博主 ${b.uid} 监控已启动`);
    }
  } catch (e: any) {
    toast.error(e?.message || '启动失败');
  }
}

async function stopSelected() {
  const b = blogger.selected;
  if (!b) return;
  try {
    const { message } = await blogger.stopTask(b.uid);
    toast.info(message || '监控已停止');
  } catch (e: any) {
    toast.error(e?.message || '停止失败');
  }
}

async function deleteSelected() {
  const b = blogger.selected;
  if (!b) return;
  // 对齐老框架 handleContextMenuDelete 的确认弹窗（okText '删除' / danger）。
  if (!await confirmDialog({
    title: '删除博主',
    message: `确定要删除博主 ${b.uid} 吗？\n这会停止监控并删除配置，但不会删除已下载的视频。`,
    confirmText: '删除',
    tone: 'danger',
  })) return;
  try {
    const message = await blogger.deleteBlogger(b.id);
    toast.success(message || '自动任务已删除，已添加博主列表不受影响');
  } catch (e: any) {
    toast.error(`删除请求失败：${e?.message || '未知错误'}`);
  }
}

/** 对齐老框架 saveBloggers：刷新列表后 toast"已刷新 N 个博主"
 *  （loadBloggersFromServer 内部吞错，故无论成败都显示该提示）。 */
async function handleRefreshList() {
  await blogger.refreshList();
  toast.success(`已刷新 ${blogger.bloggers.length} 个博主`);
}

function openEdit() {
  if (!blogger.selected) return;
  editBlogger.value = JSON.parse(JSON.stringify(blogger.selected));
  editForm.value = formFromBlogger(blogger.selected);
  showEditModal.value = true;
}
function closeEdit() { showEditModal.value = false; editBlogger.value = null; editForm.value = createForm(); }
async function saveEdit() {
  if (!editBlogger.value) return;
  const validation = validateForm(editForm.value);
  if (validation) { toast.error(validation); return; }
  try {
    const form = editForm.value;
    // 对齐老框架 confirmEditBlogger：不带 expected_version，成功 toast 优先用后端 message。
    const message = await blogger.updateBlogger(editBlogger.value.id, {
      name: form.name.trim(), min_interval: form.min_interval, max_interval: form.max_interval,
      download_video: form.download_video, download_danmaku: form.download_danmaku,
      download_comments: form.download_comments, download_cover: form.download_cover,
      burn_danmaku: form.burn_danmaku, burn_subtitle: form.burn_subtitle,
      series_filter_regex: form.series_filter_regex.trim(),
      active_windows: form.all_day ? [] : form.active_windows,
      monitor_enabled: form.start_monitoring,
    });
    toast.success(message || '博主配置已更新');
    closeEdit();
  } catch (e: any) {
    toast.error(e?.message || '更新失败');
  }
}

const editModalRoot = ref<HTMLElement | null>(null);
useModalFocus(showEditModal, editModalRoot, closeEdit);
useModalFocus(showAddModal, addModalRoot, closeAddModal);
</script>

<template>
  <section class="tab-panel">
    <div class="card">
      <div class="card-title">
        <span><i class="fa-solid fa-users"></i> 博主监控看板</span>
        <span id="auto-board-summary" class="last-pull-time" title="监控博主与运行中的任务概览">{{ blogger.boardSummary }}</span>
      </div>
      <div class="blogger-dashboard">
        <aside class="blogger-sidebar">
          <div class="blogger-sidebar-title">
            <span><i class="fa-solid fa-list"></i> 监控列表</span>
            <button type="button" class="btn btn-sm btn-ghost" id="show-add-blogger-btn" data-mutating @click="openAddModal">
              <i class="fa-solid fa-plus"></i> 添加
            </button>
          </div>
          <div id="blogger-sidebar-list">
            <template v-if="blogger.bloggers.length === 0">
              <div class="empty-state empty-state-spacious">
                <i class="fa-solid fa-users-slash"></i>
                <p>暂无监控博主</p>
                <button class="btn btn-primary" data-mutating @click="openAddModal">
                  <i class="fa-solid fa-plus"></i> 添加博主
                </button>
              </div>
            </template>
            <template v-else>
              <div v-for="b in blogger.bloggers" :key="b.id"
                   :class="['blogger-list-item', { active: blogger.selectedBloggerId === b.id }]"
                   :aria-label="`${displayName(b)}，${statusText(b)}`"
                   tabindex="0"
                   @click="blogger.selectBlogger(b.id)">
                <template v-if="b.face">
                  <img :src="imageUrl(b.face)" class="blogger-avatar" alt="" @error="imageError" />
                  <div class="blogger-avatar avatar-fallback" hidden>{{ avatarText(b) }}</div>
                </template>
                <div v-else class="blogger-avatar avatar-fallback">{{ avatarText(b) }}</div>
                <div class="blogger-info">
                  <div class="blogger-name" :title="displayName(b)">{{ b.name || `博主 ${b.id}` }}</div>
                  <div class="blogger-uid">{{ b.uid }}</div>
                  <div v-if="nextCheckDisplay(b)" :class="['blogger-next-check', nextCheckDisplay(b)!.cls]">
                    <i class="fa-solid fa-clock"></i> {{ nextCheckDisplay(b)!.text }}
                  </div>
                </div>
                <span :class="['blogger-status', dotClass(b)]" :title="dotTitle(b)"></span>
              </div>
            </template>
          </div>
          <div class="blogger-sidebar-actions">
            <button class="btn btn-primary btn-block" id="save-bloggers-btn" data-action="save-bloggers" @click="handleRefreshList">
              <i class="fa-solid fa-rotate"></i> 刷新列表
            </button>
          </div>
        </aside>

        <div class="blogger-detail-panel" id="blogger-detail-panel">
          <div v-if="!blogger.selected" class="blogger-empty-state" id="blogger-empty-state">
            <i class="fa-solid fa-user-clock"></i>
            <p>暂无博主</p>
            <p class="empty-hint">点击左侧列表的"添加"按钮以监控新博主</p>
          </div>
          <div v-else id="blogger-detail-content">
            <div class="blogger-detail-header">
              <div class="blogger-detail-title" data-mutating title="点击编辑博主配置" @click="openEdit">
                <i class="fa-solid fa-user-circle"></i>
                <span id="detail-blogger-name">{{ detailName }}</span>
                <i class="fa-solid fa-pen blogger-edit-icon"></i>
              </div>
              <div class="btn-row">
                <button class="btn btn-primary" id="detail-start-btn" data-mutating :hidden="selectedRunning" @click="startSelected">
                  <i class="fa-solid fa-play"></i> 启动
                </button>
                <button class="btn btn-danger" id="detail-stop-btn" data-mutating :hidden="!selectedRunning" @click="stopSelected">
                  <i class="fa-solid fa-stop"></i> 停止
                </button>
                <button class="btn btn-danger" data-mutating @click="deleteSelected"><i class="fa-solid fa-trash"></i> 删除博主</button>
              </div>
            </div>

            <div class="blogger-status-display">
              <div class="blogger-status-row">
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-circle"></i> 运行状态</span>
                  <span id="detail-running-status" :class="['status-value', runningStatusClass]">{{ runningStatus }}</span>
                </div>
                <div class="status-item">
                  <span class="status-label" id="detail-countdown-label"><i class="fa-solid fa-clock"></i> {{ countdownLabel }}</span>
                  <span class="status-value" id="detail-countdown" :class="countdownClass">{{ countdownText }}</span>
                </div>
              </div>
              <div class="blogger-strategy-summary" id="detail-blogger-strategy">
                <i class="fa-solid fa-sliders-h"></i> {{ strategySummary }}
              </div>
            </div>

            <div class="settings-section-title">
              <i class="fa-solid fa-terminal"></i> 博主日志
            </div>
            <div class="blogger-logs-panel" id="detail-blogger-logs" ref="logsPanel">
              <div v-if="bloggerLogs.length === 0" class="empty-state empty-state-padded">
                <i class="fa-solid fa-info-circle"></i>
                <p>暂无日志</p>
              </div>
              <div v-for="(l, i) in bloggerLogs" :key="i" :class="['log-entry', `log-level-${l.level || 'info'}`]">
                <span class="log-time">{{ l.time || '--:--:--' }}</span>
                <span>{{ l.msg || l.message || '' }}</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 编辑博主弹窗：与原版 blogger-modal 1:1 同构 -->
    <div v-if="showEditModal && editBlogger" ref="editModalRoot" id="blogger-modal" class="modal-overlay" role="dialog" aria-modal="true" @click.self="closeEdit">
      <div class="modal-container modal-container-wide">
        <div class="modal-header">
          <i class="fa-solid fa-user-plus"></i>
          <span id="blogger-modal-title">编辑博主配置 · {{ editBlogger.name }}</span>
          <button type="button" class="modal-close-btn" aria-label="关闭" @click="closeEdit">
            <i class="fa-solid fa-times"></i>
          </button>
        </div>
        <BloggerConfigForm v-model="editForm" :uid-readonly="true" />
        <div class="modal-footer">
          <button class="btn" @click="closeEdit">
            <i class="fa-solid fa-times"></i> 取消
          </button>
          <button class="btn btn-primary" data-mutating @click="saveEdit">
            <i class="fa-solid fa-check"></i> 保存
          </button>
        </div>
      </div>
    </div>

    <!-- 添加博主弹窗：与原版 blogger-modal（add 模式）1:1 同构，在自动任务页就地打开。 -->
    <div v-if="showAddModal" ref="addModalRoot" id="blogger-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="blogger-modal-title" @click.self="closeAddModal">
      <div class="modal-container modal-container-wide">
        <div class="modal-header">
          <i class="fa-solid fa-user-plus"></i>
          <span id="blogger-modal-title">添加监控博主</span>
          <button type="button" class="modal-close-btn" aria-label="关闭" @click="closeAddModal">
            <i class="fa-solid fa-times"></i>
          </button>
        </div>
        <div class="form-section">
          <div class="form-group form-full">
            <label for="known-blogger-select"><i class="fa-solid fa-bookmark"></i> 从现有博主载入配置（可选）</label>
            <select id="known-blogger-select" class="blogger-select" data-viewer-local @change="loadKnownBlogger">
              <option value="">-- 请从列表选择博主 --</option>
              <option v-for="b in blogger.savedBloggers" :key="b.uid" :value="b.uid">
                <img v-if="b.face" class="opt-avatar" :src="imageUrl(b.face)" alt="" />
                <span class="opt-info">
                  <span class="opt-name">{{ b.name }}</span>
                  <span class="opt-meta">UID: {{ b.uid }}<span v-if="b.level"> · Lv{{ b.level }}</span><span v-if="b.fans"> · 粉丝 {{ b.fans }}</span></span>
                </span>
              </option>
            </select>
            <div class="form-note">选择博主后自动载入其 UID 与配置，UID 不可手动修改。</div>
          </div>
        </div>
        <BloggerConfigForm v-model="addForm" :uid-readonly="true" />
        <div class="modal-footer">
          <button class="btn" @click="closeAddModal">
            <i class="fa-solid fa-times"></i> 取消
          </button>
          <button class="btn btn-primary" data-mutating :disabled="adding" @click="confirmAdd">
            <i class="fa-solid fa-check"></i> {{ adding ? '添加中…' : '确认添加' }}
          </button>
        </div>
      </div>
    </div>
  </section>
</template>
