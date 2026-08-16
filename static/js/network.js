import { _state, _NETWORK_ERR_MSG, subscribeState } from './state.js';
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

// 离线/后端不可用时仍可用的纯本地 UI 操作（关闭弹窗、切换视图、本地编辑与勾选）。
// 其余所有 data-action 一律禁用：任何需要后端的项在无网状态都不应可点。
const LOCAL_ONLY_ACTIONS = new Set([
    'switch-tab', 'switch-board-tab',
    'close-video-drawer', 'close-blogger-modal', 'close-add-blogger-modal',
    'close-edit-blogger-modal', 'close-qr-modal', 'close-blogger-notice-modal',
    'dismiss-network-toast', 'dismiss-cookie-warning',
    'toggle-manual-cookie', 'add-time-point', 'remove-time-point',
    'select-quality', 'toggle-all-pages', 'toggle-all-episodes',
    'browser-download-master', 'browser-download-check',
]);

// 仅修改被本模块禁用过的控件：从未被禁用的元素保持原样（包括 title 等属性），
// 避免全局遍历把其他功能的 tooltip 清掉。
function setControlDisabled(control, disabled, unavailable, roleRestricted) {
    if (disabled) {
        control.classList.add('network-disabled');
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
        return;
    }
    if (control.dataset.networkDisabled !== 'true') return;
    control.classList.remove('network-disabled');
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

export function updateNetworkDisabledButtons() {
    const unavailable = !_state.isNetworkOnline || !_state.backendAvailable;
    const roleRestricted = _state.sessionRole === 'viewer';
    document.querySelectorAll('[data-action]').forEach(control => {
        // 反转策略：默认全部禁用，仅本地 UI 操作放行。
        const disabled = (unavailable || roleRestricted)
            && !LOCAL_ONLY_ACTIONS.has(control.dataset.action);
        setControlDisabled(control, disabled, unavailable, roleRestricted);
    });

    // 显式声明需要网络的按钮（直播页等，无 data-action）。
    document.querySelectorAll('[data-network-required]').forEach(control => {
        setControlDisabled(control, unavailable || roleRestricted, unavailable, roleRestricted);
    });

    // 无 data-action 的独立按钮（搜索/刷新等）同样全部禁用。
    document.querySelectorAll('#blogger-search-btn, #manual-query-btn, #manual-resolve-btn, #show-add-blogger-btn, #detail-start-btn, #detail-stop-btn, #board-refresh-btn').forEach(control => {
        setControlDisabled(control, unavailable || roleRestricted, unavailable, roleRestricted);
    });
}

/// 唯一的离线提示：全屏期间只保留这一条持续 toast，网络恢复后自动消失。
export function showNetworkToast() {
    if (_state.networkToastEl?.isConnected) return _state.networkToastEl;
    const handle = showToast(_NETWORK_ERR_MSG, 'warning', 0);
    _state.networkToastEl = handle?.el ?? null;
    return _state.networkToastEl;
}

export function checkNetworkBeforeAction() {
    if (_state.isNetworkOnline && _state.backendAvailable) return true;
    showNetworkToast();
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
