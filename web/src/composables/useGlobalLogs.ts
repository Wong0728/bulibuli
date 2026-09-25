/**
 * 全局日志 15s 轮询（对齐老框架 startGlobalLogsPolling，对所有已认证角色开放）。
 * 从 TabSettings.vue 抽出的单例：TabSettings 负责启停，工具分组子组件触发手动刷新。
 */
import { ref } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useAppStore } from '@/stores/app';

let logsTimer: number | null = null;
let logsInFlight = false;

export function useGlobalLogs() {
  const settings = useSettingsStore();
  const app = useAppStore();

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

  function stopGlobalLogsPolling() {
    if (logsTimer) { clearInterval(logsTimer); logsTimer = null; }
  }

  return { refreshGlobalLogs, startGlobalLogsPolling, stopGlobalLogsPolling };
}
