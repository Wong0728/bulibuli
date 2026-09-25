/**
 * 基础配置只读摘要（对齐老框架 settings.js loadFoundationSummary / loadAiSkillInfo）。
 * 从 TabSettings.vue 抽出的单例：进入设置页时由 TabSettings 触发加载，
 * 「本机配置」分组子组件消费状态。
 */
import { ref, computed } from 'vue';
import { foundation as foundationApi } from '@/api';
import { useToastStore } from '@/stores/toast';

const foundationStatus = ref<any>(null);
/** 基础配置摘要读取失败文案（老框架 catch：基础配置状态读取失败：...）。 */
const foundationError = ref('');

export function useFoundationSummary() {
  const toast = useToastStore();

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

  return { foundationStatus, foundationError, loadFoundationSummary, foundationModeName, copyAiSkillPath };
}
