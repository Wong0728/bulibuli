/**
 * 更新管理（对齐老框架 checkUpdate / applyUpdate：无 toast，结果展示在 resultEl）。
 * 从 TabSettings.vue 抽出的单例：进入设置页时 TabSettings 刷新版本状态，
 * 「更新」分组子组件消费状态并触发检查/应用。
 */
import { ref } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';

const applyingUpdate = ref(false);
/** 老框架：loadUpdateStatus 失败静默，版本框保留"正在获取..."初始文案。 */
const updateStatusLoaded = ref(false);
/** 老框架 resultEl.innerHTML 的结构化等价：图标 + 色调 + 文案。 */
export type ResultHint = { tone: string; icon: string; spin?: boolean; text: string };
const updateCheckResult = ref<ResultHint | null>(null);

export function useSettingsUpdate() {
  const settings = useSettingsStore();
  const toast = useToastStore();

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

  return { applyingUpdate, updateStatusLoaded, updateCheckResult, onCheckUpdate, onApplyUpdate };
}
