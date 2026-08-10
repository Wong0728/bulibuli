export function showToast(message, type = 'info', duration = 2700, state) {
    const box = document.getElementById('msg-box');
    if (!box) return null;
    const textValue = String(message);
    const isError = type === 'error';
    if (isError && state.errorMsgSet.has(textValue)) {
        const existing = state.errorMsgSet.get(textValue);
        if (existing.el && box.contains(existing.el)) return existing;
        state.errorMsgSet.delete(textValue);
    }

    const toast = document.createElement('div');
    toast.className = `msg-toast ${type || 'info'}`;
    const icon = document.createElement('i');
    const icons = {
        success: 'fa-check-circle',
        error: 'fa-exclamation-circle',
        warning: 'fa-exclamation-triangle',
        info: 'fa-info-circle',
    };
    icon.className = `fa-solid ${icons[type] || icons.info} toast-icon toast-icon-${type}`;
    const text = document.createElement('span');
    text.className = 'msg-toast-text';
    text.textContent = textValue;
    const closeButton = document.createElement('button');
    closeButton.className = 'msg-toast-close';
    closeButton.type = 'button';
    closeButton.setAttribute('aria-label', '关闭');
    closeButton.title = '关闭';
    closeButton.textContent = '×';
    toast.append(icon, text, closeButton);
    box.appendChild(toast);

    let timer;
    const close = () => {
        if (timer) clearTimeout(timer);
        if (state.networkToastEl === toast) state.networkToastEl = null;
        if (isError) state.errorMsgSet.delete(textValue);
        toast.classList.add('msg-toast-leaving');
        setTimeout(() => toast.remove(), 300);
    };
    closeButton.addEventListener('click', close);
    timer = setTimeout(close, isError ? Math.max(duration, 5000) : duration);
    const handle = { el: toast, close };
    if (isError) state.errorMsgSet.set(textValue, handle);
    return handle;
}
