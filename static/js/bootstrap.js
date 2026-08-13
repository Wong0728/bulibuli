import { _state, subscribeState } from './state.js';
import { setTone, initWebSocket, updateNetworkBanner, updateNetworkDisabledButtons, setBackendAvailability, apiPost, apiGet, checkCookiesStatus, dismissCookieWarning, refreshLoginInfo } from './core.js';
import { setManualQueryMode, doManualQuery, doManualResolve } from './manual.js';
import { showAddBloggerModal, closeBloggerModal, showEditBloggerModal } from './modal.js';
import { loadBloggersFromServer, renderBloggerSidebar, startSelectedBlogger, stopSelectedBlogger, startStatusPolling } from './blogger.js';
import { renderUidHistorySelect, onUidHistorySelectChange, renderKnownBloggers, checkBloggerProfileNotices, showBloggerNoticeModal, closeBloggerNoticeModal, searchBloggers } from './blogger-search.js';
import { updateDownloadLists } from './download-queue.js';
import { loadHistoryBoard, manualRefreshBoard } from './history.js';
import { loadDownloadStatus, startProgressUpdates, showToast, confirmDialog } from './download-status.js';
import { onCommentsReplyModeChange, onSidecarArchiveModeChange, toggleSmartDownloadSettings, onVerifyModeChange, onDownloadModeChange, onFFmpegModeChange, refreshFFmpegDetectedPath, loadSettingsFromServer, loadSettingsFragment, updatePathPreview, initMobileSidebar } from './settings.js';
import { startPollingScheduler } from './polling.js';
import { closeVideoDrawer } from './drawer.js';
import { getQRCodePayload, getQRCodePollState } from './qrcode-contract.js';

// --- 初始化 ---
const startBootstrap = () => bootstrapApp().catch(showStartupError);
if (document.readyState === 'loading') {
    window.addEventListener('DOMContentLoaded', startBootstrap, { once: true });
} else {
    startBootstrap();
}

async function bootstrapApp() {
    document.getElementById('blogger-notice-modal')?.querySelector('.modal-header span')?.setAttribute('id', 'blogger-notice-title');
    document.getElementById('qrcode-modal')?.querySelector('.modal-header span')?.setAttribute('id', 'qrcode-modal-title');
    initTabSwitching();
    bindNetworkListeners();
    bindGlobalKeyboardInteractions();
    bindPageEventListeners();
    updateNetworkDisabledButtons();

    let authError = null;
    try {
        const authResponse = await fetch('/api/auth/state', {
            credentials: 'same-origin',
            cache: 'no-store',
        });
        let authEnvelope;
        try {
            authEnvelope = await authResponse.json();
        } catch {
            throw new Error('服务响应格式异常，请确认后端服务地址正确。');
        }
        if (!authResponse.ok) {
            throw new Error(authEnvelope.message || `后端服务返回错误（${authResponse.status}）。`);
        }
        const authData = authEnvelope.data || {};
        _state.sessionAuthenticated = authData.authenticated === true;
        _state.csrfToken = authData.csrf_token || null;
        _state.sessionRole = authData.role || 'viewer';
        if (!_state.sessionAuthenticated) _state.sessionRole = 'viewer';
    } catch (error) {
        authError = error;
        _state.sessionAuthenticated = false;
        _state.sessionRole = 'viewer';
        setBackendAvailability(false);
    }

    try {
        await loadSettingsFragment();
    } catch (error) {
        if (!authError) authError = error;
        setBackendAvailability(false);
    }

    applySessionRole(_state.sessionRole);
    bindSettingsInteractions();
    bindCollapsibleSections();
    initMobileSidebar();
    onVerifyModeChange();
    updateNetworkDisabledButtons();

    if (authError) {
        showStartupNotice(formatStartupError(authError));
        return;
    }
    if (!_state.sessionAuthenticated) {
        showStartupNotice('当前设备尚未完成配对，请先在配对页完成设置。');
        return;
    }

    // 各项服务端状态彼此独立，单项失败不阻断其他页面能力。
    await Promise.allSettled([loadBloggersFromServer(), loadDownloadStatus()]);
    initWebSocket();
    startStatusPolling();
    startProgressUpdates();
    startPollingScheduler();

    if (_state.sessionRole === 'owner') {
        checkCookiesStatus();
        refreshFFmpegDetectedPath();
        loadSettingsFromServer();
    }
    renderKnownBloggers();
    renderUidHistorySelect();
    checkBloggerProfileNotices();
}

function bindPageEventListeners() {
    // --- 页面事件绑定 ---
    document.getElementById('login-prompt-btn')?.addEventListener('click', () => showQRCodeLogin());
    document.getElementById('cookie-warning-settings-link')?.addEventListener('click', () => switchTab('settings'));
    document.querySelector('.cookie-warning-close')?.addEventListener('click', () => dismissCookieWarning());
    document.getElementById('blogger-notice-dot')?.addEventListener('click', () => showBloggerNoticeModal());
    document.getElementById('blogger-search-btn')?.addEventListener('click', () => searchBloggers());
    document.getElementById('blogger-search-input')?.addEventListener('keydown', event => {
        if (event.key === 'Enter') searchBloggers();
    });
    document.querySelectorAll('.manual-mode-switch .mode-btn').forEach(btn => {
        btn.addEventListener('click', () => setManualQueryMode(btn.dataset.mode));
    });
    document.getElementById('uid-history-select')?.addEventListener('change', () => onUidHistorySelectChange());
    document.getElementById('manual-query-btn')?.addEventListener('click', () => doManualQuery());
    document.getElementById('manual-link-input')?.addEventListener('keydown', event => {
        if (event.key === 'Enter') doManualResolve();
    });
    document.getElementById('show-add-blogger-btn')?.addEventListener('click', () => showAddBloggerModal());
    document.querySelector('.blogger-detail-title')?.addEventListener('click', () => {
        if (_state.selectedBloggerId !== null) showEditBloggerModal(_state.selectedBloggerId);
    });
    document.getElementById('detail-start-btn')?.addEventListener('click', () => startSelectedBlogger());
    document.getElementById('detail-stop-btn')?.addEventListener('click', () => stopSelectedBlogger());
    document.getElementById('board-refresh-btn')?.addEventListener('click', () => manualRefreshBoard());
}

function bindSettingsInteractions() {
    const parallelInputs = [
        document.getElementById('setting-max-parallel'),
        document.getElementById('setting-max-concurrent-downloads'),
    ].filter(Boolean);
    parallelInputs.forEach(input => {
        input.addEventListener('input', () => {
            parallelInputs.forEach(other => {
                if (other !== input) other.value = input.value;
            });
        });
    });
    document.getElementById('setting-enable-smart-download')?.addEventListener('change', event => {
        toggleSmartDownloadSettings(event.target.checked);
    });
    document.getElementById('setting-comments-reply-mode')?.addEventListener('change', onCommentsReplyModeChange);
    document.getElementById('setting-sidecar-archive-mode')?.addEventListener('change', onSidecarArchiveModeChange);
    document.getElementById('setting-download-mode')?.addEventListener('change', onDownloadModeChange);
    document.getElementById('setting-ffmpeg-mode')?.addEventListener('change', onFFmpegModeChange);
    document.getElementById('setting-auto-organize')?.addEventListener('change', updatePathPreview);
    document.getElementById('setting-path-template')?.addEventListener('input', updatePathPreview);
    document.getElementById('setting-verify-mode')?.addEventListener('change', onVerifyModeChange);
}

function bindCollapsibleSections() {
    document.querySelectorAll('.section-collapsible .section-header').forEach(header => {
        header.setAttribute('tabindex', '0');
        header.setAttribute('role', 'button');
        const section = header.closest('.section-collapsible');
        const toggleSection = () => {
            section?.classList.toggle('collapsed');
            header.setAttribute('aria-expanded', String(!section?.classList.contains('collapsed')));
        };
        header.setAttribute('aria-expanded', String(!section?.classList.contains('collapsed')));
        header.addEventListener('click', toggleSection);
        header.addEventListener('keydown', event => {
            if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); toggleSection(); }
        });
    });
}

function bindGlobalKeyboardInteractions() {
    document.addEventListener('keydown', event => {
        if (event.key !== 'Escape') return;
        const drawer = document.getElementById('video-drawer');
        if (drawer?.classList.contains('active')) { closeVideoDrawer(); return; }
        const openModal = document.querySelector('.modal-overlay.active');
        if (!openModal || openModal.id === 'confirm-modal') return;
        if (openModal.id === 'qrcode-modal') closeQRCodeModal();
        else if (openModal.id === 'blogger-modal') closeBloggerModal();
        else if (openModal.id === 'blogger-notice-modal') closeBloggerNoticeModal();
        else openModal.classList.remove('active');
    });
}

function bindNetworkListeners() {
    window.addEventListener('offline', () => {
        _state.isNetworkOnline = false;
        updateNetworkBanner();
        updateNetworkDisabledButtons();
        showToast('网络已断开，部分功能已禁用', 'warning');
    });
    window.addEventListener('online', () => {
        _state.networkFailCount = 0;
        _state.isNetworkOnline = true;
        updateNetworkBanner();
        updateNetworkDisabledButtons();
        showToast('网络已恢复', 'success');
        if (_state.sessionAuthenticated !== true) return;
        loadHistoryBoard(_state.currentBoardTab);
        updateDownloadLists();
        const offlineSince = Math.floor((Date.now() - 5 * 60 * 1000) / 1000);
        apiPost('/api/download/retry-all', { since: offlineSince }).catch(() => {});
    });
}

function applySessionRole(role) {
    if (role === 'owner') return;
    // delegated 会话不渲染凭证和基础配置控件；后端 RBAC 才是最终权限边界。
    ['account', 'local-config', 'aria2', 'ffmpeg']
        .forEach(section => document.querySelector(`[data-section="${section}"]`)?.setAttribute('hidden', ''));
    document.getElementById('cookie-warning-banner')?.setAttribute('hidden', '');
    document.getElementById('login-user-card')?.setAttribute('hidden', '');
    document.getElementById('login-prompt-btn')?.setAttribute('hidden', '');

    if (role === 'viewer') {
        document.querySelectorAll('[data-action]').forEach(control => {
            if (['close-video-drawer', 'close-blogger-modal', 'close-blogger-notice-modal', 'close-qr-modal'].includes(control.dataset.action)) return;
            control.setAttribute('aria-disabled', 'true');
            control.title = 'Viewer 会话仅可查看';
        });
        document.querySelectorAll('input, select, textarea').forEach(control => {
            control.disabled = true;
        });
    }
}

function showStartupError(error) {
    const message = formatStartupError(error);
    let panel = document.getElementById('startup-error-panel');
    if (!panel) {
        panel = document.createElement('section');
        panel.id = 'startup-error-panel';
        panel.className = 'startup-error-panel';
        panel.setAttribute('role', 'alert');
        document.body.prepend(panel);
    }
    panel.innerHTML = `
        <div class="startup-error-content">
            <h2>页面初始化异常</h2>
            <p>${escapeStartupMessage(message)}</p>
            <button type="button" class="btn btn-primary" data-startup-retry>重试</button>
        </div>
    `;
    panel.querySelector('[data-startup-retry]')?.addEventListener('click', () => window.location.reload());
}

function formatStartupError(error) {
    const message = String(error?.message || '页面初始化失败，请重试。');
    if (/Failed to fetch|NetworkError|Load failed|网络|fetch/i.test(message)) {
        return '无法连接到后端服务。请确认程序正在运行，并检查当前地址和端口。';
    }
    if (/Unexpected token|JSON|响应格式|HTML/i.test(message)) {
        return '后端服务响应格式异常。请确认当前页面由 bulibuli 服务提供，而不是直接打开静态文件。';
    }
    if (/401|403|未登录|登录状态|配对/i.test(message)) {
        return '当前设备尚未完成配对或登录状态已失效，请先完成配对后重试。';
    }
    return '页面初始化失败，请刷新页面或检查后端服务状态。';
}

function showStartupNotice(message) {
    let panel = document.getElementById('startup-error-panel');
    if (!panel) {
        panel = document.createElement('section');
        panel.id = 'startup-error-panel';
        panel.className = 'startup-error-panel startup-notice-panel';
        panel.setAttribute('role', 'status');
        document.body.prepend(panel);
    }
    panel.innerHTML = `
        <div class="startup-error-content">
            <h2>部分服务暂不可用</h2>
            <p>${escapeStartupMessage(message)}</p>
            <button type="button" class="btn btn-primary" data-startup-retry>重试连接</button>
        </div>
    `;
    panel.querySelector('[data-startup-retry]')?.addEventListener('click', () => window.location.reload());
}

function escapeStartupMessage(message) {
    const div = document.createElement('div');
    div.textContent = message;
    return div.innerHTML;
}

export function initTabSwitching() {
    document.querySelectorAll('.nav-tab').forEach(tab => {
        tab.addEventListener('click', () => switchTab(tab.dataset.tab));
        tab.addEventListener('keydown', event => {
            if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                switchTab(tab.dataset.tab);
                return;
            }
            if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
                event.preventDefault();
                const tabs = [...document.querySelectorAll('.nav-tab')];
                const currentIndex = tabs.indexOf(tab);
                const nextIndex = event.key === 'Home' ? 0
                    : event.key === 'End' ? tabs.length - 1
                        : (currentIndex + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
                tabs[nextIndex]?.focus();
            }
        });
    });
}

export function switchTab(tabName) {
    _state.currentTab = tabName;
    const activePanel = document.getElementById(`tab-${tabName}`);
    if (!activePanel) return;
    document.querySelectorAll('.tab-panel').forEach(panel => {
        const active = panel === activePanel;
        panel.classList.toggle('active', active);
        panel.hidden = !active;
    });
    document.querySelectorAll('.nav-tab').forEach(tab => {
        const selected = tab.dataset.tab === tabName;
        tab.classList.toggle('active', selected);
        tab.setAttribute('aria-selected', String(selected));
        tab.tabIndex = selected ? 0 : -1;
    });
    if (tabName === 'history') {
        // 每次进入下载管理都实时从后端拉取看板数据
        loadHistoryBoard(_state.currentBoardTab);
        updateDownloadLists();
    } else if (tabName === 'auto') {
        renderBloggerSidebar();
    } else if (tabName === 'search') {
        // 进入博主搜索页时检查黄点
        checkBloggerProfileNotices();
    } else if (tabName === 'live') {
        if (typeof window.refreshDashboard === 'function') {
            window.refreshDashboard(true);
        }
    }
}

// --- 配置管理 ---
// 博主与设置以服务端数据为准，前端不持久化副本。

// --- Cookie 管理 ---
_state.qrcodePollInterval = null;
_state.qrcodePollGeneration = 0;
_state.qrcodePollInFlight = null;

function invalidateQRCodePolling() {
    _state.qrcodePollGeneration += 1;
    if (_state.qrcodePollInterval) clearInterval(_state.qrcodePollInterval);
    _state.qrcodePollInterval = null;
    return _state.qrcodePollGeneration;
}

function isActiveQRCodeGeneration(generation) {
    return _state.qrcodePollGeneration === generation;
}

function stopQRCodePolling(generation) {
    if (!isActiveQRCodeGeneration(generation)) return;
    if (_state.qrcodePollInterval) clearInterval(_state.qrcodePollInterval);
    _state.qrcodePollInterval = null;
}

export async function showQRCodeLogin() {
    const modal = document.getElementById('qrcode-modal');
    if (modal) {
        modal.classList.add('active');
        refreshQRCode();
    }
}

export async function refreshQRCode() {
    const canvasContainer = document.getElementById('qrcode-canvas');
    const statusEl = document.getElementById('qrcode-status');
    const hintEl = document.getElementById('qrcode-hint');
    const refreshBtn = document.getElementById('refresh-qrcode-btn');
    
    if (!canvasContainer) return;
    const generation = invalidateQRCodePolling();

    canvasContainer.innerHTML = '<div class="loading"></div>';
    statusEl.hidden = true;
    hintEl.textContent = '正在获取二维码...';
    refreshBtn.hidden = true;
    
    try {
        // 检查 QRCode 库是否加载
        if (typeof QRCode === 'undefined') {
            throw new Error('QRCode 库未加载，请检查网络连接或刷新页面');
        }

        const result = await apiGet('/api/cookies/qrcode/generate');
        if (!isActiveQRCodeGeneration(generation)) return;
        const qrcode = getQRCodePayload(result);
        if (qrcode) {
            
            // 清空容器并创建新的 canvas 元素
            canvasContainer.innerHTML = '';
            const qrcanvas = document.createElement('canvas');
            
            // 生成二维码
            await QRCode.toCanvas(qrcanvas, qrcode.url, {
                width: 200,
                margin: 2,
                color: {
                    dark: '#000000',
                    light: '#ffffff'
                }
            });
            
            if (!isActiveQRCodeGeneration(generation)) return;
            canvasContainer.appendChild(qrcanvas);
            hintEl.textContent = '请使用 Bilibili 手机客户端扫码';
            
            // 开始轮询
            startPollingQRCode(qrcode.qrcodeKey, generation);
        } else {
            // 信封层错误由 apiGet 抛异常进 catch，这里仅处理 code=0 但缺字段的异常响应（不回显 'success'）
            canvasContainer.innerHTML = '<i class="fa-solid fa-exclamation-circle fa-3x status-error"></i>';
            hintEl.textContent = '获取二维码失败：响应缺少必要字段，请重试';
            refreshBtn.hidden = false;
        }
    } catch (e) {
        if (!isActiveQRCodeGeneration(generation)) return;
        console.error('[QRCode] 获取二维码失败:', e);
        canvasContainer.innerHTML = '<i class="fa-solid fa-exclamation-circle fa-3x status-error"></i>';
        hintEl.textContent = e.message || '网络请求失败';
        refreshBtn.hidden = false;
    }
}

export function startPollingQRCode(qrcodeKey, generation = _state.qrcodePollGeneration) {
    if (!isActiveQRCodeGeneration(generation)) return;
    stopQRCodePolling(generation);
    
    _state.qrcodePollInterval = setInterval(async () => {
        if (!isActiveQRCodeGeneration(generation) || _state.qrcodePollInFlight === generation) return;
        _state.qrcodePollInFlight = generation;
        try {
            const result = await apiGet(`/api/cookies/qrcode/poll?qrcode_key=${encodeURIComponent(qrcodeKey)}`);
            if (!isActiveQRCodeGeneration(generation)) return;
            const poll = getQRCodePollState(result);
            const statusEl = document.getElementById('qrcode-status');
            const refreshBtn = document.getElementById('refresh-qrcode-btn');

            if (poll.kind === 'success') {
                // 登录成功（后端已自动保存 cookie 到 DB，前端无需再持有）
                stopQRCodePolling(generation);

                statusEl.hidden = false;
                setTone(statusEl, 'success');
                statusEl.textContent = '登录成功，账号信息已更新。';
                showToast('扫码登录成功', 'success');
                // 账号信息刷新完成后短暂保留成功反馈，再关闭弹窗。
                refreshLoginInfo();
                setTimeout(() => {
                    if (isActiveQRCodeGeneration(generation)) closeQRCodeModal();
                }, 1500);
            } else if (poll.kind === 'waiting') {
                // 未扫码
                statusEl.hidden = true;
            } else if (poll.kind === 'scanned') {
                // 已扫码未确认
                statusEl.textContent = poll.message || '已扫码，请在手机上确认';
                statusEl.hidden = false;
                setTone(statusEl, 'brand');
            } else if (poll.kind === 'expired') {
                // 二维码失效
                stopQRCodePolling(generation);

                statusEl.textContent = poll.message || '二维码已失效';
                statusEl.hidden = false;
                setTone(statusEl, 'error');
                refreshBtn.hidden = false;
            } else {
                stopQRCodePolling(generation);
                statusEl.textContent = poll.message || `二维码状态异常（代码 ${poll.code ?? '未知'}），请刷新重试`;
                statusEl.hidden = false;
                setTone(statusEl, 'error');
                refreshBtn.hidden = false;
            }
        } catch (e) {
            if (!isActiveQRCodeGeneration(generation)) return;
            console.error('轮询状态失败:', e);
            stopQRCodePolling(generation);
            const statusEl = document.getElementById('qrcode-status');
            const refreshBtn = document.getElementById('refresh-qrcode-btn');
            statusEl.textContent = `轮询二维码状态失败：${e.message || '请刷新重试'}`;
            statusEl.hidden = false;
            setTone(statusEl, 'error');
            refreshBtn.hidden = false;
        } finally {
            if (_state.qrcodePollInFlight === generation) _state.qrcodePollInFlight = null;
        }
    }, 2000);
}

export function closeQRCodeModal() {
    const modal = document.getElementById('qrcode-modal');
    if (modal) {
        modal.classList.remove('active');
    }
    invalidateQRCodePolling();
}

// 切换“手动粘贴 Cookie”输入框的显隐（高级功能，默认折叠，仅用于输入不回显已保存 Cookie）
export function toggleManualCookie() {
    const box = document.getElementById('manual-cookie-box');
    if (!box) return;
    const show = box.hidden;
    box.hidden = !show;
    if (show) {
        const ta = document.getElementById('manual-cookies');
        if (ta) { ta.value = ''; ta.focus(); }
    }
}

// 手动粘贴其他账号 Cookie 并保存登录（用于切换账号 / 修复无效或权限不足的 Cookie）
export async function saveManualCookie() {
    const ta = document.getElementById('manual-cookies');
    const cookies = (ta?.value || '').trim();
    if (!cookies) {
        showToast('请先粘贴 Cookie 内容', 'error');
        return;
    }
    try {
        const result = await apiPost('/api/cookies/save', { cookies });
        if (result.code === 0) {
            // 安全：保存后立即清空输入框并折叠，页面不保留明文
            if (ta) ta.value = '';
            const box = document.getElementById('manual-cookie-box');
            if (box) box.hidden = true;
            showToast('账号已保存，正在刷新登录信息...', 'success');
            await refreshLoginInfo();
        } else {
            showToast(result.message || '保存失败', 'error');
        }
    } catch (e) {
        showToast('保存 Cookie 失败', 'error');
    }
}

// 退出登录：清空服务器保存的 Cookie（应对错误账号 / 需要重新登录）
export async function logoutAccount() {
    if (!(await confirmDialog('确定要退出当前 B 站账号登录吗？退出后需重新扫码或粘贴 Cookie。', { title: '退出登录', okText: '退出', danger: true }))) return;
    try {
        const result = await apiPost('/api/cookies/save', { cookies: '' });
        if (result.code === 0) {
            showToast('已退出登录', 'info');
            await refreshLoginInfo();
        } else {
            showToast(result.message || '退出失败', 'error');
        }
    } catch (e) {
        showToast('退出登录失败', 'error');
    }
}
