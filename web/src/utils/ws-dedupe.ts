/**
 * WebSocket 消息去重。
 *
 * 后端对没有 `id` 的载荷会在推送现场补一个全新的 UUID（src/ws/mod.rs 的
 * `entry("id").or_insert_with(|| Uuid::new_v4())`），下载进度载荷本身只带
 * `task_id`——所以"按 id 去重"永远不会命中：它只是把每条正常消息记进缓存，
 * 既挡不住 socket.io 长轮询模式的真实重复投递，缓存还常年堆满无效条目。
 *
 * 这里改为按 `事件名 + 载荷内容` 做键并排除 `id`：只有内容完全相同的短时间窗口内
 * 的重复投递会被丢弃（这类重复不携带新信息，消费方都是幂等赋值/弹窗），
 * 缓存带过期，不再是只进不出的死表。
 */
const seen = new Map<string, number>();
const DUPLICATE_WINDOW_MS = 5_000;
const MAX_ENTRIES = 500;

function contentKey(namespace: string, message: unknown): string | null {
  if (!namespace) return null;
  if (message === null || message === undefined) return `${namespace}:null`;
  if (typeof message !== 'object') return `${namespace}:${String(message)}`;
  const fields = Object.entries(message as Record<string, unknown>)
    .filter(([key]) => key !== 'id')
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const parts: string[] = [];
  for (const [key, value] of fields) {
    // 嵌套结构用 JSON 表示；循环引用等异常由外层 catch 兜底为"不去重"。
    const rendered = typeof value === 'object' && value !== null ? JSON.stringify(value) : String(value);
    parts.push(`${key}=${rendered}`);
  }
  return `${namespace}:{${parts.join(',')}}`;
}

export function acceptWebSocketMessage(namespace: string, message: unknown): boolean {
  const now = Date.now();
  let key: string | null;
  try {
    key = contentKey(namespace, message);
  } catch {
    return true;
  }
  if (key === null) return true;
  const previous = seen.get(key);
  if (previous !== undefined && now - previous < DUPLICATE_WINDOW_MS) return false;
  seen.set(key, now);
  if (seen.size > MAX_ENTRIES) {
    for (const [seenKey, at] of seen) {
      if (now - at >= DUPLICATE_WINDOW_MS) seen.delete(seenKey);
    }
    // 仍然超限说明是短时高频的不同消息：直接清表，去重只是优化不是正确性依赖。
    if (seen.size > MAX_ENTRIES) seen.clear();
  }
  return true;
}

/** 暴露给测试用：清空去重缓存。 */
export function _resetWsDedupe() {
  seen.clear();
}
