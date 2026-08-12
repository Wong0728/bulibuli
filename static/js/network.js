import { _state, subscribeState } from './state.js';
import { showToast } from './toast.js';

const NETWORK_FAIL_THRESHOLD = 1;

_state.networkFailCount ??= 0;
_state.networkToastEl ??= null;
_state.isNetworkOnline ??= navigator.onLine !== false;
_state.backendAvailable ??= true;

if (typeof MutationObserver !== 'undefined' && document.documentElement) {
    const networkControlObserver = new MutationObserver(updateNetworkDisabledButtons);
    networkControlObserver.observe(document.documentElement, { childList: true, subtree: true });
}

subscribeState('networkFailCount', updateNetworkBanner);
subscribeState('isNetworkOnline', updateNetworkDisabledButtons);

export function updateNetworkBanner() {
    const banner = document.getElementById('network-error-banner');
    if (banner) banner.classList.toggle('show', !_state.isNetworkOnline || !_state.backendAvailable || _state.networkFailCount >= NETWORK_FAIL_THRESHOLD);
}

export function updateNetworkDisabledButtons() {
    const networkRequiredActions = new Set([
        'show-qr-login', 'logout-account', 'save-manual-cookie', 'test-download',
        'restart-aria2', 'browse-ffmpeg-path', 'test-ffmpeg', 'save-settings',
        'reset-settings', 'load-settings', 'confirm-blogger-modal',
        'confirm-add-blogger', 'confirm-edit-blogger', 'refresh-qr-code',
        'open-manual-video', 'load-more-manual', 'load-more-history',
        'get-download-links', 'download-video', 'download-audio',
        'download-danmaku', 'download-comments', 'cleanup-blogger',
        'open-video', 'remove-known-blogger', 'clear-known-bloggers',
        'save-bloggers', 'acknowledge-blogger-change', 'acknowledge-all-blogger-changes',
        'retry-video', 'start-video', 'pause-download', 'resume-download',
        'delete-video', 'load-drawer-comments', 'load-drawer-danmaku',
        'refresh-video-info', 'download-cover', 'load-bvid-logs', 'burn-media',
        'select-quality', 'start-manual-video', 'resolve-link',
        'start-season-download', 'toggle-all-pages', 'toggle-all-episodes',
        'live-start', 'live-stop', 'source-edit', 'source-delete',
        'history-burn', 'history-merge', 'history-open', 'merge-cancel',
        'interaction-select', 'context-menu-edit', 'context-menu-delete',
        'open-history-directory', 'open-video-page', 'show-add-blogger-modal',
    ]);
    const unavailable = !_state.isNetworkOnline || !_state.backendAvailable;
    const roleRestricted = _state.sessionRole === 'viewer';
    const controls = document.querySelectorAll('[data-action], [data-network-required]');
    controls.forEach(control => {
        const requiresNetwork = control.dataset.networkRequired === 'true'
            || networkRequiredActions.has(control.dataset.action);
        if (!requiresNetwork) return;

        const disabled = unavailable || roleRestricted;
        control.classList.toggle('network-disabled', disabled);
        if (disabled) {
            if (!control.dataset.networkOriginalTitle) {
                control.dataset.networkOriginalTitle = control.getAttribute('title') || '';
            }
            control.dataset.networkDisabled = 'true';
            control.setAttribute('aria-disabled', 'true');
            control.setAttribute('title', roleRestricted && !unavailable
                ? '当前会话仅可查看'
                : '网络或后端服务不可用，请恢复连接后重试');
            if ('disabled' in control && !control.disabled) {
                control.dataset.networkDisabledByNetwork = 'true';
                control.disabled = true;
            }
        } else {
            delete control.dataset.networkDisabled;
            control.removeAttribute('aria-disabled');
            if (control.dataset.networkDisabledByNetwork === 'true') {
                control.disabled = false;
                delete control.dataset.networkDisabledByNetwork;
            }
            const originalTitle = control.dataset.networkOriginalTitle;
            if (originalTitle) control.setAttribute('title', originalTitle);
            else control.removeAttribute('title');
            delete control.dataset.networkOriginalTitle;
        }
    });

    document.querySelectorAll('#blogger-search-btn, #manual-query-btn, #manual-resolve-btn, #show-add-blogger-btn, #detail-start-btn, #detail-stop-btn, #board-refresh-btn').forEach(control => {
        if (unavailable || roleRestricted) {
            if (!control.dataset.networkOriginalTitle) control.dataset.networkOriginalTitle = control.getAttribute('title') || '';
            control.dataset.networkDisabled = 'true';
            control.dataset.networkDisabledByNetwork = control.disabled ? 'false' : 'true';
            control.disabled = true;
            control.classList.add('network-disabled');
            control.setAttribute('aria-disabled', 'true');
            control.setAttribute('title', roleRestricted && !unavailable
                ? '当前会话仅可查看'
                : '网络或后端服务不可用，请恢复连接后重试');
        } else if (control.dataset.networkDisabled === 'true') {
            control.classList.remove('network-disabled');
            control.removeAttribute('aria-disabled');
            if (control.dataset.networkDisabledByNetwork === 'true') control.disabled = false;
            const originalTitle = control.dataset.networkOriginalTitle;
            if (originalTitle) control.setAttribute('title', originalTitle);
            else control.removeAttribute('title');
            delete control.dataset.networkDisabled;
            delete control.dataset.networkDisabledByNetwork;
            delete control.dataset.networkOriginalTitle;
        }
    });
}

export function checkNetworkBeforeAction() {
    if (_state.isNetworkOnline && _state.backendAvailable) return true;
    showToast(_state.isNetworkOnline
        ? '后端服务暂不可用，请恢复服务后重试'
        : '网络未恢复，请检查网络后重试', 'warning');
    return false;
}

export function setBackendAvailability(available) {
    _state.backendAvailable = available;
    if (!available) _state.networkFailCount = Math.max(_state.networkFailCount, NETWORK_FAIL_THRESHOLD);
    updateNetworkBanner();
    updateNetworkDisabledButtons();
}

export function onNetworkRecovered() {
    _state.networkFailCount = 0;
    _state.isNetworkOnline = true;
    _state.backendAvailable = true;
    updateNetworkBanner();
    dismissNetworkToast();
    updateNetworkDisabledButtons();
}

export function dismissNetworkToast() {
    const element = _state.networkToastEl;
    if (!element) return;
    _state.networkToastEl = null;
    element.classList.add('msg-toast-leaving');
    setTimeout(() => element.remove(), 300);
}
