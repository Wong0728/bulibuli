/**
 * 面板"贴底才自动滚"。
 *
 * 日志列表是最新在末尾、轮询刷新的；如果每次都无条件 scrollTop = scrollHeight，
 * 用户往上翻看历史时会被持续拽回底部。只在用户本来就停在底部时才跟随新内容。
 */
export function autoScrollToBottomIfPinned(
  element: HTMLElement | null | undefined,
  thresholdPx = 24,
) {
  if (!element) return;
  const distanceToBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
  if (distanceToBottom <= thresholdPx) element.scrollTop = element.scrollHeight;
}
