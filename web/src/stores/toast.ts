/**
 * Toast 通知 store：全应用共享，调用 toast.success / toast.error 即显示。
 * 对齐老框架 toast.js + download-status.js showToast：
 * - 默认时长 2700ms；error 类型至少展示 5000ms。
 * - 网络错误文案（NETWORK_ERR_MSG）统一归一为一条持续提示（duration=0）：
 *   不论入口（warn/error）只保留一条，避免离线时弹窗堆叠，恢复后由
 *   dismissNetworkToast 关闭（老框架 _state.networkToastEl 机制）。
 * - 同 type + 同文案去重，避免弹窗堆叠。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { NETWORK_ERR_MSG } from '@/api/client';

export interface ToastItem {
  id: number;
  type: 'success' | 'error' | 'info' | 'warn';
  message: string;
  duration: number;
  createdAt: number;
}

let counter = 0;
const MAX_TOASTS = 5;

export const useToastStore = defineStore('toast', () => {
  const items = ref<ToastItem[]>([]);
  /** 唯一持续网络提示的 id（老框架 _state.networkToastEl）。 */
  const networkToastId = ref<number | null>(null);

  function push(type: ToastItem['type'], message: string, duration = 2700) {
    // 老框架 download-status.js showToast：网络消息统一为一条持续提示，
    // 避免离线时不同入口的弹窗堆叠；恢复后自动消失。
    const isNetworkMessage = message === NETWORK_ERR_MSG;
    if (isNetworkMessage) duration = 0;
    const duplicate = items.value.find(item => isNetworkMessage
      ? item.message === message
      : item.type === type && item.message === message);
    if (duplicate) {
      if (isNetworkMessage) networkToastId.value = duplicate.id;
      return duplicate.id;
    }
    const id = ++counter;
    // 老框架：error 至少展示 5000ms；持续提示（<=0）不设自动关闭。
    const autoDismiss = type === 'error' && duration > 0 ? Math.max(duration, 5000) : duration;
    const item: ToastItem = { id, type, message, duration: autoDismiss, createdAt: Date.now() };
    const next = [...items.value, item];
    if (next.length > MAX_TOASTS) {
      // 挤占时优先淘汰最旧的自动消失提示；持续提示（duration<=0，如离线横幅）
      // 不被普通 toast 挤掉，只有持续提示自身超上限时才淘汰最旧的持续提示。
      const evictableIndex = next.findIndex(t => t.duration > 0);
      const [removed] = next.splice(evictableIndex >= 0 ? evictableIndex : 0, 1);
      if (removed && networkToastId.value === removed.id) networkToastId.value = null;
    }
    items.value = next;
    if (isNetworkMessage) networkToastId.value = id;
    if (autoDismiss > 0) {
      setTimeout(() => dismiss(id), autoDismiss);
    }
    return id;
  }

  function dismiss(id: number) {
    items.value = items.value.filter(t => t.id !== id);
    if (networkToastId.value === id) networkToastId.value = null;
  }

  /** 关闭唯一的持续网络提示（老框架 network.js dismissNetworkToast）。 */
  function dismissNetworkToast() {
    if (networkToastId.value == null) return;
    dismiss(networkToastId.value);
  }

  const networkToastVisible = computed(() =>
    networkToastId.value != null && items.value.some(t => t.id === networkToastId.value));

  return {
    items,
    networkToastId,
    networkToastVisible,
    success: (msg: string, d?: number) => push('success', msg, d),
    error: (msg: string, d?: number) => push('error', msg, d ?? 4000),
    info: (msg: string, d?: number) => push('info', msg, d),
    warn: (msg: string, d?: number) => push('warn', msg, d),
    dismiss,
    dismissNetworkToast,
  };
});
