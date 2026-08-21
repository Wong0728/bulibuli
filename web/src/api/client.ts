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
import type { ApiError } from './types';

const _BASE = ''; // 同源部署，直接用相对路径
let csrfToken: string | null = null;

/** 老框架 state.js：全局统一网络错误文案（toast / 横幅共用）。 */
export const NETWORK_ERR_MSG = '网络连接异常，请检查网络或后端服务状态';

type ApiErrorHandlers = {
  onUnauthorized?: (e: ApiError, url: string) => any;
  onRiskControl?: (e: ApiError, url: string) => any;
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

function parseEnvelope(envelope: any, status: number): { code: number; message: string; data: any } {
  if (!envelope || !Number.isInteger(envelope.code) || typeof envelope.message !== 'string' || !Object.prototype.hasOwnProperty.call(envelope, 'data')) {
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

async function requestEnvelope<T = any>(
  url: string,
  options: RequestInit = {},
  handlers: ApiErrorHandlers = {},
): Promise<{ code: number; message: string; data: T | null }> {
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

  let envelope: any;
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
    return parsed as { code: number; message: string; data: T | null };
  } catch (error) {
    if (!(error instanceof ApiErrorImpl)) throw error;
    // 对齐老框架：code 502（响应契约无效）视为后端异常。
    if (error.code === 502) networkHooks.onInvalidResponse?.(error);
    const activeHandlers = { ...globalHandlers, ...handlers };
    // 对齐老框架 api.js：401/-101 → onUnauthorized；403/-352/-403 → onRiskControl。
    if (response.status === 401 || envelope.code === -101) {
      await activeHandlers.onUnauthorized?.(error, url);
    } else if (response.status === 403 || [-352, -403].includes(envelope.code)) {
      await activeHandlers.onRiskControl?.(error, url);
    }
    throw error;
  }
}

/** 与老框架 requestEnvelope 一致：需要消费后端 message（如保存设置的暂存提示）时用 Full 变体。 */
async function requestFull<T = any>(
  url: string,
  options: RequestInit = {},
  handlers: ApiErrorHandlers = {},
): Promise<{ data: T | null; message: string }> {
  const parsed = await requestEnvelope<T>(url, options, handlers);
  return { data: parsed.data, message: parsed.message };
}

async function request<T = any>(
  url: string,
  options: RequestInit = {},
  handlers: ApiErrorHandlers = {},
): Promise<T | null> {
  const parsed = await requestEnvelope<T>(url, options, handlers);
  return parsed.data as T;
}

const get = <T = any>(url: string, params?: Record<string, any>) => {
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
const post = <T = any>(url: string, body?: any) =>
  request<T>(url, { method: 'POST', body: body !== undefined ? JSON.stringify(body) : undefined });
const put = <T = any>(url: string, body?: any) =>
  request<T>(url, { method: 'PUT', body: body !== undefined ? JSON.stringify(body) : undefined });
const getFull = <T = any>(url: string, params?: Record<string, any>) => {
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
const postFull = <T = any>(url: string, body?: any) =>
  requestFull<T>(url, { method: 'POST', body: body !== undefined ? JSON.stringify(body) : undefined });
const putFull = <T = any>(url: string, body?: any) =>
  requestFull<T>(url, { method: 'PUT', body: body !== undefined ? JSON.stringify(body) : undefined });

export { request, requestFull, get, post, put, getFull, postFull, putFull };
export type { ApiError };
