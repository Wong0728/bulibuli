/**
 * Toast 通知 store：全应用共享，调用 toast.success / toast.error 即显示。
 */
import { defineStore } from 'pinia';
import { ref } from 'vue';

export interface ToastItem {
  id: number;
  type: 'success' | 'error' | 'info' | 'warn';
  message: string;
  duration: number;
  createdAt: number;
}

let counter = 0;

export const useToastStore = defineStore('toast', () => {
  const items = ref<ToastItem[]>([]);

  function push(type: ToastItem['type'], message: string, duration = 3000) {
    const id = ++counter;
    const item: ToastItem = { id, type, message, duration, createdAt: Date.now() };
    items.value.push(item);
    if (duration > 0) {
      setTimeout(() => dismiss(id), duration);
    }
    return id;
  }

  function dismiss(id: number) {
    items.value = items.value.filter(t => t.id !== id);
  }

  return {
    items,
    success: (msg: string, d?: number) => push('success', msg, d),
    error: (msg: string, d?: number) => push('error', msg, d ?? 4000),
    info: (msg: string, d?: number) => push('info', msg, d),
    warn: (msg: string, d?: number) => push('warn', msg, d),
    dismiss,
  };
});