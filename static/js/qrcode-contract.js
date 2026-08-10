// 二维码登录接口的业务 payload 解析。API 信封始终保持 { code, message, data }。

export function getQRCodePayload(envelope) {
    const payload = envelope?.data;
    if (!payload || typeof payload !== 'object') return null;

    const url = typeof payload.url === 'string' ? payload.url.trim() : '';
    const qrcodeKey = typeof payload.qrcode_key === 'string' ? payload.qrcode_key.trim() : '';
    return url && qrcodeKey ? { url, qrcodeKey } : null;
}

export function getQRCodePollState(envelope) {
    const payload = envelope?.data;
    const code = payload?.code;
    const message = typeof payload?.message === 'string' ? payload.message.trim() : '';

    if (!Number.isInteger(code)) {
        return { kind: 'invalid', code: null, message: '二维码轮询响应缺少状态码' };
    }

    const kindByCode = {
        0: 'success',
        86101: 'waiting',
        86090: 'scanned',
        86038: 'expired',
    };
    return {
        kind: kindByCode[code] || 'unexpected',
        code,
        message,
    };
}
