/**
 * 直播弹幕烧录任务跟踪（P0-2：确认框 + burn/status 3s 轮询至 30 分钟 + 终态 toast）。
 * 从 TabLive.vue 抽出：定时器句柄 + generation 记录在模块内，
 * 组件卸载时停止轮询，避免泄漏。
 */
import { onUnmounted } from 'vue';
import { useLiveStore } from '@/stores/live';
import { useToastStore } from '@/stores/toast';
import { download as downloadApi } from '@/api';

export function useLiveBurnTracking() {
  const live = useLiveStore();
  const toast = useToastStore();

  let burnPollTimer: number | null = null;
  let burnPollGeneration = 0;

  function stopBurnTaskTracking() {
    burnPollGeneration += 1;
    if (burnPollTimer !== null) {
      window.clearTimeout(burnPollTimer);
      burnPollTimer = null;
    }
  }

  function trackBurnTask(taskId: string) {
    const generation = ++burnPollGeneration;
    const startedAt = Date.now();
    const poll = async () => {
      if (generation !== burnPollGeneration) return;
      try {
        const response: any = await downloadApi.burnStatus(taskId);
        if (generation !== burnPollGeneration) return;
        const status = response?.status;
        if (status === 'completed') {
          toast.success('弹幕烧录完成，已生成弹幕版视频');
          await live.refreshDashboard();
        } else if (status === 'failed') {
          toast.error(`弹幕烧录失败：${response?.message || '未知错误'}`);
        } else if (Date.now() - startedAt > 30 * 60 * 1000) {
          toast.warn('弹幕烧录超时，请到录制目录确认结果');
        } else {
          burnPollTimer = window.setTimeout(poll, 3000);
        }
      } catch (error) {
        console.error('[live] 查询烧录任务状态失败：', error);
        // 网络异常同样受 30 分钟上限约束，避免后端不可达时无限轮询
        if (Date.now() - startedAt > 30 * 60 * 1000) toast.warn('弹幕烧录超时，请到录制目录确认结果');
        else burnPollTimer = window.setTimeout(poll, 3000);
      }
    };
    void poll();
  }

  onUnmounted(stopBurnTaskTracking);

  return { trackBurnTask, stopBurnTaskTracking };
}
