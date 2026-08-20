<script setup lang="ts">
import { onMounted, computed, ref } from 'vue';
import { useAppStore } from './stores/app';
import { useAuthStore } from './stores/auth';
import { useBloggerStore } from './stores/blogger';
import { useDownloadStore } from './stores/download';
import { useLiveStore } from './stores/live';
import { useHistoryStore } from './stores/history';
import { useSetupStore } from './stores/setup';
import { useSettingsStore } from './stores/settings';
import { useToastStore } from './stores/toast';
import TabSearch from './views/TabSearch.vue';
import TabManual from './views/TabManual.vue';
import TabAuto from './views/TabAuto.vue';
import TabHistory from './views/TabHistory.vue';
import TabLive from './views/TabLive.vue';
import TabSettings from './views/TabSettings.vue';
import SetupView from './views/SetupView.vue';
import VideoDrawer from './components/VideoDrawer.vue';
import ConfirmDialogList from './components/ConfirmDialogList.vue';
import CookieLoginModals from './components/CookieLoginModals.vue';
import ToastList from './components/ToastList.vue';

const app = useAppStore();
const auth = useAuthStore();
const blogger = useBloggerStore();
const download = useDownloadStore();
const live = useLiveStore();
const history = useHistoryStore();
const setup = useSetupStore();
const settings = useSettingsStore();
const toast = useToastStore();

const tabs = [
  { id: 'search', icon: 'fa-user-plus', label: '博主搜索' },
  { id: 'manual', icon: 'fa-search', label: '手动查询' },
  { id: 'auto', icon: 'fa-bolt', label: '自动任务' },
  { id: 'history', icon: 'fa-history', label: '下载管理' },
  { id: 'live', icon: 'fa-tower-broadcast', label: '直播' },
  { id: 'settings', icon: 'fa-cog', label: '设置' },
] as const;

const aria2Status = computed(() => download.health.aria2_ok ? 'connected' : 'disconnected');
const aria2Title = computed(() => download.health.aria2_ok ? 'aria2 已连接' : 'aria2 未连接');
const showCookieWarning = computed(() => app.backendAvailable && !auth.isCookieValid);
const serverTitle = computed(() => {
  switch (app.serverStatus) {
    case 'connected': return '已连接';
    case 'connecting': return '连接中...';
    default: return '未连接';
  }
});

/** 二态：'setup' → 配置向导；'main' → 正常主界面。
 *  - setup.status.completed === false → 'setup'
 *  - 否则 → 'main'（无论是否已认证；未认证用户可在 header 点击登录）
 */
const phase = ref<'loading' | 'setup' | 'main'>('loading');

onMounted(async () => {
  await app.bootstrap();
  // 决定初始 phase
  const s = await setup.loadStatus();
  if (s && !s.completed) {
    phase.value = 'setup';
  } else {
    phase.value = 'main';
  }
  // 预拉核心数据
  if (phase.value === 'main') {
    // 主题持久化：把后端 appearance.theme 立即反映到 <html>
    try {
      await settings.load();
      const t = (settings.settings as any)?.theme || 'system';
      document.documentElement.dataset.theme = t;
    } catch { /* 静默 */ }
    Promise.allSettled([
      download.refreshHealth(),
      blogger.refreshSaved().catch(() => {}),
      live.refreshDashboard().catch(() => {}),
    ]);
  }
});

/** 监听 setup 完成事件，切换到主界面。 */
function onSetupDone() { phase.value = 'main'; }
</script>

<template>
  <a class="skip-link" href="#main-content">跳转到主要内容</a>

  <!-- Setup 阶段：替换整个主界面 -->
  <SetupView v-if="phase === 'setup'" @done="onSetupDone" />

  <div v-else-if="phase === 'main'" class="container">
    <header class="header">
      <div class="header-left">
        <h1>补哩补哩 <span class="brand-latin">bulibuli</span></h1>
        <p>下架之前，先下为敬。　博主搜索 / 手动查询 / 自动任务 / 下载管理 / 直播 / 设置</p>
      </div>
      <div class="header-right">
        <div class="login-user-card" :hidden="!auth.isAuthenticated">
          <img v-if="auth.state.user?.face" :src="auth.state.user.face" class="login-user-avatar" alt="avatar" />
          <span>{{ auth.state.user?.name || '已登录' }}</span>
        </div>
        <button class="login-prompt-btn" :hidden="auth.isAuthenticated" @click="app.openCookieLogin()">
          <i class="fa-solid fa-user"></i> 未登录·点击登录
        </button>
        <span class="server-status" :class="app.serverStatus" :title="serverTitle">
          <span class="server-status-dot"></span>
          <span class="server-status-text">{{ serverTitle }}</span>
        </span>
      </div>
    </header>

    <div v-if="showCookieWarning" class="cookie-warning-banner">
      <i class="fa-solid fa-exclamation-triangle"></i>
      <span>未配置有效的 Cookies，部分功能受限（仅能获取低清晰度视频）。</span>
      <button class="cookie-warning-settings-link" @click="app.setTab('settings')">点击前往设置</button>
      <button class="cookie-warning-close" @click="auth.setCookieStatus({ configured: true, valid: true })" title="关闭">×</button>
    </div>

    <div v-if="app.riskNotice" class="cookie-warning-banner" style="background: var(--tone-error-bg, #fff3f3); color: var(--tone-error, #c0392b);">
      <i class="fa-solid fa-shield-halved"></i>
      <span>B站风控触发：{{ app.riskNotice }}</span>
      <button class="cookie-warning-close" @click="app.dismissRiskNotice()" title="关闭">×</button>
    </div>

    <div v-if="app.networkBannerVisible" class="network-error-banner">
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
        <span v-if="t.id === 'history'" id="aria2-status-dot-history" class="aria2-dot" :class="aria2Status" :title="aria2Title"></span>
      </button>
    </nav>

    <main id="main-content">
      <TabSearch :class="{ active: app.currentTab === 'search' }" :hidden="app.currentTab !== 'search'" />
      <TabManual :class="{ active: app.currentTab === 'manual' }" :hidden="app.currentTab !== 'manual'" />
      <TabAuto :class="{ active: app.currentTab === 'auto' }" :hidden="app.currentTab !== 'auto'" />
      <TabHistory :class="{ active: app.currentTab === 'history' }" :hidden="app.currentTab !== 'history'" />
      <TabLive :class="{ active: app.currentTab === 'live' }" :hidden="app.currentTab !== 'live'" />
      <TabSettings :class="{ active: app.currentTab === 'settings' }" :hidden="app.currentTab !== 'settings'" />
    </main>

    <VideoDrawer />
    <CookieLoginModals />
    <ConfirmDialogList />
    <ToastList />
  </div>
</template>

<style scoped>
.skip-link { position: absolute; left: -9999px; }
.skip-link:focus { left: 8px; top: 8px; background: var(--bulibuli-brand, #00aeec); color: #fff; padding: 6px 12px; z-index: 9999; }
</style>