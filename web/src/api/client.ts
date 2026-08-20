/**
 * 统一 API 客户端：按后端 Rust Axum 模块一一对应，便于维护。
 *
 * 约定：
 * - 所有方法返回标准化的 envelope `Result<T, ApiError>`。
 * - GET 用 query，POST 用 body。
 * - 不在内部处理全局 UI 副作用（toast/横幅），交由 store 与组件订阅。
 *
 * 错误分级：
 * - 网络层失败 / 后端不可达（vite 代理 502、连接被拒、超时）→ 静默，返回 null。
 * - 后端返回非 JSON 错误页（5xx proxy error、HTML 错误页）→ 静默，返回 null。
 *   调用方用 `if (r == null) return;` 走空数据路径，避免 pageerror。
 * - 后端 envelope 内业务错误（4xx 配 code !== 0）→ 仍 throw，调用方按需 toast。
 */
import type { ApiError } from './types';

const _BASE = ''; // 同源部署，直接用相对路径

export class ApiErrorImpl extends Error implements ApiError {
  code: number;
  status: number;
  retryable: boolean;
  data?: unknown;

  constructor(code: number, message: string, init: Partial<ApiError> = {}) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
    this.status = init.status ?? 502;
    this.retryable = init.retryable ?? false;
    this.data = init.data;
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

async function request<T = any>(
  url: string,
  options: RequestInit = {},
  handlers: { onUnauthorized?: (e: ApiError) => any; onRiskControl?: (e: ApiError) => any } = {},
): Promise<T | null> {
  let response: Response;
  try {
    response = await fetch(_BASE + url, {
      credentials: 'same-origin',
      ...options,
      headers: { 'Content-Type': 'application/json', ...(options.headers || {}) },
    });
  } catch {
    // 网络层失败：静默
    return null;
  }

  const contentType = response.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    // 后端不可达或 5xx proxy 错误页：静默
    return null;
  }

  let envelope: any;
  try {
    envelope = await response.json();
  } catch {
    return null;
  }

  try {
    const parsed = parseEnvelope(envelope, response.status);
    return parsed.data as T;
  } catch (error) {
    if (!(error instanceof ApiErrorImpl)) throw error;
    if (response.status === 401 || envelope.code === -101) {
      await handlers.onUnauthorized?.(error);
    } else if (response.status === 403 || [-352, -403].includes(envelope.code)) {
      await handlers.onRiskControl?.(error);
    }
    throw error;
  }
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

export { request, get, post };
export type { ApiError };
