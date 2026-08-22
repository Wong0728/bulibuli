<script setup lang="ts">
import { ref, computed, watch, onActivated, onDeactivated, onUnmounted, nextTick } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useAuthStore } from '@/stores/auth';
import { useToastStore } from '@/stores/toast';
import { useDownloadStore } from '@/stores/download';
import { confirmDialog } from '@/composables/confirm';
import { foundation as foundationApi, video as videoApi } from '@/api';

const settings = useSettingsStore();
const auth = useAuthStore();
const toast = useToastStore();
const download = useDownloadStore();

// 扫码登录：通过 app store 共享开关（顶部"未登录"按钮也共用）
import { useAppStore } from '@/stores/app';
const app = useAppStore();
const foundationStatus = ref<any>(null);
/** 基础配置摘要读取失败文案（老框架 catch：基础配置状态读取失败：...）。 */
const foundationError = ref('');
const isOwner = computed(() => auth.state.role === 'owner');
function imageUrl(url?: string) { return url ? videoApi.proxyImage(url) : ''; }
function imageError(event: Event) {
  const image = event.target as HTMLImageElement;
  image.hidden = true;
  image.nextElementSibling?.removeAttribute('hidden');
}

/** 对齐老框架 settings.js applyTheme：system → 移除 data-theme，否则写入。 */
function applyTheme(theme: string) {
  if (theme === 'system') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

// --- 全局日志 15s 轮询（对齐老框架 startGlobalLogsPolling，对所有已认证角色开放）---
let logsTimer: number | null = null;
let logsInFlight = false;
async function refreshGlobalLogs() {
  if (logsInFlight) return;
  logsInFlight = true;
  try { await settings.loadLogs(100); } finally { logsInFlight = false; }
}
function startGlobalLogsPolling() {
  if (logsTimer) return;
  void refreshGlobalLogs();
  logsTimer = window.setInterval(() => {
    if (document.hidden) return;
    if (app.currentTab !== 'settings') return;
    void refreshGlobalLogs();
  }, 15000);
}

// aria2 健康四态点：页面停留期间每 10s 跟随共享 health 缓存刷新，避免陈旧状态。
let healthTimer: number | null = null;
function startHealthPolling() {
  if (healthTimer) return;
  healthTimer = window.setInterval(() => {
    if (document.hidden) return;
    if (app.currentTab !== 'settings') return;
    void download.refreshHealth();
  }, 10000);
}

// 本页挂在 App.vue 的 KeepAlive 下：onUnmounted 在切 Tab 时不会触发，
// 必须用 onActivated/onDeactivated 管轮询启停（onActivated 首次挂载时同样会触发）。
onActivated(() => {
  void (async () => {
    // 老框架 loadSettingsFragment → startGlobalLogsPolling：日志轮询不依赖 owner。
    startGlobalLogsPolling();
    // 老框架 bootstrap.js：仅 owner 才 loadSettingsFromServer（含 ffmpeg 检测、
    // foundation 摘要、update 状态）；/api/settings 是 owner-only，非 owner 不调。
    if (!isOwner.value) return;
    await settings.load();
    if (!settings.loadError) applyTheme(settings.settings.theme || 'system');
    updateStatusLoaded.value = await settings.loadUpdateStatus();
    await loadFoundationSummary();
    void settings.refreshFfmpegPath();
    void download.refreshHealth();
    startHealthPolling();
  })();
});

function stopPolling() {
  if (logsTimer) { clearInterval(logsTimer); logsTimer = null; }
  if (healthTimer) { clearInterval(healthTimer); healthTimer = null; }
}
onDeactivated(stopPolling);
onUnmounted(stopPolling);

// 老框架 loadSettingsFromServer 的 catch：加载失败只 toast，表单保留当前内容。
watch(() => settings.loadError, (err) => {
  if (err) toast.error(`加载设置失败：${err}`);
});

// 老框架 refreshGlobalLogs：渲染后滚动到底部。
watch(() => settings.logs, async () => {
  await nextTick();
  const panel = document.getElementById('global-logs-list');
  if (panel) panel.scrollTop = panel.scrollHeight;
});

// 老框架 onFFmpegModeChange：切换检测模式时重新检测。
watch(() => settings.settings.ffmpeg_mode, () => {
  if (isOwner.value) void settings.refreshFfmpegPath();
});

const timePoints = computed({
  get: () => settings.settings.time_points || [],
  set: (v) => settings.update({ time_points: v }),
});
const pathPreviewText = computed(() => {
  const template = settings.settings.file_naming_template?.trim()
    || (settings.settings.auto_organize ? '{uid}/{title}' : '{title}');
  return `./data/downloads/${template
    .replace(/\{uid\}/g, '123456')
    .replace(/\{up\}/g, '示例UP主')
    .replace(/\{date\}/g, '2026-07-26')
    .replace(/\{title\}/g, '示例视频')
    .replace(/\{bvid\}/g, 'BV1xx411c7mD')
    .replace(/\{quality\}/g, '1080p')
    .replace(/\{codec\}/g, 'av1')
    .replace(/\{page\}/g, '1')
    .replace(/\{type\}/g, 'video')}.mp4`;
});

function addTimePoint() {
  const input = document.getElementById('time-point-input') as HTMLInputElement | null;
  const raw = input?.value.trim() || '';
  if (!raw) return;
  const hours = Number(raw);
  if (!Number.isInteger(hours) || hours < 0 || hours > 72) {
    toast.error('时间点需在 0-72 小时之间');
    return;
  }
  const list = [...(settings.settings.time_points || [])];
  if (list.includes(hours)) {
    toast.warn('该时间点已存在');
    if (input) input.value = '';
    return;
  }
  list.push(hours);
  // 自动去重升序
  const uniq = Array.from(new Set(list)).sort((a, b) => a - b);
  settings.update({ time_points: uniq });
  if (input) input.value = '';
}
function removeTimePoint(hours: number) {
  const list = (settings.settings.time_points || []).filter((x: number) => x !== hours);
  settings.update({ time_points: list });
}

async function saveSettings() {
  try {
    const message = await settings.save();
    // 老框架：优先后端 message（Aria2 重载失败警告、Windows 暂存提示等）；含警告时降级为 warning。
    if (message.includes('Aria2 未能')) toast.warn(message);
    else toast.success(message || '设置已保存');
  } catch (e: any) {
    toast.error(e?.message || '保存设置失败');
    // 老框架：409 版本冲突后重新拉取服务器设置。
    if (e?.status === 409) await settings.load();
  }
}

async function resetSettings() {
  if (!await confirmDialog({
    title: '恢复默认',
    message: '确定要恢复默认设置吗？此操作不可恢复。',
    confirmText: '恢复默认',
    tone: 'danger',
  })) return;
  try {
    const message = await settings.reset();
    toast.success(message || '已恢复默认设置');
  } catch (e: any) {
    toast.error(e?.message || '重置设置失败');
  }
}

const restartingAria2 = ref(false);
async function restartAria2() {
  restartingAria2.value = true;
  try {
    const { restarted, error, message } = await settings.restartAria2();
    if (restarted) toast.success('Aria2 已重新连接');
    else toast.error(error || message || 'Aria2 重新连接失败', 5000);
  } catch (e: any) {
    toast.error(e?.message || 'Aria2 重新连接失败', 5000);
  } finally {
    restartingAria2.value = false;
  }
}

// 更新管理
const applyingUpdate = ref(false);
/** 老框架：loadUpdateStatus 失败静默，版本框保留"正在获取..."初始文案。 */
const updateStatusLoaded = ref(false);
/** 老框架 resultEl.innerHTML 的结构化等价：图标 + 色调 + 文案。 */
type ResultHint = { tone: string; icon: string; spin?: boolean; text: string };
const ffmpegTestResult = ref<ResultHint | null>(null);
const updateCheckResult = ref<ResultHint | null>(null);
const ffmpegModeNote = computed(() => {
  // 老框架 onFFmpegModeChange 的文案表。
  switch (settings.settings.ffmpeg_mode) {
    case 'system': return '使用系统 PATH 环境变量中找到的 ffmpeg';
    case 'embedded': return '使用程序 resources 目录内置的 ffmpeg';
    case 'custom': return '使用手动指定的自定义路径';
    default: return '自动检测：自定义路径 > 内置 ffmpeg > 环境变量 > 系统 PATH';
  }
});
// --- FFmpeg 检测展示（对齐老框架 refreshFFmpegDetectedPath 的渲染）---
const ffmpegSourceText = computed(() => {
  switch (settings.ffmpegInfo?.source) {
    case 'system': return '（系统 PATH）';
    case 'embedded': return '（内置版本）';
    case 'custom': return '（自定义路径）';
    default: return '';
  }
});
const ffmpegSourceIcon = computed(() => {
  switch (settings.ffmpegInfo?.source) {
    case 'system': return 'fa-desktop';
    case 'embedded': return 'fa-box';
    case 'custom': return 'fa-user';
    default: return 'fa-question-circle';
  }
});
async function onTestFfmpeg() {
  ffmpegTestResult.value = { tone: '', icon: 'fa-spinner', spin: true, text: '测试中...' };
  try {
    const { data, message } = await settings.testFFmpeg({
      mode: settings.settings.ffmpeg_mode,
      custom_path: settings.settings.ffmpeg_custom_path,
    });
    if (data?.available) {
      const sourceText = data.source === 'system' ? ' [系统PATH]'
        : data.source === 'embedded' ? ' [内置]'
        : data.source === 'custom' ? ' [自定义]' : '';
      ffmpegTestResult.value = {
        tone: 'status-success', icon: 'fa-check-circle',
        text: `FFmpeg 可用${sourceText} ${data.version ? `(${data.version})` : ''}`.trimEnd(),
      };
    } else {
      ffmpegTestResult.value = { tone: 'status-error', icon: 'fa-times-circle', text: message || 'FFmpeg 不可用' };
    }
  } catch {
    ffmpegTestResult.value = { tone: 'status-error', icon: 'fa-times-circle', text: '测试失败' };
  }
}

/** 老框架 checkUpdate：无 toast，仅 resultEl innerHTML（含 updatable 平台提示）。 */
async function onCheckUpdate() {
  updateCheckResult.value = { tone: '', icon: 'fa-spinner', spin: true, text: '正在检查更新...' };
  try {
    const data = await settings.checkUpdate();
    if (data?.has_update) {
      updateCheckResult.value = {
        tone: 'status-success', icon: 'fa-circle-up',
        text: `发现新版本 ${data.latest_version}${data.updatable ? '' : '（当前平台暂无可下载包）'}`,
      };
    } else {
      updateCheckResult.value = { tone: 'status-success', icon: 'fa-check-circle', text: '已是最新版本' };
    }
  } catch (e: any) {
    // 老框架：网络失败 → '检查更新失败'；后端业务错误 → result.message || '检查失败'。
    updateCheckResult.value = { tone: 'status-error', icon: 'fa-times-circle', text: e?.code === 0 ? '检查更新失败' : (e?.message || '检查失败') };
  }
}

/** 老框架 applyUpdate：确认弹窗非 danger；成功 toast 后端 message 6s；失败仅 resultEl。 */
async function onApplyUpdate() {
  if (!await confirmDialog({
    title: '立即更新',
    message: '确定要立即更新吗？更新只替换程序文件、不触碰 data/ 目录；完成后需重启程序生效。',
    confirmText: '更新',
  })) return;
  applyingUpdate.value = true;
  updateCheckResult.value = { tone: '', icon: 'fa-spinner', spin: true, text: '正在下载并校验更新...' };
  try {
    const { message } = await settings.applyUpdate();
    updateCheckResult.value = { tone: 'status-success', icon: 'fa-check-circle', text: message || '更新完成' };
    toast.success(message || '更新完成', 6000);
    updateStatusLoaded.value = await settings.loadUpdateStatus();
  } catch (e: any) {
    updateCheckResult.value = { tone: 'status-error', icon: 'fa-times-circle', text: e?.message || '更新失败' };
  } finally {
    applyingUpdate.value = false;
  }
}

/** 手动刷新全局日志（对齐老框架 data-action="refresh-global-logs" 按钮，复用轮询防抖）。 */
function loadRecentLogs() { void refreshGlobalLogs(); }
/** 老框架渲染用后端 time 字符串（'--:--:--' 兜底）+ [uid] 标签。 */
const logRows = computed(() => settings.logs.map(l => ({
  time: (l as any).time || '--:--:--',
  level: l.level || 'info',
  message: l.message,
  uid: l.uid,
})));

// --- 手动粘贴 Cookie（对齐老框架 bootstrap.js toggleManualCookie / saveManualCookie）---
const showManualCookie = ref(false);
const manualCookieText = ref('');
function toggleManualCookie() {
  showManualCookie.value = !showManualCookie.value;
  if (showManualCookie.value) {
    // 老框架：展开时清空输入并聚焦。
    manualCookieText.value = '';
    void nextTick(() => document.getElementById('manual-cookies')?.focus());
  }
}
async function saveManualCookie() {
  const cookies = manualCookieText.value.trim();
  if (!cookies) {
    toast.error('请先粘贴 Cookie 内容');
    return;
  }
  try {
    await auth.saveCookies(cookies);
    manualCookieText.value = '';
    showManualCookie.value = false;
    toast.success('账号已保存，正在刷新登录信息...');
  } catch (e: any) {
    toast.error(e?.message || '保存失败');
  }
}

// --- 退出登录（对齐老框架 bootstrap.js logoutAccount：清 Cookie + 注销会话 + reload）---
async function logoutAccount() {
  if (!await confirmDialog({
    title: '退出登录',
    message: '确定要退出当前 B 站账号登录吗？退出后需重新扫码或粘贴 Cookie。',
    confirmText: '退出',
    tone: 'danger',
  })) return;
  // auth store 已完整处理 toast + reload（成功即整页刷新），调用方 await 后无需再 toast。
  await auth.logoutAccount();
}

function setGroup(g: Group) { group.value = g; }

/** 主题切换：立即把状态写到 store 并在 <html> 上反映（老框架 applyTheme），
 *  持久化通过 settings.update → save() 完成。 */
function onThemeChange(v: string) {
  settings.update({ theme: v as any });
  applyTheme(v);
}

// --- 基础配置只读摘要（对齐老框架 settings.js loadFoundationSummary / loadAiSkillInfo）---
async function loadFoundationSummary() {
  foundationError.value = '';
  try {
    foundationStatus.value = await foundationApi.status();
  } catch (e: any) {
    foundationError.value = e?.message || '请在服务器后端 TUI 输入 setup';
  }
}
const foundationModeName = computed(() => {
  const modeNames: Record<string, string> = { local: '仅本机', lan: 'IPv4/IPv6 局域网', proxy: 'HTTPS 反向代理' };
  const mode = foundationStatus.value?.access_mode;
  return modeNames[mode] || mode || '未知';
});
async function copyAiSkillPath() {
  const path = foundationStatus.value?.ai_skill_path;
  if (!path) return;
  try {
    await navigator.clipboard.writeText(path);
    toast.success('已复制路径');
  } catch (e: any) {
    toast.error(`复制失败，请手动选择文本：${e?.message || '浏览器未授权'}`, 5000);
  }
}

/** 老框架 settings.js loadSettingsFragment 的 groups：分组 → section 集合。
 *  分组切换不持久化（无记忆），每次进入默认 basic。 */
type Group = 'basic' | 'downloads' | 'advanced' | 'tools' | 'security';
const group = ref<Group>('basic');
const GROUP_SECTIONS: Record<Group, string[]> = {
  basic: ['account', 'appearance', 'query', 'parallel', 'smart'],
  downloads: ['danmaku', 'aria2', 'ffmpeg', 'burn', 'subtitle', 'path', 'storage', 'retain', 'verify'],
  advanced: ['board', 'monitor', 'refresh', 'live-recording', 'update'],
  tools: ['logs'],
  security: ['local-config'],
};
const groupButtons: Array<{ key: Group; label: string }> = [
  { key: 'basic', label: '基础' },
  { key: 'downloads', label: '下载' },
  { key: 'advanced', label: '高级' },
  { key: 'tools', label: '工具' },
  { key: 'security', label: '安全' },
];
function inGroup(section: string) {
  return (GROUP_SECTIONS[group.value] || GROUP_SECTIONS.basic).includes(section);
}

// --- section 折叠（对齐老框架 bindCollapsibleSections：点击标题栏切换 collapsed）---
const collapsedSections = ref(new Set<string>());
function toggleSection(section: string) {
  const next = new Set(collapsedSections.value);
  if (next.has(section)) next.delete(section);
  else next.add(section);
  collapsedSections.value = next;
}
/** 事件委托：容器级处理 header 点击 / Enter / Space 折叠。 */
function onSectionsClick(e: Event) {
  const header = (e.target as HTMLElement).closest?.('.section-header') as HTMLElement | null;
  if (!header) return;
  const section = header.closest('.section-collapsible')?.getAttribute('data-section');
  if (section) toggleSection(section);
}
function onSectionsKeydown(e: KeyboardEvent) {
  if (e.key !== 'Enter' && e.key !== ' ') return;
  const header = (e.target as HTMLElement).closest?.('.section-header') as HTMLElement | null;
  if (!header) return;
  e.preventDefault();
  const section = header.closest('.section-collapsible')?.getAttribute('data-section');
  if (section) toggleSection(section);
}

/** 评论回复模式说明文字（对齐老框架 onCommentsReplyModeChange：all 时 tone-error + 图标）。 */
const commentsReplyModeNote = computed(() => {
  const mode = settings.settings.comments_reply_mode;
  if (mode === 'all') return '展开每条评论的全部子评论，会显著增加请求量，触发风控的概率更高。追求稳定请用“仅热门回复”。';
  return '仅取接口自带的约 3 条热门回复，无额外请求，最不易触发风控。';
});
const commentsReplyModeWarning = computed(() => settings.settings.comments_reply_mode === 'all');

/** 下载模式说明文字：老框架 onDownloadModeChange 的 switch 只处理 embedded/rpc，
 *  现有选项 embedded/external 均落在"内置 aria2c"文案上（复刻该行为，不展示 RPC 提示）。 */
const downloadModeNote = computed(() => '自动启动内置 aria2c 并通过 RPC 控制，无需手动配置');
</script>

<style scoped>
.settings-sections-fieldset {
  min-width: 0;
  margin: 0;
  padding: 0;
  border: 0;
}
.operator-settings-note { margin: 12px 0 0; }
</style>

<template>
  <section class="tab-panel">
    <div class="settings-fragment">
      <!-- Operator 会话只读提示（对齐老框架 bootstrap.js applySessionRole；viewer 无提示）。 -->
      <p v-if="auth.state.role === 'operator'" class="form-note form-note-warning operator-settings-note">
        <i class="fa-solid fa-lock"></i> 设置仅 Owner 可修改；当前为 Operator 会话，本页为只读展示。
      </p>
      <!-- 分组导航（对齐老框架 settings-group-switcher；置于 fieldset 外，Operator 禁用表单后仍可切换分组浏览）。 -->
      <div class="settings-group-switcher" role="toolbar" aria-label="设置分组">
        <button v-for="b in groupButtons" :key="b.key" type="button" class="btn btn-sm"
                :data-settings-group="b.key" :aria-pressed="group === b.key"
                aria-controls="settings-sections" @click="setGroup(b.key)">{{ b.label }}</button>
      </div>
      <p class="form-note settings-fragment-note">设置按用途分组；首次进入默认显示基础设置。</p>
      <fieldset :disabled="!isOwner" class="settings-sections-fieldset">
      <div class="settings-sections" id="settings-sections" @click="onSectionsClick" @keydown="onSectionsKeydown">
        <!-- B 站账号登录（非 Owner 隐藏，对齐老框架 applySessionRole） -->
        <div v-if="isOwner" v-show="inGroup('account')" :class="{ collapsed: collapsedSections.has('account') }" class="section-collapsible" data-section="account">
          <div class="section-header" role="button" tabindex="0">
            <h2><i class="fa-solid fa-user-circle"></i> B站账号登录</h2>
          </div>
          <div class="section-body">
            <div class="form-section">
              <div class="form-group form-full">
                <div id="cookie-login-status" class="login-status-box">
                  <template v-if="auth.isCookieValid">
                    <template v-if="auth.biliUser?.face">
                      <img :src="imageUrl(auth.biliUser.face)" class="login-user-face" alt="" @error="imageError" />
                      <span class="login-user-face login-user-face-ph" hidden><i class="fa-solid fa-user"></i></span>
                    </template>
                    <span v-else class="login-user-face login-user-face-ph"><i class="fa-solid fa-user"></i></span>
                    <div class="login-user-meta">
                      <span class="login-user-name"><i class="fa-solid fa-user-check"></i> {{ auth.biliUser?.name || '用户' }}</span>
                      <span class="login-user-sub">UID {{ auth.biliUser?.mid || '--' }} · Lv{{ auth.cookieStatus.level || 0 }} · 已登录</span>
                    </div>
                  </template>
                  <span v-else class="login-user-sub">
                    <i class="fa-solid fa-user-xmark"></i>
                    {{ auth.cookieStatusLoaded ? '尚未登录 B 站账号' : '正在检查登录状态…' }}
                  </span>
                </div>
                <div class="btn-group account-action-group">
                  <button type="button" class="btn btn-primary" data-action="show-qr-login" @click="app.openCookieLogin()">
                    <i class="fa-solid fa-qrcode"></i> 扫码登录 / 切换账号
                  </button>
                  <button type="button" class="btn btn-danger" data-action="logout-account" @click="logoutAccount">
                    <i class="fa-solid fa-right-from-bracket"></i> 退出登录
                  </button>
                  <button type="button" class="btn btn-ghost" data-action="toggle-manual-cookie" @click="toggleManualCookie">
                    <i class="fa-solid fa-keyboard"></i> 手动粘贴 Cookie
                  </button>
                </div>
                <div class="form-note">推荐扫码登录；登录后 Cookie 由服务器安全保存，页面不再显示明文。切换账号可重新扫码或手动粘贴其他账号的 Cookie。</div>

                <div v-show="showManualCookie" class="manual-cookie-panel" id="manual-cookie-box">
                  <label class="sr-only" for="manual-cookies">B站 Cookie</label>
                  <textarea id="manual-cookies" v-model="manualCookieText" class="form-control" placeholder="粘贴其他账号的 Cookie 字符串（含 SESSDATA），点击“保存并登录”切换账号"></textarea>
                  <div class="btn-group manual-cookie-actions">
                    <button type="button" class="btn btn-primary" data-action="save-manual-cookie" @click="saveManualCookie">
                      <i class="fa-solid fa-save"></i> 保存并登录
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 外观 -->
        <div v-show="inGroup('appearance')" :class="{ collapsed: collapsedSections.has('appearance') }" class="section-collapsible" data-section="appearance">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-palette"></i> 外观</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-theme">主题</label>
                <select id="setting-theme" class="form-control" :value="settings.settings.theme || 'system'" @change="onThemeChange(($event.target as HTMLSelectElement).value)">
                  <option value="system">跟随系统</option>
                  <option value="light">浅色</option>
                  <option value="dark">深色</option>
                </select>
              </div>
            </div>
          </div>
        </div>

        <!-- 查询设置 -->
        <div v-show="inGroup('query')" :class="{ collapsed: collapsedSections.has('query') }" class="section-collapsible" data-section="query">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-search"></i> 查询设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-manual-query-limit">手动查询视频数量</label>
                <input type="number" id="setting-manual-query-limit" class="form-control" min="1" max="50"
                       :value="settings.settings.manual_query_limit ?? 10"
                       @change="settings.update({ manual_query_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-50，建议 10</div>
              </div>
              <div class="form-group">
                <label for="setting-auto-query-limit">自动下载视频数量</label>
                <input type="number" id="setting-auto-query-limit" class="form-control" min="1" max="20"
                       :value="settings.settings.auto_query_limit ?? 3"
                       @change="settings.update({ auto_query_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-20，建议 3</div>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">跳过充电专属视频</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-skip-charge-videos" aria-label="跳过充电专属视频"
                           :checked="!!settings.settings.skip_charge_videos"
                           @change="settings.update({ skip_charge_videos: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">开启后充电视频不占"自动下载视频数量"名额，自动下载最新 N 个非充电视频；充电视频会记录到看板（充电后可手动重试）。关闭后充电视频照常占名额，已充电的会自动下载、无权限的记录为不可下载</div>
              </div>
              <div class="form-group">
                <label for="setting-video-quality">视频质量 (qn)</label>
                <select id="setting-video-quality" class="form-control"
                        :value="settings.settings.video_max_quality ?? 80"
                        @change="settings.update({ video_max_quality: Number(($event.target as HTMLSelectElement).value) })">
                  <option value="16">360P 流畅</option>
                  <option value="32">480P 清晰</option>
                  <option value="64">720P 高清</option>
                  <option value="80">1080P 高清</option>
                  <option value="112">1080P+ 高码率</option>
                  <option value="116">1080P60 帧</option>
                  <option value="120">4K</option>
                  <option value="125">HDR</option>
                  <option value="126">杜比视界（仅在线观看可用，下载暂不支持）</option>
                  <option value="127">8K（仅在线观看可用，下载暂不支持）</option>
                </select>
                <div class="form-note">需要相应会员权限；126/127 下载链路暂不支持（后端下载白名单最高 125），选择后将按可用画质降级</div>
              </div>
              <div class="form-group">
                <label for="setting-video-format">视频格式 (fnval)</label>
                <select id="setting-video-format" class="form-control"
                        :value="settings.settings.video_format ?? 16"
                        @change="settings.update({ video_format: Number(($event.target as HTMLSelectElement).value) })">
                  <option value="0">FLV</option>
                  <option value="16">DASH MP4</option>
                  <option value="4048">全格式支持</option>
                </select>
                <div class="form-note">建议保持默认</div>
              </div>
              <div class="form-group">
                <label for="setting-min-video-quality">最低允许画质</label>
                <select id="setting-min-video-quality" class="form-control"
                        :value="settings.settings.video_min_quality ?? 16"
                        @change="settings.update({ video_min_quality: Number(($event.target as HTMLSelectElement).value) })">
                  <option value="16">360P</option>
                  <option value="64">720P</option>
                  <option value="80">1080P</option>
                </select>
                <div class="form-note">自动降级不会低于此值</div>
              </div>
              <div class="form-group">
                <label for="setting-codec-preference">编码优先级</label>
                <select id="setting-codec-preference" class="form-control"
                        :value="settings.settings.codec_preference || 'av1,hevc,avc'"
                        @change="settings.update({ codec_preference: ($event.target as HTMLSelectElement).value })">
                  <option value="av1,hevc,avc">AV1 → HEVC → AVC</option>
                  <option value="hevc,av1,avc">HEVC → AV1 → AVC</option>
                  <option value="avc,hevc,av1">AVC → HEVC → AV1</option>
                </select>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">允许画质自动降级</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-allow-quality-fallback" aria-label="允许画质自动降级"
                           :checked="!!settings.settings.allow_quality_fallback"
                           @change="settings.update({ allow_quality_fallback: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 并行下载设置 -->
        <div v-show="inGroup('parallel')" :class="{ collapsed: collapsedSections.has('parallel') }" class="section-collapsible" data-section="parallel">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-layer-group"></i> 并行下载设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-max-parallel">最大并行下载数</label>
                <input type="number" id="setting-max-parallel" class="form-control" min="1" max="10"
                       :value="settings.settings.max_parallel_downloads ?? 3"
                       @change="settings.update({ max_parallel_downloads: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-10。应用层：同时处理的下载任务数（几个视频一起下）</div>
              </div>
              <div class="form-group">
                <label for="setting-wait-slot-timeout">等待槽位超时 (秒)</label>
                <input type="number" id="setting-wait-slot-timeout" class="form-control" min="60" max="3600"
                       :value="settings.settings.wait_slot_timeout ?? 300"
                       @change="settings.update({ wait_slot_timeout: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 60-3600，建议 300</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 弹幕评论 -->
        <div v-show="inGroup('danmaku')" :class="{ collapsed: collapsedSections.has('danmaku') }" class="section-collapsible" data-section="danmaku">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-comments"></i> 弹幕和评论下载设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">自动下载弹幕</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-auto-download-danmaku" aria-label="自动下载弹幕"
                           :checked="!!settings.settings.auto_download_danmaku"
                           @change="settings.update({ auto_download_danmaku: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">自动监控时同步下载弹幕</div>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">自动下载评论</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-auto-download-comments" aria-label="自动下载评论"
                           :checked="!!settings.settings.auto_download_comments"
                           @change="settings.update({ auto_download_comments: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">自动监控时同步下载评论</div>
              </div>
              <div class="form-group">
                <label for="setting-comments-main-limit">主评论数量</label>
                <input type="number" id="setting-comments-main-limit" class="form-control" min="1" max="100"
                       :value="settings.settings.comments_main_limit ?? 30"
                       @change="settings.update({ comments_main_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-100，建议 30</div>
              </div>
              <div class="form-group">
                <label for="setting-comments-reply-mode">每条评论的回复</label>
                <select id="setting-comments-reply-mode" class="form-control"
                        :value="settings.settings.comments_reply_mode ?? 'hot3'"
                        @change="settings.update({ comments_reply_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="hot3">仅热门回复（约 3 条，最稳）</option>
                  <option value="all">全部回复（展开子评论，可能触发风控）</option>
                </select>
                <div class="form-note" id="comments-reply-mode-note" :class="{ 'tone-error': commentsReplyModeWarning }"><i v-if="commentsReplyModeWarning" class="fa-solid fa-exclamation-triangle"></i> {{ commentsReplyModeNote }}</div>
              </div>
              <div class="form-group form-full">
                <label for="setting-comments-filter-regex">评论正则过滤</label>
                <input type="text" id="setting-comments-filter-regex" class="form-control" placeholder="留空表示不过滤，例如: (广告|加群|微信)"
                       :value="settings.settings.comments_filter_regex || ''"
                       @change="settings.update({ comments_filter_regex: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">命中该正则的评论/回复不会写入结果（作用于评论正文，全局生效）。</div>
              </div>
              <div class="form-group">
                <label for="setting-sidecar-archive-mode">弹幕/评论归档</label>
                <select id="setting-sidecar-archive-mode" class="form-control" :value="settings.settings.sidecar_archive_mode || 'overwrite'"
                        @change="settings.update({ sidecar_archive_mode: ($event.target as HTMLSelectElement).value })">
                  <option value="overwrite">仅保留最新文件（默认）</option>
                  <option value="keep_latest_n">保留最近 N 次归档</option>
                  <option value="keep_all">永久保留全部归档</option>
                </select>
                <div class="form-note">固定名文件始终保存最新内容，供查看和烧录使用。</div>
              </div>
              <div v-show="settings.settings.sidecar_archive_mode === 'keep_latest_n'" class="form-group" id="sidecar-archive-limit-group">
                <label for="setting-sidecar-archive-limit">归档保留次数</label>
                <input type="number" id="setting-sidecar-archive-limit" class="form-control" min="1" max="50"
                       :value="settings.settings.sidecar_archive_limit ?? 3"
                       @change="settings.update({ sidecar_archive_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-50。固定名最新文件不计入归档次数。</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 智能下载 -->
        <div v-show="inGroup('smart')" :class="{ collapsed: collapsedSections.has('smart') }" class="section-collapsible" data-section="smart" id="smart-download-card">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-brain"></i> 智能下载</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">启用智能下载</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-enable-smart-download" aria-label="启用智能下载"
                           :checked="!!settings.settings.enable_smart_download"
                           @change="settings.update({ enable_smart_download: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">根据视频发布时间智能决定何时下载弹幕和评论，避免新视频弹幕/评论过少的问题</div>
              </div>
              <div id="smart-download-settings" :class="['form-group', { 'control-disabled': !settings.settings.enable_smart_download }]">
                <label for="setting-min-publish-hours">最小发布时间（小时）</label>
                <input type="number" id="setting-min-publish-hours" class="form-control" min="0" max="72"
                       :value="settings.settings.min_publish_hours ?? 1"
                       @change="settings.update({ min_publish_hours: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 0-72，默认 1 小时。视频发布超过此时间后才下载弹幕/评论</div>
              </div>
              <div id="time-points-settings" :class="['form-group', { 'control-disabled': !settings.settings.enable_smart_download }]">
                <label for="time-point-input">分时下载时间点（小时）</label>
                <div id="time-point-chips" class="time-point-chips">
                  <span v-if="!timePoints.length" class="time-point-empty">未添加时间点</span>
                  <span v-for="h in timePoints" :key="h" class="time-point-chip">
                    {{ h }}h
                    <button type="button" class="time-point-chip-del" title="移除" data-action="remove-time-point" :data-hours="h" @click="removeTimePoint(h)"><i class="fa-solid fa-times"></i></button>
                  </span>
                </div>
                <div class="time-point-add">
                  <input type="number" id="time-point-input" class="form-control" min="0" max="72" placeholder="小时，如 5" />
                  <button type="button" class="btn btn-sm btn-primary" data-action="add-time-point" @click="addTimePoint">
                    <i class="fa-solid fa-plus"></i> 添加
                  </button>
                </div>
                <div class="form-note">在视频发布后这些小时点分别下载弹幕/评论。范围 0-72，自动去重升序。</div>
              </div>
            </div>
          </div>
        </div>

        <!-- Aria2（非 Owner 隐藏，对齐老框架 applySessionRole） -->
        <div v-if="isOwner" v-show="inGroup('aria2')" :class="{ collapsed: collapsedSections.has('aria2') }" class="section-collapsible" data-section="aria2">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-download"></i> Aria2 下载设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <label for="setting-download-mode">下载模式</label>
                <select id="setting-download-mode" class="form-control"
                        :value="settings.settings.aria2_mode ?? 'embedded'"
                        @change="settings.update({ aria2_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="embedded">内置 aria2c（推荐）</option>
                  <option value="external">RPC 远程连接</option>
                </select>
                <div class="form-note" id="download-mode-note">{{ downloadModeNote }}</div>
              </div>
              <div class="form-group form-full">
                <div class="aria2-settings-status">
                  <span><span class="aria2-dot" :class="download.aria2DotClass" data-aria2-status :title="download.aria2Title"></span> 保存连接设置后会立即应用，无需重启应用。</span>
                  <button type="button" class="btn btn-sm" data-action="restart-aria2" :disabled="restartingAria2" @click="restartAria2">
                    <i class="fa-solid fa-rotate"></i> {{ restartingAria2 ? '正在重连' : '重新连接 Aria2' }}
                  </button>
                </div>
              </div>
            </div>

            <!-- 老框架 bug 复刻：onDownloadModeChange 用 `mode !== 'rpc'` 判断而实际选项值是
                 'embedded'/'external'，该面板恒隐藏；按基准原则保持恒隐藏，不"顺手修掉"。 -->
            <div id="rpc-settings-panel" class="rpc-settings-panel" hidden>
              <div class="settings-grid">
                <div class="form-group">
                  <label for="setting-aria2-host">主机地址</label>
                  <input type="text" id="setting-aria2-host" class="form-control"
                         :value="settings.settings.aria2_host || '127.0.0.1'"
                         @change="settings.update({ aria2_host: ($event.target as HTMLInputElement).value })" />
                  <div class="form-note">Aria2 RPC 主机地址</div>
                </div>
                <div class="form-group">
                  <label for="setting-aria2-port">RPC 端口</label>
                  <input type="number" id="setting-aria2-port" class="form-control" min="1" max="65535"
                         :value="settings.settings.aria2_port ?? 6800"
                         @change="settings.update({ aria2_port: Number(($event.target as HTMLInputElement).value) })" />
                  <div class="form-note">Aria2 RPC 端口</div>
                </div>
                <div class="form-group">
                  <label for="setting-aria2-secret">RPC 密钥</label>
                  <input type="password" id="setting-aria2-secret" class="form-control"
                         :value="settings.settings.aria2_secret || ''"
                         @change="settings.update({ aria2_secret: ($event.target as HTMLInputElement).value })" />
                  <div class="form-note">Aria2 RPC 密钥（如果有）</div>
                </div>
              </div>
            </div>

            <div class="rpc-settings-panel">
              <h3 class="settings-section-title"><i class="fa-solid fa-rocket"></i> Aria2c 高级参数</h3>
              <div class="settings-grid">
                <div class="form-group">
                  <label for="setting-max-conn-per-server">每服务器最大连接数</label>
                  <input type="number" id="setting-max-conn-per-server" class="form-control" min="1" max="16"
                         :value="settings.settings.aria2_max_conn_per_server ?? 16"
                         @change="settings.update({ aria2_max_conn_per_server: Number(($event.target as HTMLInputElement).value) })" />
                  <div class="form-note">范围: 1-16，建议 16</div>
                </div>
                <div class="form-group">
                  <label for="setting-split">分片数量</label>
                  <input type="number" id="setting-split" class="form-control" min="1" max="16"
                         :value="settings.settings.aria2_split ?? 16"
                         @change="settings.update({ aria2_split: Number(($event.target as HTMLInputElement).value) })" />
                  <div class="form-note">范围: 1-16，建议与连接数相同</div>
                </div>
                <div class="form-group">
                  <label for="setting-min-split-size">最小分片大小</label>
                  <select id="setting-min-split-size" class="form-control" :value="settings.settings.aria2_min_split_size || '10M'"
                          @change="settings.update({ aria2_min_split_size: ($event.target as HTMLSelectElement).value })">
                    <option value="1M">1 MB</option><option value="5M">5 MB</option><option value="10M">10 MB</option>
                  </select>
                  <div class="form-note">小文件可增大</div>
                </div>
                <div class="form-group">
                  <label for="setting-max-tries">最大重试次数</label>
                  <input type="number" id="setting-max-tries" class="form-control" min="3" max="10"
                         :value="settings.settings.aria2_max_tries ?? 5"
                         @change="settings.update({ aria2_max_tries: Number(($event.target as HTMLInputElement).value) })" />
                  <div class="form-note">范围: 3-10，建议 5</div>
                </div>
                <div class="form-group">
                  <label for="setting-retry-wait">重试等待时间 (秒)</label>
                  <input type="number" id="setting-retry-wait" class="form-control" min="1" max="30"
                         :value="settings.settings.aria2_retry_wait ?? 5"
                         @change="settings.update({ aria2_retry_wait: Number(($event.target as HTMLInputElement).value) })" />
                  <div class="form-note">范围: 1-30，建议 5</div>
                </div>
                <div class="form-group">
                  <label for="setting-max-concurrent-downloads">最大同时下载数</label>
                  <input type="number" id="setting-max-concurrent-downloads" class="form-control" min="1" max="32"
                         :value="settings.settings.aria2_max_concurrent_downloads ?? 3"
                         @change="settings.update({ aria2_max_concurrent_downloads: Number(($event.target as HTMLInputElement).value) })" />
                  <div class="form-note">范围 1-32，与上面的并行数保持同步</div>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- FFmpeg 设置（非 Owner 隐藏，对齐老框架 applySessionRole） -->
        <div v-if="isOwner" v-show="inGroup('ffmpeg')" :class="{ collapsed: collapsedSections.has('ffmpeg') }" class="section-collapsible" data-section="ffmpeg">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-film"></i> FFmpeg 设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <label for="setting-ffmpeg-mode">FFmpeg 模式</label>
                <select id="setting-ffmpeg-mode" class="form-control"
                        :value="settings.settings.ffmpeg_mode || 'auto'"
                        @change="settings.update({ ffmpeg_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="auto">自动检测（推荐）</option>
                  <option value="system">系统 PATH</option>
                  <option value="embedded">内置 ffmpeg.exe</option>
                  <option value="custom">手动指定路径</option>
                </select>
                <div class="form-note" id="ffmpeg-mode-note">{{ ffmpegModeNote }}</div>
              </div>
              <div class="form-group form-full" id="ffmpeg-detected-path-group">
                <span class="form-label">当前检测到的路径</span>
                <div id="ffmpeg-detected-path" class="path-preview-box">
                  <!-- 对齐老框架 refreshFFmpegDetectedPath：失败→检测失败；成功→状态图标 + 来源图标 + 路径 + 来源标注。 -->
                  <template v-if="settings.ffmpegDetectError">检测失败</template>
                  <template v-else-if="settings.ffmpegInfo">
                    <i :class="['fa-solid', settings.ffmpegInfo.available ? 'fa-check-circle status-success' : 'fa-exclamation-triangle status-error']"></i>
                    <i :class="['fa-solid', ffmpegSourceIcon]"></i> {{ settings.ffmpegInfo.path || '未检测到' }}
                    <span v-if="ffmpegSourceText" class="status-source">{{ ffmpegSourceText }}</span>
                  </template>
                  <template v-else>
                    <i class="fa-solid fa-spinner fa-spin"></i> 检测中...
                  </template>
                </div>
              </div>
              <div class="form-group form-full" id="ffmpeg-custom-path-group" v-show="settings.settings.ffmpeg_mode === 'custom'">
                <label for="setting-ffmpeg-path">自定义 FFmpeg 路径</label>
                <input type="text" id="setting-ffmpeg-path" class="form-control"
                       placeholder="请输入 FFmpeg 完整路径"
                       :value="settings.settings.ffmpeg_custom_path || ''"
                       @change="settings.update({ ffmpeg_custom_path: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">指定自定义的 FFmpeg 可执行文件路径</div>
              </div>
              <div class="form-group form-full">
                <div class="btn-group">
                  <button type="button" class="btn" data-action="test-ffmpeg" @click="onTestFfmpeg">
                    <i class="fa-solid fa-vial"></i> 测试 FFmpeg
                  </button>
                  <span id="ffmpeg-test-result" class="test-result-hint">
                    <span v-if="ffmpegTestResult" :class="ffmpegTestResult.tone">
                      <i class="fa-solid" :class="[ffmpegTestResult.icon, { 'fa-spin': ffmpegTestResult.spin }]"></i>
                      {{ ffmpegTestResult.text }}
                    </span>
                  </span>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 烧录设置（原"路径模板" section 已并入下方 path section，对齐老框架 data-section="path"） -->
        <!-- 弹幕烧录参数 -->
        <div v-show="inGroup('burn')" :class="{ collapsed: collapsedSections.has('burn') }" class="section-collapsible" data-section="burn">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-closed-captioning"></i> 弹幕烧录参数</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-burn-opacity">弹幕透明度</label>
                <input type="number" id="setting-burn-opacity" class="form-control" min="0.1" max="1" step="0.1"
                       :value="settings.settings.burn_opacity ?? 0.6"
                       @change="settings.update({ burn_opacity: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 0.1~1.0，默认 0.6</div>
              </div>
              <div class="form-group">
                <label for="setting-burn-font-size-scale">字号缩放</label>
                <input type="number" id="setting-burn-font-size-scale" class="form-control" min="0.5" max="2" step="0.1"
                       :value="settings.settings.burn_font_size_scale ?? 1"
                       @change="settings.update({ burn_font_size_scale: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 0.5~2.0，默认 1.0</div>
              </div>
              <div class="form-group">
                <label for="setting-burn-scroll-time">滚动弹幕时长（秒）</label>
                <input type="number" id="setting-burn-scroll-time" class="form-control" min="1" max="60" step="0.5"
                       :value="settings.settings.burn_scroll_time ?? 8"
                       @change="settings.update({ burn_scroll_time: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1~60，默认 8</div>
              </div>
              <div class="form-group">
                <label for="setting-burn-fix-time">固定弹幕时长（秒）</label>
                <input type="number" id="setting-burn-fix-time" class="form-control" min="1" max="60" step="0.5"
                       :value="settings.settings.burn_fix_time ?? 4"
                       @change="settings.update({ burn_fix_time: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1~60，默认 4</div>
              </div>
              <div class="form-group">
                <label for="setting-burn-bottom-reserve">底部保留高度（像素）</label>
                <input type="number" id="setting-burn-bottom-reserve" class="form-control" min="0" max="200"
                       :value="settings.settings.burn_bottom_reserve ?? 50"
                       @change="settings.update({ burn_bottom_reserve: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 0~200，默认 50；避免弹幕遮挡字幕</div>
              </div>
              <div class="form-group">
                <label for="setting-burn-font-family">烧录字体</label>
                <select id="setting-burn-font-family" class="form-control" :value="settings.settings.burn_font_family || 'auto'"
                        @change="settings.update({ burn_font_family: ($event.target as HTMLSelectElement).value })">
                  <option value="auto">自动（当前平台默认）</option><option value="Microsoft YaHei UI">Microsoft YaHei UI</option>
                  <option value="Noto Sans CJK SC">Noto Sans CJK SC</option><option value="Arial">Arial</option>
                </select>
                <div class="form-note">只影响 ASS 烧录，不修改原始弹幕文件。</div>
              </div>
              <div class="form-group">
                <label for="setting-burn-color-mode">弹幕颜色</label>
                <select id="setting-burn-color-mode" class="form-control" :value="settings.settings.burn_color_mode || 'source'"
                        @change="settings.update({ burn_color_mode: ($event.target as HTMLSelectElement).value })">
                  <option value="source">保留 B 站原始颜色</option><option value="uniform">使用统一颜色</option>
                </select>
              </div>
              <div class="form-group">
                <label for="setting-burn-color">统一颜色（HEX）</label>
                <input type="text" id="setting-burn-color" class="form-control" maxlength="6" placeholder="FFFFFF"
                       :value="settings.settings.burn_color || 'FFFFFF'"
                       @change="settings.update({ burn_color: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">仅在“使用统一颜色”时生效，例如 FFFFFF。</div>
              </div>
            </div>
          </div>
        </div>

        <!-- CC 字幕 -->
        <div v-show="inGroup('subtitle')" :class="{ collapsed: collapsedSections.has('subtitle') }" class="section-collapsible" data-section="subtitle">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-closed-captioning"></i> CC 字幕</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">自动下载 CC 字幕</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-subtitle-enabled" aria-label="自动下载 CC 字幕"
                           :checked="!!settings.settings.subtitle_enabled"
                           @change="settings.update({ subtitle_enabled: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">接受 AI 自动生成字幕</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-subtitle-accept-ai" aria-label="接受 AI 自动生成字幕"
                           :checked="!!settings.settings.subtitle_accept_ai"
                           @change="settings.update({ subtitle_accept_ai: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">关闭时跳过 lan 以 ai- 开头的字幕</div>
              </div>
              <div class="form-group form-full">
                <label for="setting-subtitle-languages">语言过滤（逗号分隔，留空下载全部）</label>
                <input type="text" id="setting-subtitle-languages" class="form-control"
                       placeholder="zh-CN,zh-Hans"
                       :value="settings.settings.subtitle_languages || ''"
                       @change="settings.update({ subtitle_languages: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">例如 zh-CN,zh-Hans；留空表示下载全部语言</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 下载目录整理（对齐老框架 data-section="path"：自动分文件夹 + 目录模板 + 同名策略 + 路径预览） -->
        <div v-show="inGroup('path')" :class="{ collapsed: collapsedSections.has('path') }" class="section-collapsible" data-section="path">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-folder-open"></i> 下载目录整理</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <label for="setting-auto-organize">自动分文件夹</label>
                <div class="switch-group">
                  <span class="switch-label">按博主UID自动创建子文件夹</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-auto-organize" aria-label="按博主 UID 自动创建子文件夹"
                           :checked="!!settings.settings.auto_organize"
                           @change="settings.update({ auto_organize: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
              </div>
              <div class="form-group form-full">
                <label for="setting-path-template">目录模板</label>
                <input id="setting-path-template" class="form-control" placeholder="{uid}/{title}"
                       :value="settings.settings.file_naming_template || ''"
                       @change="settings.update({ file_naming_template: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">可用变量：{up}、{uid}、{date}、{title}、{bvid}、{quality}、{codec}、{page}、{type}</div>
              </div>
              <div class="form-group">
                <label for="setting-conflict-strategy">同名不同内容时</label>
                <select id="setting-conflict-strategy" class="form-control"
                        :value="settings.settings.conflict_strategy || 'suffix'"
                        @change="settings.update({ conflict_strategy: ($event.target as HTMLSelectElement).value as any })">
                  <option value="suffix">保留并加时间戳</option>
                  <option value="skip">跳过新文件</option>
                  <option value="overwrite">覆盖已有文件</option>
                </select>
              </div>
              <div class="form-group form-full">
                <span class="form-label">当前下载路径预览</span>
                <div id="path-preview" class="path-preview-box">
                  <i class="fa-solid fa-file-video"></i>
                  <span id="path-preview-text">{{ pathPreviewText }}</span>
                </div>
                <div class="form-note">下载路径固定在 data/downloads 文件夹下</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 看板显示 -->
        <div v-show="inGroup('board')" :class="{ collapsed: collapsedSections.has('board') }" class="section-collapsible" data-section="board">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-chalkboard"></i> 看板显示</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-path-display-mode">路径显示方式</label>
                <select id="setting-path-display-mode" class="form-control"
                        :value="settings.settings.path_display_mode || 'hidden'"
                        @change="settings.update({ path_display_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="hidden">隐藏路径</option>
                  <option value="relative">复制/显示相对路径</option>
                  <option value="absolute">复制/显示绝对路径（本机访问时生效）</option>
                </select>
                <div class="form-note">绝对路径与“打开所在目录”仅对与服务同机的客户端（本机浏览器）生效；局域网远程设备仍显示相对路径，避免泄露服务端文件系统信息。</div>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">抽屉浏览器下载</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-browser-download-enabled" aria-label="抽屉浏览器下载"
                           :checked="settings.settings.browser_download_enabled !== false"
                           @change="settings.update({ browser_download_enabled: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">开启后，在下载管理页点击视频打开的抽屉里，可勾选服务器上已下载的视频、音频、弹幕、评论等产物并保存到本机（服务器到本机）；关闭后隐藏入口且接口拒绝访问。</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 监控行为 -->
        <div v-show="inGroup('monitor')" :class="{ collapsed: collapsedSections.has('monitor') }" class="section-collapsible" data-section="monitor">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-radar"></i> 监控行为</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">重投检测（纯提示）</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-detect-reupload" aria-label="重投检测"
                           :checked="settings.settings.detect_reupload !== false"
                           @change="settings.update({ detect_reupload: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">开启后，发现标题与近 90 天历史高度相似的新视频时，卡片左上角显示红点，抽屉显示“可能是 BVxxx 的重传”。不会自动重下、不会删旧文件</div>
              </div>
              <div class="form-group form-full">
                <label class="form-label" for="setting-multi-page-mode">多P自动下载范围</label>
                <select id="setting-multi-page-mode" class="form-control"
                        :value="settings.settings.multi_page_mode || 'first'"
                        @change="settings.update({ multi_page_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="first">仅 P1（保持现状）</option>
                  <option value="all">全部分P</option>
                </select>
                <div class="form-note">自动监控发现的多P投稿：选择“仅 P1”只下第一P；选择“全部分P”则每个分P独立入队下载。手动下载不受此项影响，可在抽屉内自选分P</div>
              </div>
              <div class="form-group form-full">
                <label class="form-label" for="setting-scan-page-limit">监控分页上限</label>
                <input type="number" id="setting-scan-page-limit" class="form-control" min="1" max="20"
                       :value="settings.settings.scan_page_limit ?? 5"
                       @change="settings.update({ scan_page_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-20，建议 5。监控时每个博主最多拉取最新 N 页视频（每页 30 个）；上限越高，能发现越久远的视频，但首轮扫描更慢</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 数据刷新 -->
        <div v-show="inGroup('refresh')" :class="{ collapsed: collapsedSections.has('refresh') }" class="section-collapsible" data-section="refresh">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-sync-alt"></i> 数据刷新</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-l1-interval-minutes">L1 视频数据刷新间隔（分钟）</label>
                <input type="number" id="setting-l1-interval-minutes" class="form-control" min="1" max="1440"
                       :value="settings.settings.l1_interval_minutes ?? 5"
                       @change="settings.update({ l1_interval_minutes: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-1440，建议 5。后端每 N 分钟抽 50 条最久未刷的视频更新播放量/状态</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 直播录制 -->
        <div v-show="inGroup('live-recording')" :class="{ collapsed: collapsedSections.has('live-recording') }" class="section-collapsible" data-section="live-recording">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-tower-broadcast"></i> 直播录制</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label class="form-label" for="setting-live-max-concurrent">并发录制上限（路）</label>
                <input type="number" id="setting-live-max-concurrent" class="form-control" min="1" max="8"
                       :value="settings.settings.live_max_concurrent ?? 2"
                       @change="settings.update({ live_max_concurrent: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-8。多房间同时开播时，超出上限的房间不会自动开录</div>
              </div>
              <div class="form-group">
                <label class="form-label" for="setting-live-min-free-space">磁盘余量安全阈值（GiB）</label>
                <input type="number" id="setting-live-min-free-space" class="form-control" min="1" max="1024"
                       :value="settings.settings.live_min_free_space_gib ?? 10"
                       @change="settings.update({ live_min_free_space_gib: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">启动和录制过程中可用空间低于该值时安全停录，避免磁盘写满</div>
              </div>
              <div class="form-group">
                <label class="form-label" for="setting-live-max-duration">单场录制时长上限（小时）</label>
                <input type="number" id="setting-live-max-duration" class="form-control" min="1" max="72"
                       :value="settings.settings.live_max_duration_hours ?? 12"
                       @change="settings.update({ live_max_duration_hours: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-72。防止忘关的异常直播永久占用磁盘</div>
              </div>
              <div class="form-group form-full">
                <label for="setting-live-file-template">录制文件名模板</label>
                <input type="text" id="setting-live-file-template" class="form-control"
                       placeholder="{room_id}_{title}_{date}"
                       :value="settings.settings.live_file_name_template || ''"
                       @change="settings.update({ live_file_name_template: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">可用占位符：{room_id} 房间号、{title} 直播标题、{date} 日期、{time} 时间；系统会自动追加时间戳与短后缀避免重名</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 存储设置。 -->
        <div v-show="inGroup('storage')" :class="{ collapsed: collapsedSections.has('storage') }" class="section-collapsible" data-section="storage">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-hdd"></i> 存储设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-history-limit">历史记录上限</label>
                <input type="number" id="setting-history-limit" class="form-control" min="100" max="5000"
                       :value="settings.settings.history_limit ?? 1000"
                       @change="settings.update({ history_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 100-5000，建议 1000；历史清理按此设置生效</div>
              </div>
              <div class="form-group">
                <label for="setting-log-limit">日志上限</label>
                <input type="number" id="setting-log-limit" class="form-control" min="50" max="500"
                       :value="settings.settings.log_limit ?? 100"
                       @change="settings.update({ log_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 50-500，建议 100</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 视频保留数（全局默认，统一生效）。 -->
        <div v-show="inGroup('retain')" :class="{ collapsed: collapsedSections.has('retain') }" class="section-collapsible" data-section="retain">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-database"></i> 视频保留数</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <label for="setting-per-blogger-retain-default">全局默认保留数</label>
                <input type="number" id="setting-per-blogger-retain-default" class="form-control" min="0" max="1000"
                       :value="settings.settings.per_blogger_retain_default ?? 0"
                       @change="settings.update({ per_blogger_retain_default: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">0 = 不限制；≥1 时每个博主只保留最新 N 条视频（超出的按发布时间删除文件 + 记录），对所有博主统一生效。</div>
              </div>
            </div>
          </div>
        </div>

        <!-- MD5 完整性校验。 -->
        <div v-show="inGroup('verify')" :class="{ collapsed: collapsedSections.has('verify') }" class="section-collapsible" data-section="verify">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-fingerprint"></i> MD5 完整性校验</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <label for="setting-verify-mode">校验模式</label>
                <select id="setting-verify-mode" class="form-control"
                        :value="settings.settings.verify_mode || 'off'"
                        @change="settings.update({ verify_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="off">关闭</option>
                  <option value="on_completion">下载完成时校验一次</option>
                  <option value="periodic">周期校验（按天数）</option>
                </select>
                <div class="form-note">下载完成时算一次 MD5 存库；周期模式另起后台 worker 定时复核</div>
              </div>
              <div class="form-group" id="verify-periodic-group" v-show="settings.settings.verify_mode === 'periodic'">
                <label for="setting-verify-periodic-days">校验间隔（天）</label>
                <input type="number" id="setting-verify-periodic-days" class="form-control" min="1" max="90"
                       :value="settings.settings.verify_periodic_days ?? 7"
                       @change="settings.update({ verify_periodic_days: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-90，建议 7</div>
              </div>
              <div class="form-group" id="verify-batch-group" v-show="settings.settings.verify_mode === 'periodic'">
                <label for="setting-verify-periodic-batch">单次批量</label>
                <input type="number" id="setting-verify-periodic-batch" class="form-control" min="1" max="200"
                       :value="settings.settings.verify_periodic_batch ?? 20"
                       @change="settings.update({ verify_periodic_batch: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-200，建议 20。每 60 秒扫一批最久未校验的</div>
              </div>
              <div class="form-group">
                <label for="setting-verify-concurrency">校验并发数</label>
                <input type="number" id="setting-verify-concurrency" class="form-control" min="1" max="16"
                       :value="settings.settings.verify_concurrency ?? 4"
                       @change="settings.update({ verify_concurrency: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-16，建议 4；后台 MD5 校验线程数</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 更新（对齐老框架 settings.html update section）。 -->
        <div v-show="inGroup('update')" :class="{ collapsed: collapsedSections.has('update') }" class="section-collapsible" data-section="update">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-cloud-arrow-down"></i> 更新</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <!-- loadUpdateStatus 失败静默（老框架），版本框保留“正在获取...”。 -->
              <div class="form-group form-full">
                <span class="form-label">当前版本</span>
                <div class="path-preview-box">
                  <code id="update-current-version">{{ updateStatusLoaded ? (settings.updateStatus.current_version || '未知') : '正在获取...' }}</code>
                </div>
              </div>
              <div class="form-group form-full">
                <span class="form-label">最新版本</span>
                <div class="path-preview-box">
                  <code id="update-latest-version">{{ settings.updateStatus.has_update ? `${settings.updateStatus.latest_version}（有新版本）` : (settings.updateStatus.latest_version || '尚未检查') }}</code>
                </div>
              </div>
              <div class="form-group form-full">
                <label for="setting-update-policy">更新策略</label>
                <select id="setting-update-policy" class="form-control"
                        :value="settings.settings.update_policy || 'manual'"
                        @change="settings.update({ update_policy: ($event.target as HTMLSelectElement).value as any })">
                  <option value="manual">仅提示，不自动更新（推荐）</option>
                  <option value="auto">检测到新版本自动下载暂存</option>
                  <option value="off">关闭检测</option>
                </select>
                <div class="form-note">自动更新只替换程序文件、不触碰 data/ 目录；更新完成后需重启程序生效。</div>
              </div>
              <div class="form-group form-full">
                <div class="btn-group">
                  <button type="button" class="btn" data-action="check-update" @click="onCheckUpdate">
                    <i class="fa-solid fa-magnifying-glass"></i> 立即检查更新
                  </button>
                  <button type="button" class="btn btn-primary" data-action="apply-update" :hidden="!settings.updateStatus.has_update" :disabled="applyingUpdate" @click="onApplyUpdate">
                    <i class="fa-solid fa-download"></i> 立即更新
                  </button>
                  <span id="update-check-result" class="test-result-hint">
                    <span v-if="updateCheckResult" :class="updateCheckResult.tone">
                      <i class="fa-solid" :class="[updateCheckResult.icon, { 'fa-spin': updateCheckResult.spin }]"></i>
                      {{ updateCheckResult.text }}
                    </span>
                  </span>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 全局日志（对齐老框架 logs section：15 秒自动轮询 + 手动刷新）。 -->
        <div v-show="inGroup('logs')" :class="{ collapsed: collapsedSections.has('logs') }" class="section-collapsible" data-section="logs">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-terminal"></i> 全局日志</h2></div>
          <div class="section-body">
            <div class="form-group form-full">
              <div class="btn-group">
                <button type="button" class="btn btn-sm" data-action="refresh-global-logs" @click="loadRecentLogs">
                  <i class="fa-solid fa-rotate"></i> 刷新
                </button>
                <span class="form-note" id="global-logs-hint">最近 100 条监控日志（跨博主），每 15 秒自动刷新</span>
              </div>
              <div id="global-logs-list" class="blogger-logs-panel">
                <div v-if="logRows.length === 0" class="empty-state empty-state-padded"><i class="fa-solid fa-inbox"></i><p>暂无日志</p></div>
                <div v-for="(l, i) in logRows" v-else :key="i" :class="['log-entry', `log-level-${l.level}`]">
                  <span class="log-time">{{ l.time }}</span>
                  <span v-if="l.uid" class="log-uid">[{{ l.uid }}]</span>
                  <span>{{ l.message }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 本机配置（对齐老框架 local-config section：只读文档；实际编辑入口仅在本机 TUI / SSH 隧道）。非 owner 隐藏。 -->
        <div v-if="isOwner" v-show="inGroup('local-config')" :class="{ collapsed: collapsedSections.has('local-config') }" class="section-collapsible" data-section="local-config">
          <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-shield-halved"></i> 本机配置</h2></div>
          <div class="section-body">
            <p class="form-note">以下配置项涉及本机安全或网络绑定，仅允许在服务端本地操作，远程 Web 无法修改。</p>
            <ol class="settings-local-docs">
              <li><strong>基础网络 / 访问模式</strong>：在服务器终端 TUI 输入 <code>setup</code> 进入设置向导，可切换仅本机 / 局域网 / 反向代理模式。</li>
              <li><strong>AI Skill</strong>：同一向导中开关。启用后复制下方路径发给 AI 即可使用。</li>
              <li><strong>Cookie 信任 / FFmpeg 白名单</strong>：首次使用时 TUI 会交互式提示确认，无需手动编辑配置文件。</li>
            </ol>
            <div id="ai-skill-path-box" class="form-group form-full settings-ai-skill-path" v-show="foundationStatus?.ai_skill_enabled">
              <span class="form-label">AI Skill 文件路径</span>
              <div class="path-preview-box settings-path-preview">
                <code id="ai-skill-path-text">{{ foundationStatus?.ai_skill_path || '正在获取路径...' }}</code>
                <button class="btn btn-sm" id="copy-ai-skill-path-btn" @click="copyAiSkillPath"><i class="fa-solid fa-copy"></i> 复制路径</button>
              </div>
              <div class="form-note settings-ai-skill-note">复制路径 → 发给 AI → 让它阅读该文件并学习如何使用</div>
            </div>
            <div id="foundation-summary-content" class="form-group form-full settings-foundation-summary">
              <p v-if="foundationError" class="form-note">基础配置状态读取失败：{{ foundationError }}</p>
              <p v-else-if="!foundationStatus" class="form-note">正在读取基础配置状态...</p>
              <template v-else>
                <p class="form-note">基础配置状态：{{ foundationStatus.configuration_status === 'normal' ? '正常' : '需要检查' }}</p>
                <p class="form-note">AI Skill：{{ foundationStatus.ai_skill_enabled ? '已启用' : '已关闭' }}</p>
                <p class="form-note">当前访问模式：{{ foundationModeName }}</p>
                <p class="form-note">基础配置入口：{{ foundationStatus.setup_access === 'unavailable' ? '未启动' : '仅本机可访问（127.0.0.1）' }}</p>
                <p v-if="foundationStatus.restart_required" class="form-note form-note-warning">基础网络配置已保存但尚未生效；请重启程序。</p>
              </template>
            </div>
          </div>
        </div>
      </div>
      </fieldset>

      <!-- 设置操作按钮（对齐老框架 .card > .settings-actions）。 -->
      <div class="card">
        <div class="settings-actions">
          <button class="btn btn-primary" data-action="save-settings" :disabled="!isOwner || settings.saving" @click="saveSettings">
            <i class="fa-solid fa-save"></i> {{ settings.saving ? '保存中...' : '保存所有设置' }}
          </button>
          <button class="btn btn-danger" data-action="reset-settings" :disabled="!isOwner" @click="resetSettings"><i class="fa-solid fa-undo"></i> 恢复默认</button>
          <button class="btn" data-action="load-settings" :disabled="!isOwner" @click="settings.load"><i class="fa-solid fa-redo"></i> 刷新</button>
        </div>
        <!-- DB-IP GeoIP 数据归因（CC BY 4.0 许可要求）。 -->
        <p class="form-note settings-attribution">
          GeoIP data by <a href="https://db-ip.com" target="_blank" rel="noopener noreferrer">DB-IP.com</a> (CC BY 4.0)
        </p>
      </div>
    </div>
  </section>
</template>
