<script setup lang="ts">
import { onMounted, computed, ref, watch, defineAsyncComponent } from 'vue';
import { useAppStore } from './stores/app';
import { useAuthStore } from './stores/auth';
import { useDownloadStore } from './stores/download';
// 视图按需加载：8 个视图全部动态 import，拆分 chunk 缩小首屏 bundle。
const TabSearch = defineAsyncComponent(() => import('./views/TabSearch.vue'));
const TabManual = defineAsyncComponent(() => import('./views/TabManual.vue'));
const TabAuto = defineAsyncComponent(() => import('./views/TabAuto.vue'));
const TabHistory = defineAsyncComponent(() => import('./views/TabHistory.vue'));
const TabLive = defineAsyncComponent(() => import('./views/TabLive.vue'));
const TabSettings = defineAsyncComponent(() => import('./views/TabSettings.vue'));
const PairView = defineAsyncComponent(() => import('./views/PairView.vue'));
const SetupView = defineAsyncComponent(() => import('./views/SetupView.vue'));
import VideoDrawer from './components/VideoDrawer.vue';
import ConfirmDialogList from './components/ConfirmDialogList.vue';
import CookieLoginModals from './components/CookieLoginModals.vue';
import ToastList from './components/ToastList.vue';
import { video as videoApi, setup as setupApi } from './api';
import { useToastStore } from './stores/toast';

const app = useAppStore();
const auth = useAuthStore();
const download = useDownloadStore();

const toast = useToastStore();

const tabs = [
  { id: 'search', icon: 'fa-user-plus', label: '博主搜索' },
  { id: 'manual', icon: 'fa-search', label: '手动查询' },
  { id: 'auto', icon: 'fa-bolt', label: '自动任务' },
  { id: 'history', icon: 'fa-history', label: '下载管理' },
  { id: 'live', icon: 'fa-tower-broadcast', label: '直播' },
  { id: 'settings', icon: 'fa-cog', label: '设置' },
] as const;

const cookieWarningDismissed = ref(false);
const isOwner = computed(() => auth.state.role === 'owner');
const isViewer = computed(() => auth.state.role === 'viewer');
const showCookieWarning = computed(() => isOwner.value && app.backendAvailable && auth.cookieStatusLoaded && !auth.isCookieValid && !cookieWarningDismissed.value);
const cookieWarningText = computed(() => {
  switch (auth.cookieStatus.state) {
    case 'risk_control': return 'B 站暂时限制了状态检查，保留当前 Cookie，请稍后重试。';
    case 'unreachable': return '暂时无法连接 B 站，保留当前 Cookie，请稍后重试。';
    case 'malformed': return 'B 站返回的数据暂时无法识别，请稍后重试。';
    default: return auth.cookieStatus.has_cookies
      ? 'B 站登录已失效：请立即重新登录，否则仅能下载低清晰度视频。'
      : '未登录 B 站账号：博主搜索不可用，仅能下载低清晰度视频。点击右上角登录。';
  }
});
const cookieLoginPrompt = computed(() => {
  if (['risk_control', 'unreachable', 'malformed'].includes(auth.cookieStatus.state || '')) return 'B 站状态暂不可用';
  return auth.cookieStatus.has_cookies ? '登录失效·重新登录' : '未登录·点击登录';
});
const serverTitle = computed(() => {
  switch (app.serverStatus) {
    case 'connected': return '已连接到服务器';
    case 'connecting': return '正在连接服务器...';
    default: return '未连接到服务器，请检查后端服务是否已启动';
  }
});
const serverStatusText = computed(() => {
  switch (app.serverStatus) {
    case 'connected': return '已连接';
    case 'connecting': return '连接中';
    default: return '未连接';
  }
});

/** 对齐老框架 updateServerStatus：点击状态指示器给出连接反馈 toast。 */
function onServerStatusClick() {
  switch (app.serverStatus) {
    case 'connected': toast.success('服务器连接正常'); break;
    case 'connecting': toast.info('正在尝试连接服务器...'); break;
    default: toast.error('无法连接到服务器，请检查后端服务是否已启动');
  }
}

/** 先完成设备认证，再进入配置向导 / B 站登录门 / 主界面。 */
const phase = ref<'loading' | 'pair' | 'setup' | 'bili-login' | 'unavailable' | 'main'>('loading');
const tabComponents = {
  search: TabSearch,
  manual: TabManual,
  auto: TabAuto,
  history: TabHistory,
  live: TabLive,
  settings: TabSettings,
} as const;
const activeTabComponent = computed(() => tabComponents[app.currentTab]);

let enteringApp: Promise<void> | null = null;

/** 进入主界面前的统一入口检查（bootstrap / 配对成功 / 向导完成共用）：
 *  1. Setup 向导未完成 → setup；
 *  2. B 站未登录（无 Cookie）→ bili-login 登录门，扫码成功后自动进入主界面。 */
async function resolveEntryPhase(): Promise<'main' | 'setup' | 'bili-login'> {
  try {
    const setupStatus = await setupApi.status();
    if (setupStatus && !setupStatus.completed) return 'setup';
  } catch {
    // Setup 接口失败不阻塞主流程（对齐 bootstrap 的容错）。
  }
  await auth.refreshCookieStatus();
  if (!auth.cookieStatus.has_cookies) return 'bili-login';
  return 'main';
}

function enterAuthenticatedApp() {
  if (enteringApp) return enteringApp;
  enteringApp = (async () => {
    phase.value = 'loading';
    const result = await app.bootstrap();
    if (result === 'unavailable') {
      phase.value = 'unavailable';
      return;
    }
    if (result === 'pair') {
      phase.value = 'pair';
      return;
    }
    if (result === 'setup') {
      phase.value = 'setup';
      return;
    }
    phase.value = await resolveEntryPhase();
    if (phase.value !== 'main') return;
    void Promise.allSettled([download.refreshHealth(), download.refreshStatus()]);
  })().finally(() => {
    enteringApp = null;
  });
  return enteringApp;
}

onMounted(() => { void enterAuthenticatedApp(); });

function onPaired() {
  // PairView 只有在 auth/state 已确认 authenticated 后才会发出 paired。
  // 这里绝不再把页面切回 loading；配对成功只做一次稳定的 phase 切换。
  if (!auth.isAuthenticated) return;
  // 新会话必须立刻持有 CSRF token，否则进入主界面后的首个写请求就会 403。
  void auth.refreshCsrfToken();
  app.activateSession();
  void resolveEntryPhase().then((next) => {
    phase.value = next;
    if (next !== 'main') return;
    void Promise.allSettled([download.refreshHealth(), download.refreshStatus()]);
  });
}
// 设备会话 401 失效：切回配对流程（不整页 reload，保留用户现场）。
watch(() => app.sessionInvalid, (invalid) => {
  if (!invalid) return;
  app.sessionInvalid = false;
  phase.value = 'pair';
});
function onSetupDone() {
  // Setup 向导完成后同样过登录门检查
  void resolveEntryPhase().then(async (next) => {
    phase.value = next;
    if (next !== 'main') return;
    app.activateSession();
    await auth.refreshCsrfToken();
    void Promise.allSettled([download.refreshHealth(), download.refreshStatus()]);
  });
}
// 登录门：扫码成功后 refreshCookieStatus 会把 isCookieValid 置真，自动进主界面。
watch(() => auth.isCookieValid, (valid) => {
  if (!valid || phase.value !== 'bili-login') return;
  phase.value = 'main';
  app.activateSession();
  void auth.refreshCsrfToken();
  void Promise.allSettled([download.refreshHealth(), download.refreshStatus()]);
});
function retryConnection() { void enterAuthenticatedApp(); }

function imageError(event: Event) {
  const image = event.target as HTMLImageElement;
  image.hidden = true;
  image.nextElementSibling?.removeAttribute('hidden');
}

function imageUrl(url?: string) {
  return url ? videoApi.proxyImage(url) : '';
}

const viewerLocalActions = new Set([
  'switch-tab', 'switch-board-tab', 'switch-live-board',
  'close-video-drawer', 'close-blogger-modal', 'close-add-blogger-modal',
  'close-edit-blogger-modal', 'close-blogger-notice-modal', 'close-qr-modal',
  'dismiss-network-toast', 'dismiss-cookie-warning', 'toggle-manual-cookie',
  'add-time-point', 'remove-time-point', 'select-quality', 'toggle-all-pages',
  'toggle-page', 'toggle-all-episodes', 'browser-download-master', 'browser-download-check',
  'toggle-file',
]);

function guardViewerMutation(event: Event) {
  if (!isViewer.value) return;
  const source = event.target instanceof Element ? event.target : null;
  if (!source) return;
  const actionTarget = source.closest<HTMLElement>('[data-action]');
  const action = actionTarget?.dataset.action;
  const formControl = source.closest('input, select, textarea');
  const lockedTarget = source.closest('[data-mutating], [data-network-required]');
  // Viewer 仍可操作不会触发后端写入的本地表单控件（例如从已添加博主载入配置）。
  if (source.closest('[data-viewer-local]')) return;
  if (!action && !formControl && !lockedTarget) return;
  if (action && viewerLocalActions.has(action)) return;
  event.preventDefault();
  event.stopPropagation();
}

/** 老框架 network.js LOCAL_ONLY_ACTIONS：离线/后端不可用时仍可用的纯本地 UI 操作
 * （关弹窗、切视图、本地编辑与勾选）。其余一律拦截。 */
const networkLocalActions = new Set([
  'switch-tab', 'switch-board-tab',
  'close-video-drawer', 'close-blogger-modal', 'close-add-blogger-modal',
  'close-edit-blogger-modal', 'close-qr-modal', 'close-blogger-notice-modal',
  'dismiss-network-toast', 'dismiss-cookie-warning',
  'toggle-manual-cookie', 'add-time-point', 'remove-time-point',
  'select-quality', 'toggle-all-pages', 'toggle-all-episodes',
  'browser-download-master', 'browser-download-check',
]);

/** 老框架 network.js updateNetworkDisabledButtons 的三类禁用目标：
 * data-action（白名单外）、data-network-required（直播页等无 data-action 控件）、
 * 独立按钮 id（搜索/刷新等）。 */
const NETWORK_GUARD_SELECTOR = [
  '[data-action]', '[data-network-required]',
  '#blogger-search-btn', '#manual-query-btn', '#manual-resolve-btn',
  '#show-add-blogger-btn', '#detail-start-btn', '#detail-stop-btn', '#board-refresh-btn',
].join(', ');

/** 对齐老框架 network.js：离线/后端不可用时拦截需要后端的控件，
 * 并通过 checkNetworkBeforeAction 弹出唯一持续离线 toast。 */
function guardNetworkAction(event: Event) {
  if (!app.networkControlsLocked) return;
  const source = event.target instanceof Element ? event.target : null;
  if (!source) return;
  const locked = source.closest<HTMLElement>(NETWORK_GUARD_SELECTOR);
  if (!locked) return;
  // 纯本地 UI 操作（关弹窗/切视图/本地勾选）放行。
  if (locked.dataset.action && networkLocalActions.has(locked.dataset.action)) return;
  event.preventDefault();
  event.stopPropagation();
  app.checkNetworkBeforeAction();
}

/** 主容器统一捕获点击：网络降级守卫与 viewer 只读守卫共用一个 capture 入口。 */
function onContainerClickCapture(event: Event) {
  guardNetworkAction(event);
  guardViewerMutation(event);
}

watch(() => auth.isAuthenticated, (authenticated) => {
  if (phase.value !== 'main' || authenticated) return;
  app.disconnectSocket();
  phase.value = 'pair';
});

</script>

<template>
  <a class="skip-link" href="#main-content">跳转到主要内容</a>

  <!-- 初始化阶段保持稳定占位，避免认证请求完成前出现空白闪变。 -->
  <div v-if="phase === 'loading'" class="app-loading" aria-live="polite">
    <span class="loading" aria-hidden="true"></span>
    <span>正在连接服务…</span>
  </div>

  <div v-else-if="phase === 'unavailable'" class="app-unavailable" role="alert">
    <i class="fa-solid fa-plug-circle-xmark" aria-hidden="true"></i>
    <h1>暂时无法连接服务</h1>
    <p>后端尚未就绪，配对状态不会被误判为未配对。</p>
    <button class="btn btn-primary" @click="retryConnection">重新连接</button>
  </div>

  <!-- Setup 向导：首次启动未完成时显示 -->
  <SetupView v-else-if="phase === 'setup'" @done="onSetupDone" />

  <PairView v-else-if="phase === 'pair'" @paired="onPaired" />

  <!-- B 站登录门：未登录 B 站不允许进入主界面，扫码成功后自动放行。 -->
  <div v-else-if="phase === 'bili-login'" class="app-unavailable" role="alert">
    <i class="fa-brands fa-bilibili" aria-hidden="true"></i>
    <h1>请先登录 B 站账号</h1>
    <p>博主搜索、视频下载等功能需要登录 B 站后才能使用。</p>
    <button id="bili-login-gate-btn" class="btn btn-primary" @click="app.openCookieLogin()">扫码登录 B 站</button>
  </div>

  <div v-else-if="phase === 'main'" class="container" :class="{ 'session-viewer': isViewer, 'session-operator': auth.state.role === 'operator', 'network-degraded': app.networkControlsLocked }"
       @click.capture="onContainerClickCapture" @beforeinput.capture="guardViewerMutation" @change.capture="guardViewerMutation">
    <header class="header">
      <div class="header-left">
        <h1>补哩补哩 <span class="brand-latin">bulibuli</span></h1>
        <p>下架之前，先下为敬。　博主搜索 / 手动查询 / 自动任务 / 下载管理 / 直播 / 设置</p>
      </div>
      <div class="header-right">
        <div id="login-user-card" class="login-user-card" :hidden="!isOwner || !auth.isCookieValid">
          <template v-if="auth.biliUser?.face">
            <img :src="imageUrl(auth.biliUser.face)" class="login-user-face" alt="avatar" @error="imageError" />
            <span class="login-user-face login-user-face-ph" hidden><i class="fa-solid fa-user"></i></span>
          </template>
          <span v-else class="login-user-face login-user-face-ph"><i class="fa-solid fa-user"></i></span>
          <div class="login-user-meta">
            <span class="login-user-name">{{ auth.biliUser?.name || '已登录' }}</span>
            <span class="login-user-sub">Lv{{ auth.cookieStatus.level || 0 }}<span v-if="auth.cookieStatus.vip_status"> · {{ auth.cookieStatus.vip_label || '大会员' }}</span></span>
          </div>
          <button class="login-switch-btn" title="切换账号" @click="app.openCookieLogin()"><i class="fa-solid fa-right-left"></i></button>
        </div>
        <button id="login-prompt-btn" class="login-prompt-btn" :hidden="!isOwner || !auth.cookieStatusLoaded || auth.isCookieValid" @click="app.openCookieLogin()">
          <i class="fa-solid fa-user"></i> {{ cookieLoginPrompt }}
        </button>
        <span id="server-status-indicator" class="server-status" :class="app.serverStatus" :title="serverTitle" @click="onServerStatusClick">
          <span class="server-status-dot"></span>
          <span class="server-status-text">{{ serverStatusText }}</span>
        </span>
      </div>
    </header>

    <div v-if="showCookieWarning" id="cookie-warning-banner" class="cookie-warning-banner">
      <i class="fa-solid fa-exclamation-triangle"></i>
      <span>{{ cookieWarningText }}</span>
      <button id="cookie-warning-settings-link" class="cookie-warning-settings-link" @click="app.setTab('settings')">点击前往设置</button>
      <button class="cookie-warning-close" @click="cookieWarningDismissed = true" title="关闭">×</button>
    </div>

    <div v-if="app.networkBannerVisible" id="network-error-banner" class="network-error-banner show">
      <i class="fa-solid fa-wifi network-banner-icon"></i>
      <span>网络连接异常，部分功能不可用，请检查网络或后端服务状态</span>
    </div>

    <nav class="nav-tabs" role="tablist" aria-label="主要功能">
      <button v-for="t in tabs" :key="t.id"
              :class="['nav-tab', { active: app.currentTab === t.id }]"
              :id="`tab-${t.id}-label`"
              :data-tab="t.id"
              role="tab"
              :aria-selected="app.currentTab === t.id"
              :aria-controls="`tab-${t.id}`"
              :tabindex="app.currentTab === t.id ? 0 : -1"
              @click="app.setTab(t.id)">
        <i class="fa-solid" :class="t.icon"></i> {{ t.label }}
      </button>
    </nav>

    <main id="main-content">
      <!-- 只挂载当前 Tab；KeepAlive 保留表单状态，同时停止隐藏页面的轮询。 -->
      <KeepAlive>
        <component :is="activeTabComponent" class="active"
                   :id="`tab-${app.currentTab}`" role="tabpanel"
                   :aria-labelledby="`tab-${app.currentTab}-label`" />
      </KeepAlive>
    </main>

    <VideoDrawer />
    <ConfirmDialogList />
  </div>

  <!-- 扫码登录弹窗与 toast 挂根层级：配对/向导/登录门阶段也需要（扫码成功自动进主界面）。 -->
  <CookieLoginModals />
  <ToastList />
</template>

<style scoped>
.skip-link { position: absolute; left: -9999px; }
.skip-link:focus { left: 8px; top: 8px; background: var(--bulibuli-brand, #00aeec); color: #fff; padding: 6px 12px; z-index: 9999; }
.app-loading { min-height: 100vh; min-height: 100dvh; display: grid; place-items: center; gap: 10px; color: var(--text-secondary); }
.app-unavailable { min-height: 100vh; min-height: 100dvh; display: grid; place-items: center; align-content: center; gap: 10px; padding: 24px; color: var(--text-secondary); text-align: center; }
.app-unavailable > i { color: var(--error); font-size: 32px; }
.app-unavailable h1 { margin: 0; color: var(--text); font-size: 22px; }
.app-unavailable p { margin: 0 0 8px; }
.migration-links { margin-top: 6px; font-size: 12px; }
.migration-links a { color: var(--bulibuli-brand, #00aeec); text-decoration: none; }
</style>
