import { _state } from './state.js';
import { setTone, initWebSocket, updateNetworkBanner, updateNetworkDisabledButtons, apiPost, apiGet, checkCookiesStatus, dismissCookieWarning, refreshLoginInfo } from './core.js';
import { setManualQueryMode, doManualQuery, doManualResolve } from './manual.js';
import { showAddBloggerModal, closeAddBloggerModal, showEditBloggerModal, closeEditBloggerModal } from './modal.js';
import { loadBloggersFromServer, renderBloggerSidebar, startSelectedBlogger, stopSelectedBlogger, startStatusPolling } from './blogger.js';
import { renderUidHistorySelect, onUidHistorySelectChange, renderKnownBloggers, checkBloggerProfileNotices, showBloggerNoticeModal, closeBloggerNoticeModal, searchBloggers } from './blogger-search.js';
import { updateDownloadLists } from './download-queue.js';
import { loadHistoryBoard, manualRefreshBoard, startLastPullPolling } from './history.js';
import { loadDownloadStatus, startProgressUpdates, showToast, confirmDialog } from './download-status.js';
import { onCommentsReplyModeChange, onSidecarArchiveModeChange, toggleSmartDownloadSettings, onVerifyModeChange, onDownloadModeChange, onFFmpegModeChange, refreshFFmpegDetectedPath, loadSettingsFromServer, updatePathPreview, initMobileSidebar } from './settings.js';
import { closeVideoDrawer } from './drawer.js';
import { getQRCodePayload, getQRCodePollState } from './qrcode-contract.js';

// ==================== 初始化 ====================
window.addEventListener('DOMContentLoaded', () => {
    bootstrapApp().catch(showStartupError);
});

async function bootstrapApp() {
    const authResponse = await fetch('/api/auth/state', {
        credentials: 'same-origin',
        cache: 'no-store',
    });
    const authEnvelope = await authResponse.json();
    if (!authResponse.ok || !authEnvelope.data?.authenticated) {
        throw new Error(authEnvelope.message || '登录状态已失效，请刷新页面或重新配对。');
    }
    _state.csrfToken = authEnvelope.data.csrf_token;
    _state.sessionRole = authEnvelope.data.role || 'owner';
    applySessionRole(_state.sessionRole);
    initTabSwitching();
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

    // 先恢复页面依赖的服务端状态，再启动实时连接和轮询。
    await loadBloggersFromServer();
    await loadDownloadStatus();

    // 初始化WebSocket连接（"任务状态已恢复"提示在 connect 回调中弹出，确保后端可用）
    initWebSocket();

    // 启动状态轮询
    startStatusPolling();
    startProgressUpdates();

    // 启动看板"上次拉取"轮询（5s 检查是否超过 60s）
    startLastPullPolling();

    // 初始化移动端侧边栏
    initMobileSidebar();

    // 初始化智能下载设置的事件监听
    const smartDownloadToggle = document.getElementById('setting-enable-smart-download');
    if (smartDownloadToggle) {
        smartDownloadToggle.addEventListener('change', (e) => {
            toggleSmartDownloadSettings(e.target.checked);
        });
    }

    // 初始化 MD5 校验模式切换
    onVerifyModeChange();

    // Cookie、基础配置和可执行路径只对 Owner 可见/可请求。
    if (_state.sessionRole === 'owner') checkCookiesStatus();

    // FFmpeg 路径属于基础配置，Operator / Viewer 不请求该接口。
    if (_state.sessionRole === 'owner') refreshFFmpegDetectedPath();

    // 渲染已添加博主列表（博主搜索标签页）- 异步加载
    renderKnownBloggers();

    // 初始化手动查询页的博主快捷选择下拉 - 异步加载
    renderUidHistorySelect();

    // 检查博主资料变更通知（黄点）
    checkBloggerProfileNotices();

    if (_state.sessionRole === 'owner') loadSettingsFromServer();

    // 设置分区可折叠：点击标题栏收起 / 展开（含键盘支持）
    document.querySelectorAll('.section-collapsible .section-header').forEach(header => {
        header.setAttribute('tabindex', '0');
        header.setAttribute('role', 'button');
        const toggleSection = () => {
            const sec = header.closest('.section-collapsible');
            if (sec) sec.classList.toggle('collapsed');
        };
        header.addEventListener('click', toggleSection);
        header.addEventListener('keydown', (e) => {
            if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleSection(); }
        });
    });

    // 全局 ESC 关闭抽屉 / 弹窗
    document.addEventListener('keydown', (e) => {
        if (e.key !== 'Escape') return;
        const drawer = document.getElementById('video-drawer');
        if (drawer && drawer.classList.contains('active')) { closeVideoDrawer(); return; }
        const openModal = document.querySelector('.modal-overlay.active');
        if (!openModal || openModal.id === 'confirm-modal') return; // confirm-modal 自行处理 ESC
        if (openModal.id === 'qrcode-modal') closeQRCodeModal();
        else if (openModal.id === 'add-blogger-modal') closeAddBloggerModal();
        else if (openModal.id === 'edit-blogger-modal') closeEditBloggerModal();
        else if (openModal.id === 'blogger-notice-modal') closeBloggerNoticeModal();
        else openModal.classList.remove('active');
    });

    // 网络在线/离线事件
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
        // 刷新看板与下载状态
        loadHistoryBoard(_state.currentBoardTab);
        updateDownloadLists();
        // 只重试最近五分钟内可能因离线失败的任务。
        const offlineSince = Math.floor((Date.now() - 5 * 60 * 1000) / 1000);
        apiPost('/api/download/retry-all', { since: offlineSince }).catch(error => {
            showToast(`恢复离线下载任务失败: ${error.message || '未知错误'}`, 'error');
        });
    });
    updateNetworkDisabledButtons();

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
    document.getElementById('setting-comments-reply-mode')?.addEventListener('change', onCommentsReplyModeChange);
    document.getElementById('setting-sidecar-archive-mode')?.addEventListener('change', onSidecarArchiveModeChange);
    document.getElementById('setting-download-mode')?.addEventListener('change', onDownloadModeChange);
    document.getElementById('setting-ffmpeg-mode')?.addEventListener('change', onFFmpegModeChange);
    document.getElementById('setting-auto-organize')?.addEventListener('change', updatePathPreview);
    document.getElementById('setting-path-template')?.addEventListener('input', updatePathPreview);
    document.getElementById('setting-verify-mode')?.addEventListener('change', onVerifyModeChange);
}

function applySessionRole(role) {
    if (role === 'owner') return;
    // Credential and foundation controls are never rendered for delegated
    // sessions.  Backend RBAC is the authoritative enforcement mechanism.
    ['account', 'ai-skill', 'setup-wizard', 'foundation-summary', 'aria2', 'ffmpeg']
        .forEach(section => document.querySelector(`[data-section="${section}"]`)?.setAttribute('hidden', ''));
    document.getElementById('cookie-warning-banner')?.setAttribute('hidden', '');
    document.getElementById('login-user-card')?.setAttribute('hidden', '');
    document.getElementById('login-prompt-btn')?.setAttribute('hidden', '');

    if (role === 'viewer') {
        document.querySelectorAll('button[data-action]').forEach(button => {
            button.disabled = true;
            button.title = 'Viewer 会话仅可查看';
        });
        document.querySelectorAll('input, select, textarea').forEach(control => {
            control.disabled = true;
        });
    }
}

function showStartupError(error) {
    const message = error?.message || '页面初始化失败，请重试。';
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
            <h2>页面启动失败</h2>
            <p>${escapeStartupMessage(message)}</p>
            <button type="button" class="btn btn-primary" data-startup-retry>重试</button>
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
    });
}

export function switchTab(tabName) {
    document.querySelectorAll('.tab-panel, .nav-tab').forEach(el => el.classList.remove('active'));
    document.getElementById(`tab-${tabName}`).classList.add('active');
    document.querySelector(`.nav-tab[data-tab="${tabName}"]`).classList.add('active');
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

// ==================== 配置管理 ====================
// 博主与设置以服务端数据为准，前端不持久化副本。

// ==================== Cookies 管理 ====================
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
            canvasContainer.innerHTML = '<i class="fa-solid fa-exclamation-circle fa-3x" data-js-style="1"></i>';
            hintEl.textContent = '获取二维码失败：响应缺少必要字段，请重试';
            refreshBtn.hidden = false;
        }
    } catch (e) {
        if (!isActiveQRCodeGeneration(generation)) return;
        console.error('[QRCode] 获取二维码失败:', e);
        canvasContainer.innerHTML = '<i class="fa-solid fa-exclamation-circle fa-3x" data-js-style="1"></i>';
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
                statusEl.textContent = '登录成功！Cookie 已安全保存。';
                showToast('扫码登录成功', 'success');

                // 几秒后隐藏 cookie，显示“已保存”
                setTimeout(() => {
                    if (!isActiveQRCodeGeneration(generation)) return;
                    statusEl.textContent = 'Cookie 已保存 ✓';
                }, 4000);

                // 刷新顶部登录卡片与横幅
                setTimeout(() => {
                    if (isActiveQRCodeGeneration(generation)) refreshLoginInfo();
                }, 800);

                setTimeout(() => {
                    if (isActiveQRCodeGeneration(generation)) closeQRCodeModal();
                }, 6000);
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
