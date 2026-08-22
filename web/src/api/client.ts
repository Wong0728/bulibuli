/**
 * 统一 API 客户端：按后端 Rust Axum 模块一一对应，便于维护。
 *
 * 约定：
 * - 所有方法返回标准化的 envelope `Result<T, ApiError>`。
 * - GET 用 query，POST 用 body。
 * - 不在内部处理全局 UI 副作用（toast/横幅），交由 store 与组件订阅。
 *
 * 错误分级：网络、非 JSON、HTTP 和 envelope 业务错误都抛出 ApiError，
 * 由 store/页面显示真实失败原因，不能把搜索失败伪装成空结果。
 */
import type { ApiError, ApiResp } from './types';

const _BASE = ''; // 同源部署，直接用相对路径
let csrfToken: string | null = null;

/** 老框架 state.js：全局统一网络错误文案（toast / 横幅共用）。 */
export const NETWORK_ERR_MSG = '网络连接异常，请检查网络或后端服务状态';

type ApiErrorHandlers = {
  onUnauthorized?: (e: ApiError, url: string) => void | Promise<void>;
  onRiskControl?: (e: ApiError, url: string) => void | Promise<void>;
};
let globalHandlers: ApiErrorHandlers = {};

/** 网络层钩子：由 app store 注册，把请求成败接入全局网络降级体系（对齐老框架 core.js apiRequest）。 */
type NetworkHooks = {
  /** 任一请求成功（envelope 正带返回）：对齐 onNetworkRecovered() 清账。 */
  onRequestSuccess?: () => void;
  /** fetch 网络失败/响应无法解析：对齐 networkFailCount++ + 离线 toast。 */
  onRequestNetworkFailure?: () => void;
  /** 后端响应异常（非 JSON / 契约无效）：对齐 setBackendAvailability(false)。 */
  onInvalidResponse?: (e: ApiError) => void;
};
let networkHooks: NetworkHooks = {};

/** 请求默认超时（毫秒）：可用环境变量 VITE_API_TIMEOUT 覆盖（如 "30000"）；
 *  后端挂起时不能让 UI 永久 pending，调用方可经 options.signal 对大文件/慢端点单独覆盖。 */
const ENV_TIMEOUT_MS = Number(import.meta.env?.VITE_API_TIMEOUT);
export const DEFAULT_TIMEOUT_MS =
  Number.isFinite(ENV_TIMEOUT_MS) && ENV_TIMEOUT_MS > 0 ? ENV_TIMEOUT_MS : 15_000;

/** 慢端点按前缀匹配差异化超时：这些接口依赖 B 站上游 / GitHub / 本机 ffmpeg 子进程，
 *  固定 15s 会把上游慢误报为自身网络故障（对齐审查项"按端点调参，保守小步"）。
 *  显式 env 覆盖时以 env 为准，不再二次放大。 */
const SLOW_ENDPOINT_TIMEOUTS: Array<[prefix: string, timeoutMs: number]> = [
  ['/api/update/', 30_000], // GitHub API 检查 / 应用更新
  ['/api/settings/ffmpeg-test', 30_000], // 本机拉起 ffmpeg 探测
  ['/api/video/resolve', 25_000], // B 站链接解析（番剧/课程上游慢）
  ['/api/video/get-video-urls', 25_000], // 取播放地址（B 站上游）
  ['/api/blogger/search', 25_000], // B 站用户搜索
  ['/api/refresh', 30_000], // 手动触发全量刷新
];

function endpointTimeoutMs(url: string): number {
  if (Number.isFinite(ENV_TIMEOUT_MS) && ENV_TIMEOUT_MS > 0) return DEFAULT_TIMEOUT_MS;
  const hit = SLOW_ENDPOINT_TIMEOUTS.find(([prefix]) => url.startsWith(prefix));
  return hit ? hit[1] : DEFAULT_TIMEOUT_MS;
}

/** 认证状态加载后由 auth store 注入；所有写请求共用这一份会话 token。 */
export function setCsrfToken(token: string | null | undefined) {
  csrfToken = token || null;
}

/** 由应用层注册一次，避免每个 API 调用方各自遗漏 401/403 恢复处理。 */
export function setApiErrorHandlers(handlers: ApiErrorHandlers) {
  globalHandlers = handlers;
}

/** 由 app store 注册一次：把网络层成败接入全局降级体系。 */
export function setNetworkHooks(hooks: NetworkHooks) {
  networkHooks = hooks;
}

export class ApiErrorImpl extends Error implements ApiError {
  code: number;
  status: number;
  retryable: boolean;
  data?: unknown;
  offline?: boolean;

  constructor(code: number, message: string, init: Partial<ApiError> = {}) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
    this.status = init.status ?? 502;
    this.retryable = init.retryable ?? false;
    this.data = init.data;
    this.offline = init.offline;
  }
}

/** envelope 最小结构校验：{ code, message, data } 三键齐备才算合法响应。 */
function isEnvelopeLike(value: unknown): value is { code: number; message: string; data: unknown } {
  if (!value || typeof value !== 'object') return false;
  const v = value as Record<string, unknown>;
  return Number.isInteger(v.code) && typeof v.message === 'string' && Object.prototype.hasOwnProperty.call(value, 'data');
}

function parseEnvelope(envelope: unknown, status: number): ApiResp<unknown> {
  if (!isEnvelopeLike(envelope)) {
    throw new ApiErrorImpl(502, 'API 响应契约无效', { status, retryable: true });
  }
  if (status < 200 || status >= 300 || envelope.code !== 0) {
    throw new ApiErrorImpl(envelope.code || status, envelope.message, {
      status,
      retryable: status >= 500,
      data: envelope.data,
    });
  }
  return envelope;
}

async function requestEnvelope<T = unknown>(
  url: string,
  options: RequestInit = {},
  handlers: ApiErrorHandlers = {},
): Promise<ApiResp<T | null>> {
  let response: Response;
  try {
    const method = (options.method || 'GET').toUpperCase();
    const headers = new Headers(options.headers);
    if (!headers.has('Content-Type')) headers.set('Content-Type', 'application/json');
    if (!['GET', 'HEAD', 'OPTIONS'].includes(method) && csrfToken && !headers.has('X-CSRF-Token')) {
      headers.set('X-CSRF-Token', csrfToken);
    }
    response = await fetch(_BASE + url, {
      credentials: 'same-origin',
      ...options,
      headers,
      signal: options.signal ?? AbortSignal.timeout(endpointTimeoutMs(url)),
    });
  } catch (error) {
    if (error instanceof ApiErrorImpl) throw error;
    // 网络层失败：接入全局降级（对齐老框架 networkFailCount++ + 持续离线 toast）。
    networkHooks.onRequestNetworkFailure?.();
    throw new ApiErrorImpl(0, NETWORK_ERR_MSG, { retryable: true, data: error, offline: true });
  }

  const contentType = response.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    // 对齐老框架 api.js：非 JSON 响应按状态分流 401/403 全局处理器。
    const status = response.status || 502;
    const error = new ApiErrorImpl(
      status,
      status === 401 ? '登录已过期' : status === 403 ? '请求被风控拦截' : '响应格式异常',
      { status, retryable: status >= 500 },
    );
    const activeHandlers = { ...globalHandlers, ...handlers };
    if (status === 401) {
      await activeHandlers.onUnauthorized?.(error, url);
    } else if (status === 403) {
      await activeHandlers.onRiskControl?.(error, url);
    } else {
      // 对齐老框架 core.js apiRequest：响应格式异常 → 后端不可用。
      networkHooks.onInvalidResponse?.(error);
    }
    throw error;
  }

  let envelope: unknown;
  try {
    envelope = await response.json();
  } catch (error) {
    // 对齐老框架 api.js（json() 无捕获，SyntaxError 落到 apiRequest 的网络降级分支）。
    networkHooks.onRequestNetworkFailure?.();
    throw new ApiErrorImpl(0, NETWORK_ERR_MSG, { status: response.status, retryable: true, data: error, offline: true });
  }

  try {
    const parsed = parseEnvelope(envelope, response.status);
    // 请求成功：接入全局恢复清账（对齐老框架 onNetworkRecovered()）。
    networkHooks.onRequestSuccess?.();
    // data 的具体形状由调用方以泛型声明，运行时结构已由 parseEnvelope 校验为 envelope。
    return parsed as ApiResp<T | null>;
  } catch (error) {
    if (!(error instanceof ApiErrorImpl)) throw error;
    // 对齐老框架：code 502（响应契约无效）视为后端异常。
    if (error.code === 502) networkHooks.onInvalidResponse?.(error);
    const activeHandlers = { ...globalHandlers, ...handlers };
    // 对齐老框架 api.js：401/-101 → onUnauthorized；403/-352/-403 → onRiskControl。
    const envCode = isEnvelopeLike(envelope) ? envelope.code : undefined;
    if (response.status === 401 || envCode === -101) {
      await activeHandlers.onUnauthorized?.(error, url);
    } else if (response.status === 403 || (envCode !== undefined && [-352, -403].includes(envCode))) {
      await activeHandlers.onRiskControl?.(error, url);
    }
    throw error;
  }
}

/** 与老框架 requestEnvelope 一致：需要消费后端 message（如保存设置的暂存提示）时用 Full 变体。 */
async function requestFull<T = unknown>(
  url: string,
  options: RequestInit = {},
  handlers: ApiErrorHandlers = {},
): Promise<{ data: T | null; message: string }> {
  const parsed = await requestEnvelope<T>(url, options, handlers);
  return { data: parsed.data, message: parsed.message };
}

async function request<T = unknown>(
  url: string,
  options: RequestInit = {},
  handlers: ApiErrorHandlers = {},
): Promise<T | null> {
  const parsed = await requestEnvelope<T>(url, options, handlers);
  return parsed.data as T | null;
}

const get = <T = unknown>(url: string, params?: Record<string, unknown>) => {
  if (params) {
    const qs = new URLSearchParams();
    Object.entries(params).forEach(([k, v]) => {
      if (v === undefined || v === null) return;
      qs.append(k, String(v));
    });
    const s = qs.toString();
    if (s) url += (url.includes('?') ? '&' : '?') + s;
  }
  return request<T>(url, { method: 'GET' });
};
const post = <T = unknown>(url: string, body?: unknown) =>
  request<T>(url, { method: 'POST', body: body !== undefined ? JSON.stringify(body) : undefined });
const put = <T = unknown>(url: string, body?: unknown) =>
  request<T>(url, { method: 'PUT', body: body !== undefined ? JSON.stringify(body) : undefined });
const getFull = <T = unknown>(url: string, params?: Record<string, unknown>) => {
  if (params) {
    const qs = new URLSearchParams();
    Object.entries(params).forEach(([k, v]) => {
      if (v === undefined || v === null) return;
      qs.append(k, String(v));
    });
    const s = qs.toString();
    if (s) url += (url.includes('?') ? '&' : '?') + s;
  }
  return requestFull<T>(url, { method: 'GET' });
};
const postFull = <T = unknown>(url: string, body?: unknown) =>
  requestFull<T>(url, { method: 'POST', body: body !== undefined ? JSON.stringify(body) : undefined });
const putFull = <T = unknown>(url: string, body?: unknown) =>
  requestFull<T>(url, { method: 'PUT', body: body !== undefined ? JSON.stringify(body) : undefined });

export { request, requestFull, get, post, put, getFull, postFull, putFull };
export type { ApiError };
