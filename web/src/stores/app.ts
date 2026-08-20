/**
 * 全局应用状态：tab 切换、WS 状态、网络横幅、登录卡片等所有"页面无关"的全局信号。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { io, Socket } from 'socket.io-client';
import { health, auth as authApi, cookies as cookiesApi } from '@/api';
import { acceptWebSocketMessage } from '@/utils/ws-dedupe';
import { useAuthStore } from './auth';
import { useDownloadStore } from './download';
import { useLiveStore } from './live';

export type ServerStatus = 'connecting' | 'connected' | 'disconnected';

export const useAppStore = defineStore('app', () => {
  const currentTab = ref<'search' | 'manual' | 'auto' | 'history' | 'live' | 'settings'>('search');
  const serverStatus = ref<ServerStatus>('connecting');
  const wsConnected = ref(false);
  const backendAvailable = ref(true);
  const networkToastVisible = ref(false);
  const networkBannerVisible = ref(false);
  // 全局"扫码登录弹窗"开关：顶部登录按钮 / 设置页"扫码登录"按钮共享。
  const cookieLoginVisible = ref(false);
  const riskNotice = ref<string | null>(null);
  const authExpired = ref(false);

  let socket: Socket | null = null;
  const downloadStore = () => useDownloadStore();
  const liveStore = () => useLiveStore();
  const authStore = () => useAuthStore();

  function connectSocket() {
    if (socket) return;
    socket = io({
      reconnection: true,
      reconnectionDelay: 2000,
      reconnectionDelayMax: 15000,
      timeout: 10000,
    });

    socket.on('connect', () => {
      wsConnected.value = true;
      serverStatus.value = 'connected';
      socket?.emit('download:subscribe');
      // 重新订阅博主日志
      const uid = authStore().subscribedBloggerUid;
      if (uid) socket?.emit('blogger:logs:subscribe', { uid });
    });

    socket.on('disconnect', () => {
      wsConnected.value = false;
      serverStatus.value = 'disconnected';
    });

    socket.on('connect_error', () => {
      serverStatus.value = 'connecting';
    });

    socket.on('reconnect_failed', () => {
      serverStatus.value = 'disconnected';
    });

    socket.on('log:update', (data: any) => {
      if (!acceptWebSocketMessage('log', data)) return;
      authStore().appendBloggerLog(data);
    });

    socket.on('download:progress', (data: any) => {
      if (!acceptWebSocketMessage('download', data)) return;
      // 后端 payload 是单条，不是数组（见 src/services/download/status.rs::broadcast_progress）
      downloadStore().applyWsProgress(data);
    });

    socket.on('live:update', (data: any) => {
      if (!acceptWebSocketMessage('live', data)) return;
      liveStore().applyWsUpdate(data);
    });

    socket.on('bili:risk-control', (data: any) => {
      riskNotice.value = data?.message ?? 'B站风控触发，请稍后再试';
    });
    socket.on('bili:auth-expired', (_data: any) => {
      authExpired.value = true;
      authStore().setAuthState({ authenticated: false });
    });
    socket.on('download:disk-full', (data: any) => {
      // 磁盘满：弹一个全局提示
      authStore().appendBloggerLog({ ts: Date.now(), level: 'error', message: '磁盘空间不足：' + (data?.message || '') });
    });

    socket.on('server:announce', (data: any) => {
      if (!acceptWebSocketMessage('announce', data)) return;
      // toast 由各 store 自取，这里只转发
    });
  }

  async function bootstrap() {
    connectSocket();
    // 检测后端可用性 + 拉取基础状态
    try {
      await health.ready();
      backendAvailable.value = true;
    } catch {
      backendAvailable.value = false;
      networkBannerVisible.value = true;
      return;
    }
    // 拉取认证状态
    try {
      const state = await authApi.state();
      if (state) authStore().setAuthState(state as any);
      else authStore().setAuthState({ authenticated: false });
    } catch {
      // 未认证或后端不可达，置为未登录
      authStore().setAuthState({ authenticated: false });
    }
    // 拉取 cookie 状态
    try {
      const cs = await cookiesApi.status();
      if (cs) authStore().setCookieStatus(cs as any);
      else authStore().setCookieStatus({ configured: false, valid: false });
    } catch {
      authStore().setCookieStatus({ configured: false, valid: false });
    }
  }

  function setTab(tab: typeof currentTab.value) {
    currentTab.value = tab;
  }

  function showNetworkToast(visible: boolean) {
    networkToastVisible.value = visible;
  }
  function showNetworkBanner(visible: boolean) {
    networkBannerVisible.value = visible;
    if (!visible) showNetworkToast(false);
  }
  function setBackendAvailable(ok: boolean) {
    backendAvailable.value = ok;
    if (!ok) showNetworkBanner(true);
  }

  return {
    currentTab,
    serverStatus,
    wsConnected,
    backendAvailable,
    networkToastVisible,
    networkBannerVisible,
    cookieLoginVisible,
    riskNotice,
    authExpired,
    bootstrap,
    setTab,
    showNetworkToast,
    showNetworkBanner,
    setBackendAvailable,
    openCookieLogin() { cookieLoginVisible.value = true; },
    closeCookieLogin() { cookieLoginVisible.value = false; },
    dismissRiskNotice() { riskNotice.value = null; },
    subscribeBloggerLogs(uid: number) {
      authStore().subscribedBloggerUid = uid;
      socket?.emit('blogger:logs:subscribe', { uid });
    },
    unsubscribeBloggerLogs() {
      authStore().subscribedBloggerUid = null;
      socket?.emit('blogger:logs:unsubscribe');
    },
  };
});