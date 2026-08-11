import { _state } from './state.js';
import { apiGet } from './core.js';
import { escapeHtml } from './utils.js';

_state.cookieWarningShown ??= false;
_state.loginValid ??= false;

export async function checkCookiesStatus() {
    const banner = document.getElementById('cookie-warning-banner');
    try {
        const result = await apiGet('/api/cookies/status');
        if (result.offline) return;
        const data = result.data || {};
        const statusState = data.state || (data.valid ? 'authenticated' : 'unauthenticated');
        updateLoginCard(data);
        if (data.valid || statusState === 'authenticated') {
            if (banner) banner.hidden = true;
            _state.cookieWarningShown = false;
        } else {
            if (banner) {
                const span = banner.querySelector('span');
                if (span && ['risk_control', 'unreachable', 'malformed'].includes(statusState)) {
                    span.textContent = statusState === 'risk_control'
                        ? 'B 站暂时限制了状态检查，保留当前 Cookie，不会误判为过期。'
                        : statusState === 'unreachable'
                            ? '暂时无法连接 B 站，保留当前 Cookie，请稍后重试。'
                            : 'B 站返回的数据暂时无法识别，请稍后重试。';
                } else if (span) span.textContent = data.has_cookies
                    ? '当前 B 站登录已失效，请重新登录，部分功能受限（仅能获取低清晰度视频）。'
                    : '未登录 B 站账号，部分功能受限（仅能获取低清晰度视频）。';
                banner.hidden = false;
            }
            _state.cookieWarningShown = true;
        }
    } catch (error) {
        updateLoginCard({ state: 'unreachable', has_cookies: true });
        if (banner) banner.hidden = false;
        _state.cookieWarningShown = true;
    }
}

export function dismissCookieWarning() {
    const banner = document.getElementById('cookie-warning-banner');
    if (banner) banner.hidden = true;
}

export function updateLoginCard(info) {
    _state.loginValid = !!(info && info.valid);
    const card = document.getElementById('login-user-card');
    const prompt = document.getElementById('login-prompt-btn');
    const settingsStatus = document.getElementById('cookie-login-status');
    if (_state.loginValid) {
        const face = info.face ? `/api/video/proxy-image?url=${encodeURIComponent(info.face)}` : '';
        const isVip = (info.vip_status || 0) > 0;
        const vipText = info.vip_label || (isVip ? '大会员' : '');
        const faceHtml = face
            ? `<img class="login-user-face" src="${face}" alt="" data-image-error="hide">`
            : '<div class="login-user-face login-user-face-ph"><i class="fa-solid fa-user"></i></div>';
        const vipBadge = isVip ? `<span class="login-vip-badge">${escapeHtml(vipText || '大会员')}</span>` : '';
        if (card) {
            card.hidden = false;
            card.innerHTML = `${faceHtml}<div class="login-user-meta"><span class="login-user-name">${escapeHtml(info.uname || '')}</span><span class="login-user-sub">Lv${Number(info.level) || 0} ${vipBadge}</span></div><button class="login-switch-btn" data-action="show-qr-login" title="切换账号"><i class="fa-solid fa-right-left"></i></button>`;
        }
        if (prompt) prompt.hidden = true;
        if (settingsStatus) settingsStatus.innerHTML = `${faceHtml}<div class="login-user-meta"><span class="login-user-name">${escapeHtml(info.uname || '')} ${vipBadge}</span><span class="login-user-sub">UID ${escapeHtml(String(info.mid || '--'))} · Lv${Number(info.level) || 0} · 已登录</span></div>`;
    } else {
        const transientState = ['risk_control', 'unreachable', 'malformed'].includes(info?.state);
        if (card) card.hidden = true;
        if (prompt && transientState) {
            prompt.hidden = false;
            prompt.innerHTML = '<i class="fa-solid fa-circle-exclamation"></i> B 站状态暂不可用';
        } else if (prompt) {
            prompt.hidden = false;
            prompt.innerHTML = `<i class="fa-solid fa-user"></i> ${info && info.has_cookies ? '登录失效·重新登录' : '未登录·点击登录'}`;
        }
        const statusText = transientState
            ? info?.state === 'risk_control'
                ? 'B 站暂时限制状态检查，请稍后重试'
                : 'B 站状态暂时不可用，请稍后重试'
            : info && info.has_cookies
                ? '当前 Cookie 无效或已过期'
                : '尚未登录 B 站账号';
        if (settingsStatus) settingsStatus.innerHTML = `<span class="login-user-sub"><i class="fa-solid fa-circle-exclamation"></i> ${statusText}</span>`;
    }
}

export async function refreshLoginInfo() {
    await checkCookiesStatus();
}
