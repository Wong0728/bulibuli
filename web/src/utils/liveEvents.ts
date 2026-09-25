/**
 * 直播互动事件的纯渲染工具（对齐 live.js renderEvents / renderBoard）。
 * 从 TabLive.vue 抽出：无状态、无副作用，供 TabLive 与直播子组件共用。
 */
import { videoStatusText, stopReasonText } from '@/stores/live';

export type LiveEventCategory = 'user' | 'stats' | 'system' | 'unknown';

export function eventCategory(event: any): LiveEventCategory {
  if (event.event_category) return event.event_category as LiveEventCategory;
  // split 结果可能为空数组（理论上不会），空串兜底走 unknown 分类。
  const cmd = String(event.cmd || '').split(':', 1)[0] || '';
  if (['danmaku', 'gift', 'super_chat', 'guard', 'interact', 'like', 'entry', 'link_mic_pk'].includes(event.event_type)) return 'user';
  if (['watched', 'stats'].includes(event.event_type)
    || ['WATCHED_CHANGE', 'ONLINE_RANK_V3', 'ONLINE_RANK_V2', 'ONLINE_RANK_TOP3', 'ONLINE_RANK_COUNT', 'LIKE_INFO_V3_UPDATE', 'ROOM_REAL_TIME_MESSAGE_UPDATE', 'HOT_RANK_CHANGED', 'AREA_RANK_CHANGED'].includes(cmd)) return 'stats';
  if (['INTERACT_WORD', 'INTERACT_WORD_V2', 'INTERACT_WORD_V3', 'WELCOME', 'WELCOME_GUARD', 'ENTRY_EFFECT', 'LIKE_INFO_V3_CLICK'].includes(cmd)) return 'user';
  if (['system', 'capture_gap'].includes(event.event_type)
    || ['LIVE', 'PREPARING', 'ROOM_CHANGE', 'ROOM_LOCK', 'ROOM_BLOCK_MSG', 'ROOM_SILENT_ON', 'ROOM_SILENT_OFF', 'CUT_OFF', 'STOP_LIVE_ROOM_LIST', 'NOTICE_MSG', 'COMMON_NOTICE_DANMAKU', 'WIDGET_BANNER'].includes(cmd)) return 'system';
  if (['VOICE_JOIN', 'LINK_MIC', 'LIVE_MULTI_VIEW'].some(prefix => cmd.startsWith(prefix)) || cmd.startsWith('PK_')) return 'user';
  return 'unknown';
}

export function eventTypeLabel(event: any): string {
  const map: Record<string, string> = {
    danmaku: '弹幕', gift: '礼物', super_chat: 'SC', guard: '上舰',
    link_mic_pk: '连麦 / PK', interact: '进场', watched: '看过人数',
    like: '点赞', entry: '进场特效', stats: '统计', system: '系统', unknown: '未识别命令',
  };
  const label = map[event.event_type];
  if (label) return label;
  if (event.event_label) return String(event.event_label);
  const categoryMap: Record<string, string> = { user: '用户互动', stats: '统计', system: '系统', unknown: '未识别命令' };
  return categoryMap[eventCategory(event)] || '事件';
}

export function eventText(event: any): string {
  const d = event.data || {};
  const type = event.event_type;
  if (event.display_text) return String(event.display_text);
  if (type === 'danmaku') return String(d.text || '');
  if (type === 'gift') return `${d.gift_name || '礼物'} ×${d.num || 1}`;
  if (type === 'super_chat') return `SC ¥${d.price || 0}：${d.message || ''}`;
  if (type === 'guard') return `上舰 等级 ${d.guard_level || '-'}`;
  if (d.text) return String(d.text);
  const category = eventCategory(event);
  if (category === 'system') return '系统事件';
  if (category === 'unknown') return `未知命令：${event.cmd || '空命令'}`;
  return '互动事件';
}

export function mergeJobStatusText(status?: string): string {
  const map: Record<string, string> = { queued: '排队中', running: '进行中', cancelling: '取消中', completed: '已完成', failed: '失败', cancelled: '已取消' };
  return map[status || ''] || status || '—';
}

export function historyBadgeClass(h: any): string {
  return h.status === 'failed' ? 'failed' : h.status === 'completed' ? 'completed' : '';
}
export function historyBadgeText(h: any): string {
  return stopReasonText(h.stop_reason) || videoStatusText(h.status);
}
