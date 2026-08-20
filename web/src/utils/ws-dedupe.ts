/**
 * WebSocket 消息去重：保留与原 state.js 中 `acceptWebSocketMessage` 一致的语义。
 *
 * - 后端通过 socketioxide 推消息时会自动塞一个 `id`（见 src/ws/mod.rs 的
 *   `broadcast_download_progress` / `broadcast_log` / `broadcast_system`）。
 * - 老系统/某些系统事件可能不带 `id`，或相同 id 在 200ms 内的重复事件是合法的
 *   （progress 推送），这里允许"无 id"的消息直接放行（不缓存），避免误丢。
 * - 带 id 的消息按 `namespace:id` 维度去重，缓存最近 2000 条。
 */
const seen = new Map<string, number>();
const MAX_SIZE = 2000;

export function webSocketMessageKey(namespace: string, message: any): string | null {
  if (!namespace) return null;
  if (!message || typeof message.id !== 'string' || message.id.length === 0) return null;
  return `${namespace}:${message.id}`;
}

export function acceptWebSocketMessage(namespace: string, message: any): boolean {
  const key = webSocketMessageKey(namespace, message);
  if (!key) {
    // 无 id 的消息不参与去重，直接放行（保留原 state.js 的 "无 id 视为重复"
    // 语义会造成所有 progress 推送被吞没）。
    return true;
  }
  if (seen.has(key)) return false;
  seen.set(key, Date.now());
  if (seen.size > MAX_SIZE) {
    const oldest = seen.keys().next().value;
    if (oldest) seen.delete(oldest);
  }
  return true;
}

/** 暴露给测试或调试用：清空去重缓存。 */
export function _resetWsDedupe() {
  seen.clear();
}
