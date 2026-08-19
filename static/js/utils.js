import { _NETWORK_ERR_MSG } from './state.js';

export function escapeHtml(value) {
    if (value === null || value === undefined) return '';
    return String(value)
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#039;');
}

// 把 catch 块里的异常统一成「操作描述 + 具体原因」的中文短句。
// 调用规范：showToast(formatError('加载博主列表', e), 'error')
//
// - AbortError 默认按"操作已取消"处理（原代码多在 catch 上方就 return，不受影响）。
// - offline：复用 _NETWORK_ERR_MSG（注意不直接返回它，以保留 showToast 的"持续网络 toast"
//   机制由调用方按 e.offline 短路）。
// - 裸 fetch 失败（TypeError: Failed to fetch ...）也归一为网络错误。
export function formatError(prefix, error) {
    if (!error) return prefix || '操作失败';
    if (error.name === 'AbortError') return prefix || '操作已取消';
    const offline =
        error.offline ||
        error?.cause?.offline ||
        (error.name === 'TypeError' && /fetch|network|load/i.test(error.message || ''));
    const suffix = offline
        ? _NETWORK_ERR_MSG
        : (error.message || (typeof error === 'string' ? error : null) || '未知错误');
    return prefix ? `${prefix}：${suffix}` : suffix;
}

export function formatFileSize(bytes) {
    const value = Number(bytes);
    if (!Number.isFinite(value) || value <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
    return `${parseFloat((value / Math.pow(1024, index)).toFixed(2))} ${units[index]}`;
}

export function formatSpeed(bytesPerSecond) {
    return `${formatFileSize(bytesPerSecond)}/s`;
}

export function clampPercent(value) {
    const numeric = Number(value);
    return Number.isFinite(numeric) ? Math.min(100, Math.max(0, numeric)) : 0;
}

// 将后端 `error_kind` 结构化错误码翻译为中文短标签，供失败卡片使用。
// 后端 kind 取自 `BiliApiError` 的 Debug 输出（Paywall / PermissionDenied /
// AccountFrozen / RegionRestricted / LoginRequired / NetworkError / ...），
// 未识别时降级显示原文 + 「未知错误」标签。
const ERROR_KIND_LABELS = {
    Paywall: '充电/付费',
    PermissionDenied: '权限不足',
    AccountFrozen: '账号被封',
    RegionRestricted: '区域限制',
    LoginRequired: '需要登录',
    NetworkError: '网络错误',
    CookieInvalid: 'Cookie 失效',
    AlreadyExists: '重复任务',
    RateLimited: '触发风控',
    BvidNotFound: '视频不存在',
    NotFound: '视频不存在',
    Internal: '服务器内部错误',
};

export function formatErrorKind(kind) {
    if (!kind) return null;
    return ERROR_KIND_LABELS[kind] || `${kind}（未知错误）`;
}

// 抽取「原因」短文：失败卡片 hover/详情用。
// 输入可以是 failure 对象 {message, kind, fallback_reason} 或 task 里平铺的字段。
export function formatFailureText(input) {
    if (!input) return null;
    const message = input.message || input.error;
    const kindLabel = formatErrorKind(input.kind || input.error_kind);
    const fallback = input.fallback_reason;
    const parts = [];
    if (kindLabel) parts.push(kindLabel);
    if (fallback) parts.push(fallback);
    if (message) parts.push(message);
    return parts.length ? parts.join(' · ') : null;
}
