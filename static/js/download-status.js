import { _state, _NETWORK_ERR_MSG } from './state.js';
import { showToast as renderToast } from './toast.js';
import { escapeHtml } from './utils.js';
import { setElementHandler, dismissNetworkToast } from './core.js';
import { updateManualDownloadProgress } from './media-links.js';
import { patchBoardCardProgress } from './download-queue.js';
import { fetchDownloadHealth, fetchDownloadSnapshot } from './download-status-store.js';

// ==================== 下载状态管理 ====================
// 根据 /api/download/status 返回的结果更新 Aria2 状态指示点。
// status 可传 'connected' | 'starting' | 'disconnected'，未传时从 result 解析。
export function updateAria2StatusDot(result, statusOverride) {
    const dots = document.querySelectorAll('#aria2-status-dot, #aria2-status-dot-history, [data-aria2-status]');
    if (dots.length === 0) return;
    const data = result?.data || {};
    const aria2Status = statusOverride || data.aria2_status || (data.aria2_connected ? 'connected' : 'disconnected');
    const diagnostics = data.aria2_diagnostics || {};
    dots.forEach(dot => {
        dot.classList.remove('connected', 'connecting', 'disconnected', 'failed');
        if (aria2Status === 'connected') {
            dot.classList.add('connected');
            dot.title = 'Aria2 已连接（点击查看详情）';
            setElementHandler(dot, 'click', event => {
                event.stopPropagation();
                showToast(`Aria2 已连接 · ${diagnostics.mode || '下载引擎'} · ${diagnostics.endpoint || '本机 RPC'}`, 'success');
            });
        } else if (aria2Status === 'starting') {
            dot.classList.add('connecting');
            const elapsed = Number.isFinite(diagnostics.starting_for_ms)
                ? `（${Math.ceil(diagnostics.starting_for_ms / 1000)} 秒）`
                : '';
            dot.title = `Aria2 正在启动${elapsed}`;
            setElementHandler(dot, 'click', event => {
                event.stopPropagation();
                showToast(`Aria2 正在启动${elapsed}，启动超时后会显示具体故障`, 'info');
            });
        } else if (aria2Status === 'failed') {
            dot.classList.add('failed');
            dot.title = 'Aria2 启动失败（点击查看原因）';
            setElementHandler(dot, 'click', event => {
                event.stopPropagation();
                showToast(diagnostics.last_error || 'Aria2 进程已退出或启动失败，当前将使用原生下载兜底', 'error', 5000);
            });
        } else {
            dot.classList.add('disconnected');
            dot.title = 'Aria2 未连接（点击查看详情）';
            setElementHandler(dot, 'click', event => {
                event.stopPropagation();
                showToast(diagnostics.last_error || 'Aria2 RPC 当前不可达，下载任务会尝试恢复或使用原生兜底', 'error', 5000);
            });
        }
    });
}

export async function loadDownloadStatus() {
    try {
        const [result, health] = await Promise.all([
            fetchDownloadSnapshot(),
            fetchDownloadHealth(),
        ]);
        if (result.code === 0) {
            const statuses = result.data?.statuses || {};

            if (health.code === 0) updateAria2StatusDot(health);

            // 恢复下载进度到内存
            for (const key in statuses) {
                const s = statuses[key];
                _state.manualDownloadProgress[key] = {
                    bvid: s.bvid,
                    type: s.type,
                    status: s.status,
                    progress_percent: s.progress_percent,
                    downloaded_size: s.downloaded_size,
                    total_size: s.total_size,
                    speed: s.speed,
                    filename: s.title || s.filename
                };

                // 刷新手动下载按钮状态
                updateManualDownloadProgress(s.bvid, s.type);
            }
            _state.currentDownloadStatuses = statuses;
            patchBoardCardProgress(statuses);
        }
    } catch (e) {
        // 网络错误时将 Aria2 指示点置为断开，避免给用户"状态正常"的错觉
        console.warn('加载下载状态失败:', e);
        updateAria2StatusDot({}, 'disconnected');
    }
}

export function startProgressUpdates() {
    if (_state.downloadPollTimer) clearTimeout(_state.downloadPollTimer);
    const poll = async () => {
        if (!document.hidden) await loadDownloadStatus();
        const delay = _state.wsConnected ? 10000 : 1500;
        _state.downloadPollTimer = setTimeout(poll, delay);
    };
    poll();
}

// ==================== 工具函数 ====================
// 全局 HTML 转义工具，避免 XSS。同时用于文本内容与属性值（' 也被转义为 &#039;）。
export function showToast(msg, type = 'info', duration = 2700) {
    const isNetworkMessage = msg === _NETWORK_ERR_MSG;
    if (isNetworkMessage && _state.networkToastEl) {
        return { el: _state.networkToastEl, close: dismissNetworkToast };
    }
    const handle = renderToast(msg, type, duration, _state);
    if (isNetworkMessage && handle) _state.networkToastEl = handle.el;
    return handle;
}

export function confirmDialog(message, opts) {
    opts = opts || {};
    return new Promise((resolve) => {
        const modal = document.getElementById('confirm-modal');
        const msgEl = document.getElementById('confirm-modal-message');
        const okBtn = document.getElementById('confirm-modal-ok');
        const cancelBtn = document.getElementById('confirm-modal-cancel');
        const titleEl = document.getElementById('confirm-modal-title');
        if (!modal || !msgEl || !okBtn || !cancelBtn) {
            // 兑底：元素缺失时降级为原生 confirm
            resolve(window.confirm(message));
            return;
        }
        if (titleEl) titleEl.textContent = opts.title || '请确认';
        msgEl.innerHTML = escapeHtml(message).replace(/\n/g, '<br>');
        okBtn.textContent = opts.okText || '确定';
        cancelBtn.textContent = opts.cancelText || '取消';
        okBtn.className = opts.danger ? 'btn btn-danger' : 'btn btn-primary';

        const cleanup = () => {
            modal.classList.remove('active');
            okBtn.removeEventListener('click', onOk);
            cancelBtn.removeEventListener('click', onCancel);
            modal.removeEventListener('click', onBackdrop);
            document.removeEventListener('keydown', onKey);
        };
        const onOk = () => { cleanup(); resolve(true); };
        const onCancel = () => { cleanup(); resolve(false); };
        const onBackdrop = (e) => { if (e.target === modal) onCancel(); };
        const onKey = (e) => {
            if (e.key === 'Escape') { e.preventDefault(); onCancel(); }
            else if (e.key === 'Enter') { e.preventDefault(); onOk(); }
        };
        okBtn.addEventListener('click', onOk);
        cancelBtn.addEventListener('click', onCancel);
        modal.addEventListener('click', onBackdrop);
        document.addEventListener('keydown', onKey);
        modal.classList.add('active');
        setTimeout(() => okBtn.focus(), 50);
    });
}
