import { _state, subscribeState } from './state.js';
import { showToast } from './toast.js';

const NETWORK_FAIL_THRESHOLD = 1;

_state.networkFailCount ??= 0;
_state.networkToastEl ??= null;
_state.isNetworkOnline ??= navigator.onLine !== false;

subscribeState('networkFailCount', updateNetworkBanner);
subscribeState('isNetworkOnline', updateNetworkDisabledButtons);

export function updateNetworkBanner() {
    const banner = document.getElementById('network-error-banner');
    if (banner) banner.classList.toggle('show', _state.networkFailCount >= NETWORK_FAIL_THRESHOLD);
}

export function updateNetworkDisabledButtons() {
    const selectors = [
        '#tab-manual .btn-primary',
        '#tab-manual .manual-download-btn',
        '.drawer-btn-primary',
        '#detail-start-btn',
        '#board-refresh-btn',
    ];
    document.querySelectorAll(selectors.join(',')).forEach(btn => {
        btn.classList.toggle('network-disabled', !_state.isNetworkOnline);
        if (_state.isNetworkOnline) delete btn.dataset.networkDisabled;
        else btn.dataset.networkDisabled = 'true';
    });
}

export function checkNetworkBeforeAction() {
    if (_state.isNetworkOnline) return true;
    showToast('网络未恢复，请检查网络后重试', 'warning');
    return false;
}

export function onNetworkRecovered() {
    _state.networkFailCount = 0;
    _state.isNetworkOnline = true;
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
