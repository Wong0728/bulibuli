/**
 * 全局确认弹窗（替代原 modal.js 中的 confirmDialog）。
 * 用 vue ref 注入 root 组件，通过 provide/inject 在任意组件调用。
 */
import { ref } from 'vue';

interface ConfirmRequest {
  title: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  tone?: 'default' | 'danger';
  resolve: (v: boolean) => void;
}

export const confirmDialogs = ref<ConfirmRequest[]>([]);

export function confirmDialog(opts: { title: string; message: string; confirmText?: string; cancelText?: string; tone?: 'default' | 'danger' }): Promise<boolean> {
  return new Promise((resolve) => {
    confirmDialogs.value.push({
      title: opts.title,
      message: opts.message,
      confirmText: opts.confirmText || '确定',
      cancelText: opts.cancelText || '取消',
      tone: opts.tone || 'default',
      resolve,
    });
  });
}

export function resolveConfirm(req: ConfirmRequest, value: boolean) {
  req.resolve(value);
  confirmDialogs.value = confirmDialogs.value.filter(r => r !== req);
}