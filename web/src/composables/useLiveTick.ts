/**
 * 直播页 1s UI tick：录制时长实时更新的共享时钟（对齐 live.js tickUi）。
 * 模块级单份 nowTick：TabLive 负责启停，子组件（房间信息卡 / 录制看板）只读。
 */
import { ref } from 'vue';
import { formatLiveDuration } from '@/stores/live';

const nowTick = ref(Date.now());
let tickTimer: number | null = null;
let tickUsers = 0;

export function useLiveTick() {
  function startTick() {
    tickUsers += 1;
    if (tickTimer) return;
    tickTimer = window.setInterval(() => { nowTick.value = Date.now(); }, 1000);
  }
  function stopTick() {
    tickUsers = Math.max(0, tickUsers - 1);
    if (tickUsers > 0 || !tickTimer) return;
    clearInterval(tickTimer);
    tickTimer = null;
  }
  /** 录制时长文案：直播中按 started_at 实时推算，否则用落库时长。 */
  function liveDurationText(r: any): string {
    if (r.started_at) return formatLiveDuration((nowTick.value - r.started_at * 1000) / 1000);
    return formatLiveDuration(r.duration_secs || 0);
  }
  return { nowTick, startTick, stopTick, liveDurationText };
}
