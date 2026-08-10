export class ApiError extends Error {
    constructor(code, message, details = {}) {
        super(message);
        this.name = 'ApiError';
        this.code = code;
        Object.assign(this, details);
    }
}

export function parseEnvelope(envelope, status = 200) {
    if (!envelope
        || !Number.isInteger(envelope.code)
        || typeof envelope.message !== 'string'
        || !Object.hasOwn(envelope, 'data')) {
        throw new ApiError(502, 'API 响应契约无效', { retryable: true });
    }
    if (status < 200 || status >= 300 || envelope.code !== 0) {
        throw new ApiError(envelope.code || status, envelope.message, {
            retryable: status >= 500,
            data: envelope.data,
            status,
        });
    }
    return envelope;
}

export async function requestEnvelope(url, options = {}, handlers = {}) {
    const { headers = {}, ...requestOptions } = options;
    const response = await fetch(url, {
        credentials: 'same-origin',
        ...requestOptions,
        headers: { 'Content-Type': 'application/json', ...headers },
    });
    const contentType = response.headers.get('content-type') || '';
    if (!contentType.includes('application/json')) {
        throw new ApiError(response.status || 502, '响应格式异常', {
            retryable: response.status >= 500,
        });
    }
    const envelope = await response.json();
    try {
        return parseEnvelope(envelope, response.status);
    } catch (error) {
        if (!(error instanceof ApiError)) throw error;
        if (response.status === 401 || envelope.code === -101) {
            await handlers.onUnauthorized?.(error);
        } else if (response.status === 403 || [-352, -403].includes(envelope.code)) {
            await handlers.onRiskControl?.(error);
        }
        throw error;
    }
}
