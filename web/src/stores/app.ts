/**
 * 全局应用状态：tab 切换、WS 状态、网络降级、登录卡片等所有"页面无关"的全局信号。
 *
 * 对齐老框架（static/js）的全局反馈链：
 * - network.js   → 网络降级体系：横幅三条件、持续离线 toast、按钮禁用、恢复清账。
 * - bootstrap.js → window online/offline 监听 + 网络恢复静默 retry-all（5 分钟窗口）。
 * - core.js + download-status.js + blogger.js → WS 断开后 HTTP 兜底轮询
 *   （下载进度 1.5s / 博主日志 2s），WS 重连后停止。
 * - core.js:342-377 → 下载终态 toast + 300ms/800ms 防抖刷看板。
 * - showSystemModal → 风控 / 登录过期 / 磁盘满 模态框（共享确认弹窗队列）。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { io, Socket } from 'socket.io-client';
import { auth as authApi, health, setup as setupApi, download as downloadApi, logs as logsApi } from '@/api';
import { setApiErrorHandlers, setNetworkHooks, NETWORK_ERR_MSG } from '@/api/client';
import { acceptWebSocketMessage } from '@/utils/ws-dedupe';
import { confirmDialog, closeConfirmByTitle } from '@/composables/confirm';
import { useToastStore } from './toast';
import { useAuthStore } from './auth';
import { useDownloadStore } from './download';
import { useLiveStore } from './live';
import { useBloggerStore } from './blogger';
import { useHistoryStore } from './history';

export type ServerStatus = 'connecting' | 'connected' | 'disconnected';

/** 老框架 network.js：单次网络失败即触发降级。 */
const NETWORK_FAIL_THRESHOLD = 1;
/** 下载终态集合（core.js:346），与后端发射点一一对应：
 * completion.rs("completed") / post_process.rs("merged"|"merge_failed") / monitor.rs|engine.rs("failed")。 */
const TERMINAL_STATUSES = new Set(['completed', 'merged', 'failed', 'merge_failed']);
/** 网络恢复后 retry-all 的回溯窗口（老框架 bootstrap.js：5 分钟）。 */
const RETRY_ALL_WINDOW_MS = 5 * 60 * 1000;

export const useAppStore = defineStore('app', () => {
  const toast = useToastStore();
  const currentTab = ref<'search' | 'manual' | 'auto' | 'history' | 'live' | 'settings'>('search');
  const serverStatus = ref<ServerStatus>('connecting');
  const wsConnected = ref(false);
  const backendAvailable = ref(true);
  // 全局"扫码登录弹窗"开关：顶部登录按钮 / 设置页"扫码登录"按钮共享。
  const cookieLoginVisible = ref(false);
  const riskNotice = ref<string | null>(null);
  const authExpired = ref(false);

  // --- 网络降级体系（network.js 对齐） ---
  const networkOnline = ref(navigator.onLine !== false);
  const networkFailCount = ref(0);

  /** 横幅三条件：离线 / 后端不可用 / 失败计数达到阈值。 */
  const networkBannerVisible = computed(() =>
    !networkOnline.value || !backendAvailable.value || networkFailCount.value >= NETWORK_FAIL_THRESHOLD);
  /** 离线或后端不可用时锁定所有需要后端的控件（视觉 + 点击拦截由 App.vue/CSS 配合）。 */
  const networkControlsLocked = computed(() => !networkOnline.value || !backendAvailable.value);
  /** 唯一持续离线 toast 是否在场（id 由 toast store 追踪并归一化）。 */
  const networkToastVisible = computed(() => toast.networkToastVisible);

  let socket: Socket | null = null;
  let socketIntentionallyClosed = false;
  const downloadStore = () => useDownloadStore();
  const liveStore = () => useLiveStore();
  const authStore = () => useAuthStore();
  const bloggerStore = () => useBloggerStore();
  const historyStore = () => useHistoryStore();

  // --- 老框架 core.js showSystemModal：风控 / 登录过期 / 磁盘满统一模态框 ---
  async function showSystemModal(title: string, message: string, onConfirm: (() => void) | null = null) {
    // 对齐老框架 confirmDialog：新弹窗顶掉同标题旧弹窗，避免堆叠。
    closeConfirmByTitle(title);
    const confirmed = await confirmDialog({
      title,
      message,
      confirmText: onConfirm ? '立即处理' : '知道了',
      cancelText: '关闭',
    });
    if (confirmed && onConfirm) onConfirm();
  }

  // --- 网络降级体系（network.js 对齐） ---

  /** 唯一的离线提示：全屏期间只保留这一条持续 toast（老框架 showNetworkToast）。
   * 任意入口 push(NETWORK_ERR_MSG) 都会被 toast store 归一化合并到这一条。 */
  function showNetworkToast() {
    toast.warn(NETWORK_ERR_MSG, 0);
  }

  function dismissNetworkToast() {
    toast.dismissNetworkToast();
  }

  /** 离线时点击被禁用控件的统一入口（老框架 checkNetworkBeforeAction）。 */
  function checkNetworkBeforeAction(): boolean {
    if (networkOnline.value && backendAvailable.value) return true;
    showNetworkToast();
    return false;
  }

  /** 后端响应异常时标记不可用（老框架 setBackendAvailability）。 */
  function setBackendAvailable(ok: boolean) {
    backendAvailable.value = ok;
    if (!ok) networkFailCount.value = Math.max(networkFailCount.value, NETWORK_FAIL_THRESHOLD);
  }

  /** 任一请求成功后的清账（老框架 onNetworkRecovered）。 */
  function onNetworkRecovered() {
    networkFailCount.value = 0;
    networkOnline.value = true;
    backendAvailable.value = true;
    dismissNetworkToast();
  }

  /** 网络层请求失败（fetch 抛错 / 响应无法解析）时的降级（老框架 apiRequest catch）。 */
  function registerNetworkFailure() {
    networkFailCount.value++;
    networkOnline.value = false;
    showNetworkToast();
  }

  // --- HTTP 层钩子注册（对齐老框架 core.js apiRequest 的全局副作用） ---
  setNetworkHooks({
    onRequestSuccess: () => onNetworkRecovered(),
    onRequestNetworkFailure: () => registerNetworkFailure(),
    onInvalidResponse: () => setBackendAvailable(false),
  });

  // --- 401/403 全局处理（对齐老框架 core.js apiRequest 的 handlers） ---
  setApiErrorHandlers({
    onUnauthorized: async (error, url) => {
      // 设备会话 401：整页 reload，回到配对/入口流程（老框架 window.location.reload()）。
      if (error.status === 401) {
        window.location.reload();
        return;
      }
      // B 站凭证过期（envelope code -101，HTTP 非 401）：登录过期模态框 + 一键扫码。
      if (url === '/api/auth/state') return;
      authExpired.value = true;
      void authStore().refreshCookieStatus();
      await showSystemModal('登录已过期', error.message || '登录凭证已失效，请重新登录。', () => {
        cookieLoginVisible.value = true;
      });
      authExpired.value = false;
    },
    onRiskControl: async (error) => {
      riskNotice.value = error.message;
      await showSystemModal('B站风控', error.message || '请求触发 B站风控，请稍后重试。');
    },
  });

  // --- WS 断开后的 HTTP 兜底轮询（core.js / download-status.js / blogger.js 对齐） ---
  let progressPollTimer: ReturnType<typeof setTimeout> | null = null;
  let progressPollGeneration = 0;
  let progressPollInFlight = false;
  let logPollTimer: ReturnType<typeof setInterval> | null = null;
  let logPollInFlight = false;

  /** 对齐老框架 loadDownloadStatus：并行拉状态快照 + aria2 健康。 */
  async function pollDownloadStatus() {
    if (progressPollInFlight) return;
    progressPollInFlight = true;
    try {
      if (!document.hidden) {
        await Promise.all([downloadStore().refreshStatus(), downloadStore().refreshHealth()]);
      }
    } finally {
      progressPollInFlight = false;
    }
  }

  /** 对齐老框架 startProgressUpdates：WS 在线 10s / 断开 1.5s。 */
  function startProgressPolling() {
    if (progressPollTimer) return;
    const generation = ++progressPollGeneration;
    const poll = async () => {
      if (generation !== progressPollGeneration) return;
      await pollDownloadStatus();
      if (generation !== progressPollGeneration) return;
      progressPollTimer = setTimeout(poll, wsConnected.value ? 10000 : 1500);
    };
    void poll();
  }

  function stopProgressPolling() {
    progressPollGeneration++;
    if (progressPollTimer) clearTimeout(progressPollTimer);
    progressPollTimer = null;
  }

  /** 对齐老框架 loadBloggerLogs：替换当前选中博主的日志列表。 */
  async function pollBloggerLogs() {
    const uid = authStore().subscribedBloggerUid;
    if (uid == null) return;
    if (logPollInFlight) return;
    logPollInFlight = true;
    try {
      const result: any = await logsApi.blogger(uid, 100);
      const logs = result?.logs;
      if (!Array.isArray(logs)) return;
      const auth = authStore();
      auth.clearBloggerLogs();
      // 后端 timestamp 是秒；与 settings store 的日志归一化保持一致。
      for (const l of logs) {
        auth.appendBloggerLog({
          ts: Number(l.timestamp ?? l.ts ?? 0) * (Number(l.timestamp ?? 0) > 0 ? 1000 : 1),
          level: l.level ?? 'info',
          message: l.msg ?? l.message ?? '',
        });
      }
    } catch {
      // 静默：轮询失败由网络降级体系统一提示。
    } finally {
      logPollInFlight = false;
    }
  }

  /** 对齐老框架 startLogRefresh：每 2s 刷新当前选中博主日志。 */
  function startLogPolling() {
    if (logPollTimer) return;
    logPollTimer = setInterval(() => { void pollBloggerLogs(); }, 2000);
  }

  function stopLogPolling() {
    if (logPollTimer) clearInterval(logPollTimer);
    logPollTimer = null;
  }

  function startFallbackPolling() {
    startProgressPolling();
    if (authStore().subscribedBloggerUid != null) startLogPolling();
  }

  function stopFallbackPolling() {
    stopProgressPolling();
    stopLogPolling();
  }

  // --- 下载终态 toast + 防抖刷看板（core.js:342-377 对齐） ---
  /** 任务状态内存表（老框架 manualDownloadProgress 的终态判断部分）。 */
  const lastProgressStatus = new Map<string, string>();
  let boardRefreshTimer: ReturnType<typeof setTimeout> | null = null;

  function progressTitle(bvid: string, type: string): string {
    for (const task of downloadStore().tasks.values()) {
      if (task.bvid === bvid && (task.type || 'video') === type) return task.title || bvid;
    }
    return bvid;
  }

  function connectSocket() {
    if (socket) return;
    socketIntentionallyClosed = false;
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
      if (uid) socket?.emit('blogger:logs:subscribe', { uid: String(uid) });
      // WS 已连接，停止 HTTP 轮询回退（老框架 core.js connect）。
      stopFallbackPolling();
    });

    socket.on('disconnect', () => {
      wsConnected.value = false;
      serverStatus.value = 'disconnected';
      // 主动断开（会话注销/退出）不启动兜底轮询。
      if (socketIntentionallyClosed) return;
      // WS 断开，启动 HTTP 轮询回退（老框架 core.js disconnect）。
      startFallbackPolling();
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
      const bvid = String(data?.bvid ?? '');
      const taskType = data?.type || 'video';
      const stateKey = `${bvid}_${taskType}`;
      const oldStatus = lastProgressStatus.get(stateKey);
      const status = String(data?.status ?? '');

      downloadStore().applyWsProgress(data);

      // 对终态变化显示 toast 通知（不论用户在哪个页面）。
      let terminalTransition = false;
      if (oldStatus && oldStatus !== status) {
        terminalTransition = TERMINAL_STATUSES.has(status);
        const title = progressTitle(bvid, taskType);
        if (status === 'completed') {
          toast.success(`下载完成: ${title}`);
        } else if (status === 'merged') {
          toast.success(`合并完成: ${title}`);
        } else if (status === 'failed' || status === 'merge_failed') {
          const errorMsg = data?.error || data?.message || (status === 'merge_failed' ? '合并失败' : '下载失败');
          toast.error(`${status === 'merge_failed' ? '合并失败' : '下载失败'}: ${bvid} - ${errorMsg}`);
        }
      }

      // 新任务或终态变化时防抖刷新看板（仅当前在下载管理页）。
      if (!oldStatus || (oldStatus !== status && terminalTransition)) {
        if (currentTab.value === 'history') {
          if (boardRefreshTimer) clearTimeout(boardRefreshTimer);
          boardRefreshTimer = setTimeout(() => {
            void historyStore().loadBoard(historyStore().activeTab || 'completed');
            if (terminalTransition) void downloadStore().refreshStatus();
          }, terminalTransition ? 300 : 800);
        }
      }

      if (status) lastProgressStatus.set(stateKey, status);
    });

    // 后端不发 live:update：直播看板由 TabLive 的 5 秒轮询 dashboard 驱动。

    socket.on('bili:risk-control', (data: any) => {
      if (!acceptWebSocketMessage('bili:risk-control', data)) return;
      // 对齐老框架：风控弹模态框（riskNotice 供直播页横幅继续消费）。
      const message = data?.message || '请求触发 B站风控，请稍后重试。';
      riskNotice.value = message;
      void showSystemModal('B站风控', message);
    });

    socket.on('bili:auth-expired', (data: any) => {
      if (!acceptWebSocketMessage('bili:auth-expired', data)) return;
      // 这是 B 站凭证过期，不是 bulibuli 设备会话失效。老框架：弹"登录已过期"
      // 模态框，确认后一键打开扫码登录；不能把 authenticated 清成 false。
      authExpired.value = true;
      void authStore().refreshCookieStatus();
      void showSystemModal('登录已过期', data?.message || '登录凭证已失效，请重新登录。', () => {
        cookieLoginVisible.value = true;
      }).finally(() => { authExpired.value = false; });
    });

    socket.on('download:disk-full', (data: any) => {
      if (!acceptWebSocketMessage('download:disk-full', data)) return;
      // 对齐老框架：磁盘满弹模态框（不再只记一条博主日志）。
      void showSystemModal('磁盘空间不足', data?.message || '下载已暂停，请释放磁盘空间后重试。');
    });

    socket.on('download:disk-recovered', (data: any) => {
      if (!acceptWebSocketMessage('download:disk-recovered', data)) return;
      // 对齐老框架：磁盘恢复后自动收掉"磁盘空间不足"弹窗。
      closeConfirmByTitle('磁盘空间不足');
    });

    // 审计事件桥（src/ws/mod.rs::start_audit_event_bridge）：AI/TUI/其他端的写操作
    // 成功后广播，前端据此刷新对应区域保持一致。target_type 取值见
    // src/models/operation_log.rs::OperationTarget。
    socket.on('audit:event', (data: any) => {
      if (!acceptWebSocketMessage('audit', data)) return;
      if (data?.outcome !== 'success') return;
      const target = String(data?.target_type ?? '');
      if (target === 'blogger' || target === 'task') {
        void bloggerStore().refreshList().catch(() => { /* 静默 */ });
        void bloggerStore().refreshAllStatus();
      } else if (target === 'live_source') {
        void liveStore().refreshDashboard();
      }
      // 下载进度有自己的 WS 通道；settings/cookie/session 不在此处刷新避免自触发循环。
    });

    // 启动即开启兜底轮询（对齐老框架 bootstrap 的 startProgressUpdates）；
    // WS 首次连接成功后由 connect 处理器停止。
    startFallbackPolling();
  }

  function disconnectSocket() {
    socketIntentionallyClosed = true;
    stopFallbackPolling();
    socket?.disconnect();
    socket = null;
    wsConnected.value = false;
    serverStatus.value = 'disconnected';
  }

  // --- window 网络监听（老框架 bootstrap.js bindNetworkListeners 对齐） ---
  function bindNetworkListeners() {
    window.addEventListener('offline', () => {
      networkOnline.value = false;
      toast.warn('网络已断开，部分功能已禁用');
    });
    window.addEventListener('online', () => {
      networkFailCount.value = 0;
      networkOnline.value = true;
      toast.success('网络已恢复');
      if (!authStore().isAuthenticated) return;
      // 网络恢复：刷新看板与下载列表 + 静默重试断线期间失败的任务（5 分钟窗口）。
      void historyStore().loadBoard(historyStore().activeTab || 'completed');
      void downloadStore().refreshStatus();
      const offlineSince = Math.floor((Date.now() - RETRY_ALL_WINDOW_MS) / 1000);
      // 后端 retry_all 只解析 URL query string（Query<RetryAllQuery>），
      // since 必须放 query 而非 JSON body，否则会被静默忽略。
      downloadApi.retryAll(offlineSince).catch(() => { /* 静默 */ });
    });
    window.addEventListener('beforeunload', () => {
      // 对齐老框架 core.js beforeunload：停轮询 + 断开 WS。
      stopFallbackPolling();
      socket?.disconnect();
    });
  }
  bindNetworkListeners();

  /** 配对页已确认会话后只启动主界面的后台通道，不再重复请求入口状态。 */
  function activateSession() {
    connectSocket();
    void authStore().refreshCookieStatus();
  }

  async function bootstrap(): Promise<'authenticated' | 'pair' | 'setup' | 'unavailable'> {
    // 先确认 API/数据库可响应，再读取设备会话；ready 的 degraded 状态
    // 只表示运行时依赖不完整，不应阻止配对或进入主界面。
    try {
      await health.ready();
      const state = await authApi.state();
      if (!state) throw new Error('认证状态不可用');
      authStore().setAuthState(state as any);
    } catch {
      setBackendAvailable(false);
      return 'unavailable';
    }
    if (!authStore().isAuthenticated) return 'pair';

    // 检查 Setup 向导是否完成：首次启动未配置时引导用户走三步向导。
    try {
      const setupStatus: any = await setupApi.status();
      if (setupStatus && !setupStatus.completed) return 'setup';
    } catch {
      // Setup 接口失败不阻塞主流程，跳过向导
    }

    // 旧版先进入管理页，Cookie 状态在后台检查；B 站上游慢时不能阻塞整个 App。
    activateSession();
    return 'authenticated';
  }

  function setTab(tab: typeof currentTab.value) {
    currentTab.value = tab;
  }

  return {
    currentTab,
    serverStatus,
    wsConnected,
    backendAvailable,
    networkOnline,
    networkFailCount,
    networkToastVisible,
    networkBannerVisible,
    networkControlsLocked,
    cookieLoginVisible,
    riskNotice,
    authExpired,
    bootstrap,
    setTab,
    showNetworkToast,
    dismissNetworkToast,
    checkNetworkBeforeAction,
    setBackendAvailable,
    onNetworkRecovered,
    disconnectSocket,
    activateSession,
    openCookieLogin() { cookieLoginVisible.value = true; },
    closeCookieLogin() { cookieLoginVisible.value = false; },
    dismissRiskNotice() { riskNotice.value = null; },
    subscribeBloggerLogs(uid: number) {
      const previous = authStore().subscribedBloggerUid;
      if (previous && previous !== uid) {
        socket?.emit('blogger:logs:unsubscribe', { uid: String(previous) });
      }
      authStore().subscribedBloggerUid = uid;
      socket?.emit('blogger:logs:subscribe', { uid: String(uid) });
      // 对齐老框架 selectBlogger → startLogRefresh：WS 未连接时启动 HTTP 兜底轮询。
      if (!wsConnected.value) startLogPolling();
    },
    unsubscribeBloggerLogs() {
      const uid = authStore().subscribedBloggerUid;
      authStore().subscribedBloggerUid = null;
      if (uid) socket?.emit('blogger:logs:unsubscribe', { uid: String(uid) });
      stopLogPolling();
    },
  };
});
