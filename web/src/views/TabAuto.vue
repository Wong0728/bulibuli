<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue';
import { useBloggerStore } from '@/stores/blogger';
import { useAppStore } from '@/stores/app';
import { useAuthStore } from '@/stores/auth';
import { useToastStore } from '@/stores/toast';
import { task as taskApi, logs as logsApi } from '@/api';
import type { Blogger } from '@/api/types';

const blogger = useBloggerStore();
const app = useAppStore();
const auth = useAuthStore();
const toast = useToastStore();

const editBlogger = ref<Blogger | null>(null);
const showEditModal = ref(false);

let countdownTimer: number | null = null;
const now = ref(Date.now());

onMounted(async () => {
  await blogger.refreshList();
  await refreshAllStatuses();
  countdownTimer = window.setInterval(() => {
    now.value = Date.now();
    if (blogger.selected) void blogger.refreshStatus(blogger.selected.uid).catch(() => {});
  }, 1000);
});

onUnmounted(() => {
  if (countdownTimer) clearInterval(countdownTimer);
});

watch(() => blogger.selectedBloggerId, async (id) => {
  if (!id) return;
  const b = blogger.bloggers.find(x => x.id === id);
  if (b) {
    auth.clearBloggerLogs();
    app.subscribeBloggerLogs(b.uid);
    await blogger.refreshStatus(b.uid);
  }
});

async function refreshAllStatuses() {
  for (const b of blogger.bloggers) {
    blogger.refreshStatus(b.uid).catch(() => {});
  }
}

const countdownText = computed(() => {
  if (!blogger.selected) return '--:--:--';
  const status = blogger.taskStatuses[blogger.selected.uid];
  if (!status?.running) return '已停止';
  if (!status.next_check) return '计算中…';
  const ms = Math.max(0, status.next_check * 1000 - now.value);
  const h = Math.floor(ms / 3600000);
  const m = Math.floor((ms % 3600000) / 60000);
  const s = Math.floor((ms % 60000) / 1000);
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
});

const runningStatus = computed(() => {
  if (!blogger.selected) return '—';
  const status = blogger.taskStatuses[blogger.selected.uid];
  return status?.running ? '运行中' : '已停止';
});

const strategySummary = computed(() => {
  if (!blogger.selected) return '—';
  const b = blogger.selected;
  const parts: string[] = [];
  if (b.download_video) parts.push('视频');
  if (b.download_danmaku) parts.push('弹幕');
  if (b.download_comments) parts.push('评论');
  if (b.download_cover) parts.push('封面');
  parts.push(b.burn_after_merge ? '自动烧录' : '不自动烧录');
  return parts.join('·');
});

async function startSelected() {
  if (!blogger.selected) return;
  try {
    await blogger.startTask(blogger.selected.uid);
    toast.success('任务已启动');
  } catch (e: any) { toast.error(e?.message || '启动失败'); }
}

async function stopSelected() {
  if (!blogger.selected) return;
  try {
    await blogger.stopTask(blogger.selected.uid);
    toast.success('任务已停止');
  } catch (e: any) { toast.error(e?.message || '停止失败'); }
}

async function cleanupSelected() {
  if (!blogger.selected) return;
  try {
    await blogger.cleanupNow(blogger.selected.uid);
    toast.success('清理完成');
  } catch (e: any) { toast.error(e?.message || '清理失败'); }
}

function openEdit() {
  if (!blogger.selected) return;
  editBlogger.value = JSON.parse(JSON.stringify(blogger.selected));
  showEditModal.value = true;
}
function closeEdit() { showEditModal.value = false; editBlogger.value = null; }
async function saveEdit() {
  if (!editBlogger.value) return;
  try {
    await blogger.updateBlogger(editBlogger.value.id, editBlogger.value);
    toast.success('已更新');
    closeEdit();
  } catch (e: any) { toast.error(e?.message || '更新失败'); }
}

// acknowledge 通知弹窗
const showNoticeModal = ref(false);
function openNoticeModal() { if (blogger.hasUnackNotices) showNoticeModal.value = true; }
function closeNoticeModal() { showNoticeModal.value = false; }
async function ackOne(uid: number) {
  await blogger.acknowledgeOne(uid);
}
async function ackAll() {
  await blogger.acknowledgeAll();
  closeNoticeModal();
}
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
            <button class="btn btn-sm btn-ghost" id="show-add-blogger-btn" @click="app.setTab('search')">
              <i class="fa-solid fa-plus"></i> 添加
            </button>
          </div>
          <div id="blogger-sidebar-list">
            <div v-for="b in blogger.bloggers" :key="b.id"
                 :class="['blogger-sidebar-item', { active: blogger.selectedBloggerId === b.id }]"
                 @click="blogger.selectBlogger(b.id)">
              <div class="blogger-sidebar-name">
                {{ b.name }}
                <span v-if="b.notice_visible" class="blogger-notice-dot" :title="`有变动未确认 (${b.last_seen_at || ''})`"></span>
              </div>
              <div class="blogger-sidebar-meta">
                <span>UID: {{ b.uid }}</span>
                <span v-if="blogger.taskStatuses[b.uid]?.running" class="blogger-sidebar-status running">运行中</span>
                <span v-else class="blogger-sidebar-status stopped">已停止</span>
              </div>
            </div>
            <div v-if="blogger.bloggers.length === 0" class="empty-state" style="padding: 16px;">
              <p>暂无博主</p>
            </div>
          </div>
          <div class="blogger-sidebar-actions">
            <button v-if="blogger.hasUnackNotices"
                    class="btn btn-warning btn-block"
                    data-action="show-blogger-notices"
                    @click="openNoticeModal">
              <i class="fa-solid fa-bell"></i>
              {{ blogger.noticeBloggers.length }} 条变动未确认
            </button>
            <button class="btn btn-primary btn-block" id="save-bloggers-btn" data-action="save-bloggers" @click="blogger.refreshList()">
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
              <div class="blogger-detail-title" title="点击编辑博主配置" @click="openEdit">
                <i class="fa-solid fa-user-circle"></i>
                <span id="detail-blogger-name">{{ blogger.selected.name }}</span>
                <i class="fa-solid fa-pen blogger-edit-icon"></i>
              </div>
              <div class="btn-row">
                <button class="btn btn-primary" id="detail-start-btn" @click="startSelected" :disabled="blogger.taskStatuses[blogger.selected.uid]?.running">
                  <i class="fa-solid fa-play"></i> 启动
                </button>
                <button class="btn btn-danger" id="detail-stop-btn" @click="stopSelected" :hidden="!blogger.taskStatuses[blogger.selected.uid]?.running">
                  <i class="fa-solid fa-stop"></i> 停止
                </button>
                <button class="btn" @click="cleanupSelected"><i class="fa-solid fa-broom"></i> 立即清理</button>
              </div>
            </div>

            <div class="blogger-status-display">
              <div class="blogger-status-row">
                <div class="status-item">
                  <span class="status-label"><i class="fa-solid fa-circle"></i> 运行状态</span>
                  <span :id="'detail-running-status'" :class="['status-value', blogger.taskStatuses[blogger.selected.uid]?.running ? 'tone-success' : 'tone-error']">
                    {{ runningStatus }}
                  </span>
                </div>
                <div class="status-item">
                  <span class="status-label" id="detail-countdown-label"><i class="fa-solid fa-clock"></i> 下次检查</span>
                  <span class="status-value" id="detail-countdown">{{ countdownText }}</span>
                </div>
              </div>
              <div class="blogger-strategy-summary" id="detail-blogger-strategy">
                <i class="fa-solid fa-sliders-h"></i> {{ strategySummary }}
              </div>
            </div>

            <div class="settings-section-title">
              <i class="fa-solid fa-terminal"></i> 博主日志
            </div>
            <div class="blogger-logs-panel" id="detail-blogger-logs">
              <div v-if="auth.bloggerLogs.length === 0" class="drawer-logs-hint">等待日志推送…</div>
              <div v-for="(log, i) in auth.bloggerLogs" :key="i" :class="['drawer-log-item', 'log-' + (log.level || 'info')]">
                <span class="drawer-log-time">{{ new Date(log.ts).toLocaleTimeString() }}</span>
                <span class="drawer-log-level">{{ (log.level || 'info').toUpperCase() }}</span>
                <span class="drawer-log-msg">{{ log.message }}</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- 编辑博主弹窗：与原版 blogger-modal 1:1 同构 -->
    <div v-if="showEditModal && editBlogger" id="blogger-modal" class="modal-overlay" role="dialog" aria-modal="true" @click.self="closeEdit">
      <div class="modal-container modal-container-wide">
        <div class="modal-header">
          <i class="fa-solid fa-user-plus"></i>
          <span id="blogger-modal-title">编辑博主配置 · {{ editBlogger.name }}</span>
          <button type="button" class="modal-close-btn" aria-label="关闭" @click="closeEdit">
            <i class="fa-solid fa-times"></i>
          </button>
        </div>
        <div class="form-section">
          <div class="form-group form-full">
            <label><i class="fa-solid fa-id-card"></i> 博主 UID</label>
            <input type="text" class="form-control" :value="editBlogger.uid" readonly />
          </div>
          <div class="form-divider"><span>下载策略</span></div>
          <div class="form-group form-full policy-grid">
            <label class="choice-row">
              <span>下载视频</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="editBlogger.download_video" />
                <span class="slider"></span>
              </span>
            </label>
            <label class="choice-row">
              <span>下载弹幕</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="editBlogger.download_danmaku" />
                <span class="slider"></span>
              </span>
            </label>
            <label class="choice-row">
              <span>下载评论</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="editBlogger.download_comments" />
                <span class="slider"></span>
              </span>
            </label>
            <label class="choice-row">
              <span>下载封面</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="editBlogger.download_cover" />
                <span class="slider"></span>
              </span>
            </label>
            <label class="choice-row">
              <span>自动烧录弹幕</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="editBlogger.burn_after_merge" />
                <span class="slider"></span>
              </span>
            </label>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" @click="closeEdit">
            <i class="fa-solid fa-times"></i> 取消
          </button>
          <button class="btn btn-primary" @click="saveEdit">
            <i class="fa-solid fa-check"></i> 保存
          </button>
        </div>
      </div>
    </div>

    <!-- 博主变动通知弹窗 -->
    <div v-if="showNoticeModal" id="blogger-notice-modal" class="modal-overlay" role="dialog" aria-modal="true" @click.self="closeNoticeModal">
      <div class="modal-container">
        <div class="modal-header">
          <i class="fa-solid fa-bell"></i>
          <span>博主变动（{{ blogger.noticeBloggers.length }}）</span>
          <button type="button" class="modal-close-btn" aria-label="关闭" @click="closeNoticeModal">
            <i class="fa-solid fa-times"></i>
          </button>
        </div>
        <p class="form-note" style="padding: 0 16px;">
          后端检测到这些博主的昵称或头像发生变化，请确认是否已知。
        </p>
        <div class="form-section">
          <div v-for="b in blogger.noticeBloggers" :key="b.uid" class="form-group form-full blogger-notice-row">
            <div class="blogger-notice-compare">
              <div class="blogger-notice-cell">
                <img v-if="b.face" :src="b.face" class="blogger-notice-avatar" alt="current" />
                <div class="blogger-notice-name">{{ b.name }}</div>
                <div class="form-note">当前</div>
              </div>
              <i class="fa-solid fa-arrow-right"></i>
              <div class="blogger-notice-cell">
                <img v-if="b.last_seen_face" :src="b.last_seen_face" class="blogger-notice-avatar" alt="seen" />
                <div class="blogger-notice-name">{{ b.last_seen_name || '(已删除)' }}</div>
                <div class="form-note">检测到</div>
              </div>
            </div>
            <div class="btn-row" style="margin-top: 8px;">
              <button class="btn btn-sm" @click="ackOne(Number(b.uid))">
                <i class="fa-solid fa-check"></i> 知道了
              </button>
            </div>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" @click="closeNoticeModal">关闭</button>
          <button class="btn btn-primary" @click="ackAll">
            <i class="fa-solid fa-check-double"></i> 全部知道了
          </button>
        </div>
      </div>
    </div>
  </section>
</template>
