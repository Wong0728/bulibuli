import { _state, webSocketMessageKey, _NETWORK_ERR_MSG } from './state.js';
import { ApiError, requestEnvelope } from './api.js';
import { escapeHtml } from './utils.js';
import { switchTab, showQRCodeLogin, refreshQRCode, closeQRCodeModal, toggleManualCookie, saveManualCookie, logoutAccount } from './bootstrap.js';
import { loadMoreManualQuery, doManualResolve } from './manual.js';
import { getDownloadLinks, downloadAudioWithQuality, downloadVideoWithQuality, downloadDanmaku, downloadComments } from './media-links.js';
import {
    showAddBloggerModal,
    closeAddBloggerModal,
    confirmAddBlogger,
    showContextMenu,
    handleContextMenuEdit,
    handleContextMenuDelete,
    closeEditBloggerModal,
    confirmEditBlogger,
    addActiveWindowRow,
    removeActiveWindowRow,
    toggleActiveWindowMode,
    addActiveWindowPreset,
    loadKnownBloggerIntoAddForm
} from './modal.js';
import { saveBloggers, selectBlogger, renderBloggerLogs, startLogRefresh, cleanupBloggerNowByUid } from './blogger.js';
import { removeKnownBlogger, clearKnownBloggers, closeBloggerNoticeModal, acknowledgeBloggerChange, acknowledgeAllBloggerChanges } from './blogger-search.js';
import { updateDownloadLists, patchSingleCardProgress, pauseDownload, resumeDownload } from './download-queue.js';
import { switchBoardTab, loadHistoryBoard, updateDownloadProgressInList } from './history.js';
import { startProgressUpdates, showToast, confirmDialog } from './download-status.js';
import { addTimePoint, removeTimePoint, browseFFmpegPath, testFFmpeg, saveSettings, resetSettings, loadSettings, testDownload, restartAria2 } from './settings.js';
import { openVideoDrawer, closeVideoDrawer, openVideoDrawerFromManual } from './drawer.js';
import { burnMedia, deleteVideoRecord, refreshVideoInfo, loadBvidLogs, loadDrawerComments, loadDrawerDanmaku, selectQualityPill, startVideoDownload, retryVideoDownload, startVideoDownloadFromManual, toggleAllPages, openVideoPage, downloadCover, startSeasonDownload, toggleAllEpisodes } from './media-actions.js';
import './live.js';

// B站视频监控助手 - feature modules
// 连接后端API

// ==================== API基础配置 ====================
const _API_BASE = '';

// ==================== WebSocket配置 ====================
_state.wsConnected = false;  // WebSocket 连接状态标志，用于 HTTP 轮询回退

export function setElementHandler(element, eventName, handler) {
    const handlers = _state.elementHandlers.get(element) || {};
    if (handlers[eventName]) {
        element.removeEventListener(eventName, handlers[eventName]);
    }
    handlers[eventName] = handler;
    _state.elementHandlers.set(element, handlers);
    element.addEventListener(eventName, handler);
}

export function setVisible(element, visible) {
    if (element) element.hidden = !visible;
}

export function setTone(element, tone = null) {
    if (!element) return;
    element.classList.remove('tone-brand', 'tone-success', 'tone-error');
    if (tone) element.classList.add(`tone-${tone}`);
}

export function acceptWebSocketMessage(namespace, data) {
    const key = webSocketMessageKey(namespace, data);
    if (!key) return false;
    if (_state.seenWebSocketMessages.has(key)) return false;
    _state.seenWebSocketMessages.add(key);
    if (_state.seenWebSocketMessages.size > 2000) {
        const oldest = _state.seenWebSocketMessages.values().next().value;
        _state.seenWebSocketMessages.delete(oldest);
    }
    return true;
}

const _declarativeActions = {
    'switch-board-tab': el => switchBoardTab(el.dataset.boardTab),
    'close-video-drawer': () => closeVideoDrawer(),
    'show-qr-login': () => showQRCodeLogin(),
    'logout-account': () => logoutAccount(),
    'toggle-manual-cookie': () => toggleManualCookie(),
    'save-manual-cookie': () => saveManualCookie(),
    'test-download': () => testDownload(),
    'restart-aria2': el => restartAria2(el),
    'add-time-point': () => addTimePoint(),
    'browse-ffmpeg-path': () => browseFFmpegPath(),
    'test-ffmpeg': () => testFFmpeg(),
    'save-settings': el => saveSettings(el),
    'reset-settings': () => resetSettings(),
    'load-settings': () => loadSettings(),
    'close-add-blogger-modal': () => closeAddBloggerModal(),
    'confirm-add-blogger': () => confirmAddBlogger(),
    'close-edit-blogger-modal': () => closeEditBloggerModal(),
    'confirm-edit-blogger': () => confirmEditBlogger(),
    'add-active-window': el => addActiveWindowRow(el.dataset.scope || 'edit'),
    'active-window-preset': el => addActiveWindowPreset(el.dataset.scope || 'edit', el.dataset.preset),
    'remove-active-window': el => removeActiveWindowRow(el),
    'close-blogger-notice-modal': () => closeBloggerNoticeModal(),
    'acknowledge-all-blogger-changes': () => acknowledgeAllBloggerChanges(),
    'close-qr-modal': () => closeQRCodeModal(),
    'refresh-qr-code': () => refreshQRCode(),
    'context-menu-edit': () => handleContextMenuEdit(),
    'context-menu-delete': () => handleContextMenuDelete(),
    'open-manual-video': el => openVideoDrawerFromManual(el.dataset.bvid),
    'load-more-manual': () => loadMoreManualQuery(),
    'load-more-history': el => loadHistoryBoard(el.dataset.tab, { append: true }),
    'get-download-links': el => getDownloadLinks(el.dataset.bvid, el.dataset.title),
    'download-video': el => downloadVideoWithQuality(el.dataset.bvid, el.dataset.title, el.dataset.mode),
    'download-audio': el => downloadAudioWithQuality(el.dataset.bvid, el.dataset.title, el.dataset.mode),
    'download-danmaku': el => downloadDanmaku(el.dataset.bvid, el.dataset.source),
    'download-comments': el => downloadComments(el.dataset.bvid, el.dataset.source),
    'select-blogger': el => selectBlogger(Number(el.dataset.bloggerId)),
    'switch-tab': el => switchTab(el.dataset.tab),
    'cleanup-blogger': el => cleanupBloggerNowByUid(el.dataset.uid),
    'open-video': el => openVideoDrawer(el.dataset.bvid),
    'remove-time-point': el => removeTimePoint(Number(el.dataset.hours)),
    'remove-known-blogger': el => removeKnownBlogger(el.dataset.uid),
    'clear-known-bloggers': () => clearKnownBloggers(),
    'save-bloggers': () => saveBloggers(),
    'acknowledge-blogger-change': el => acknowledgeBloggerChange(el.dataset.uid),
    'retry-video': el => retryVideoDownload(el.dataset.bvid),
    'start-video': el => startVideoDownload(el.dataset.bvid),
    'pause-download': el => pauseDownload(Number(el.dataset.taskId)),
    'resume-download': el => resumeDownload(Number(el.dataset.taskId)),
    'delete-video': el => deleteVideoRecord(el.dataset.bvid),
    'load-drawer-comments': el => loadDrawerComments(el.dataset.bvid, el.dataset.path),
    'load-drawer-danmaku': el => loadDrawerDanmaku(el.dataset.bvid, el.dataset.path),
    'refresh-video-info': el => refreshVideoInfo(el.dataset.bvid),
    'download-cover': el => downloadCover(el.dataset.bvid),
    'open-video-page': el => openVideoPage(el.dataset.bvid),
    'load-bvid-logs': el => loadBvidLogs(el.dataset.bvid),
    'burn-media': el => burnMedia(el.dataset.bvid, el.dataset.kind, el),
    'select-quality': el => selectQualityPill(el, Number(el.dataset.qn)),
    'start-manual-video': el => startVideoDownloadFromManual(el.dataset.bvid),
    'toggle-all-pages': () => toggleAllPages(),
    'resolve-link': () => doManualResolve(),
    'start-season-download': el => startSeasonDownload(el.dataset.mediaType, el.dataset.seasonTitle),
    'toggle-all-episodes': () => toggleAllEpisodes(),
    'show-add-blogger-modal': () => showAddBloggerModal(),
};

const _declarativeChangeActions = {
    'toggle-active-window-mode': el => toggleActiveWindowMode(el.dataset.scope || 'edit'),
    'load-known-blogger-config': () => loadKnownBloggerIntoAddForm(),
};

document.addEventListener('click', event => {
    const target = event.target.closest('[data-action]');
    if (!target) return;
    const handler = _declarativeActions[target.dataset.action];
    if (!handler) return;
    event.preventDefault();
    handler(target);
});

document.addEventListener('change', event => {
    const target = event.target.closest('[data-change-action]');
    if (!target) return;
    const handler = _declarativeChangeActions[target.dataset.changeAction];
    if (handler) handler(target);
});

document.addEventListener('contextmenu', event => {
    const target = event.target.closest('[data-context-blogger-id]');
    if (!target) return;
    showContextMenu(event, Number(target.dataset.contextBloggerId));
});

document.addEventListener('keydown', event => {
    const bloggerItem = event.target.closest?.('.blogger-list-item[data-blogger-id]');
    if (!bloggerItem || event.target !== bloggerItem) return;
    if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        bloggerItem.click();
    }
});

document.addEventListener('error', event => {
    const image = event.target;
    if (!(image instanceof HTMLImageElement) || !image.dataset.imageError) return;
    switch (image.dataset.imageError) {
        case 'hide':
            image.hidden = true;
            break;
        case 'remove':
            image.hidden = true;
            break;
        case 'avatar-fallback':
            image.hidden = true;
            image.parentElement?.classList.add('avatar-fallback');
            if (image.parentElement) {
                image.parentElement.textContent = image.dataset.fallbackText || '?';
            }
            break;
        case 'show-next':
            image.hidden = true;
            setVisible(image.nextElementSibling, true);
            break;
        case 'thumb-fallback':
            image.hidden = true;
            image.parentElement?.classList.add('thumb-fallback');
            break;
    }
}, true);

// ==================== 全局网络状态追踪 ====================
_state.networkFailCount = 0;          // 连续网络（连接）失败计数；仅统计真正的连接失败
const _NETWORK_FAIL_THRESHOLD = 1;  // 达到即显示顶栏横幅（真正断连时立即提示，与右上角 toast 同步）
// 各模块共用的网络错误文案由 state.js 导出。
// 当前显示中的网络错误 toast（单例，避免大量请求失败时堆叠）
_state.networkToastEl = null;
// 浏览器在线状态（navigator.onLine 可能不准，这里以浏览器事件 + apiRequest 失败计数共同维护）
_state.isNetworkOnline = navigator.onLine !== false;

// 初始化WebSocket连接
export function initWebSocket() {
    updateServerStatus('connecting');

    try {
        _state.socket = io({
            reconnection: true,
            // 保持无限重连，但限制退避上限，确保后端长时间重启后仍可自动恢复。
            reconnectionDelay: 2000,
            reconnectionDelayMax: 15000,
            timeout: 10000
        });

        _state.socket.on('connect', function() {
            _state.isWebSocketConnected = true;
            _state.wsConnected = true;
            updateServerStatus('connected');

            // 订阅下载进度更新
            _state.socket.emit('download:subscribe');

            // 重新订阅当前选中博主的日志
            if (_state.selectedBloggerId !== null) {
                const blogger = _state.bloggers.find(b => b.id === _state.selectedBloggerId);
                if (blogger && blogger.uid) {
                    _state.socket.emit('blogger:logs:subscribe', { uid: blogger.uid });
                }
            }

            // WebSocket 已连接，停止 HTTP 轮询回退
            if (_state.progressUpdateInterval) {
                clearInterval(_state.progressUpdateInterval);
                _state.progressUpdateInterval = null;
            }
            if (_state.logRefreshInterval) {
                clearInterval(_state.logRefreshInterval);
                _state.logRefreshInterval = null;
            }
        });

        _state.socket.on('disconnect', function() {
            _state.isWebSocketConnected = false;
            _state.wsConnected = false;
            updateServerStatus('disconnected');

            // WebSocket 断开，启动 HTTP 轮询回退
            startProgressUpdates();
            if (_state.selectedBloggerId !== null) {
                startLogRefresh();
            }
        });

        _state.socket.on('connect_error', function(error) {
            // 连接失败时保持"连接中"态；Socket.IO 已按配置自动重连，
            // 真正断连由 disconnect 处理器启动 HTTP 轮询兜底，此处无需额外动作
            updateServerStatus('connecting');
        });

        _state.socket.on('reconnect_failed', function() {
            updateServerStatus('disconnected');
        });

        _state.socket.on('log:update', function(data) {
            if (!acceptWebSocketMessage('log', data)) return;

            // 收到日志更新
            if (data.uid && _state.bloggerStates) {
                // 找到对应的博主ID
                const blogger = _state.bloggers.find(b => b.uid === data.uid);
                if (blogger && _state.bloggerStates[blogger.id]) {
                    if (!_state.bloggerStates[blogger.id].logs) {
                        _state.bloggerStates[blogger.id].logs = [];
                    }
                    _state.bloggerStates[blogger.id].logs.push({
                        time: data.time,
                        timestamp: data.timestamp || Date.now(),  // ← 使用完整时间戳
                        level: data.level,
                        msg: data.message
                    });

                    // 限制日志数量
                    if (_state.bloggerStates[blogger.id].logs.length > 100) {
                        _state.bloggerStates[blogger.id].logs.shift();
                    }

                    // 如果当前正在查看该博主，更新显示
                    if (_state.selectedBloggerId === blogger.id) {
                        renderBloggerLogs(blogger.id);
                    }
                }
            }
        });

        _state.socket.on('download:progress', function(data) {
            if (!acceptWebSocketMessage('download', data)) return;
            // 收到下载进度更新
            const bvid = data.bvid;
            const taskType = data.type || 'video';
            const stateKey = `${bvid}_${taskType}`;
            const oldStatus = _state.manualDownloadProgress[stateKey]?.status;

            // 始终保存进度到内存，确保状态不因 DOM 元素缺失而丢失
            _state.manualDownloadProgress[stateKey] = {
                ..._state.manualDownloadProgress[stateKey],
                ...data
            };

            // 对终态变化显示 toast 通知（不论用户在哪个页面）
            const status = data.status;
            let terminalTransition = false;
            if (oldStatus && oldStatus !== status) {
                terminalTransition = ['completed', 'merged', 'failed', 'merge_failed'].includes(status);
                if (status === 'completed') {
                    const title = _state.manualDownloadProgress[stateKey]?.filename || bvid;
                    showToast(`下载完成: ${title}`, 'success');
                } else if (status === 'merged') {
                    const title = _state.manualDownloadProgress[stateKey]?.filename || bvid;
                    showToast(`合并完成: ${title}`, 'success');
                } else if (status === 'failed' || status === 'merge_failed') {
                    const errorMsg = data.error || data.message || (status === 'merge_failed' ? '合并失败' : '下载失败');
                    showToast(`${status === 'merge_failed' ? '合并失败' : '下载失败'}: ${bvid} - ${errorMsg}`, 'error');
                }
            }

            // 统一更新所有相关的 UI 组件（下载列表、手动下载按钮等）
            updateDownloadProgressInList(bvid, data);

            // WebSocket 收到进度时，直接更新看板卡片（无需等 HTTP 轮询）
            if (data.status === 'downloading' || data.status === 'pending') {
                patchSingleCardProgress(bvid, data);
            }

            // 新任务或终态变化时防抖刷新看板。
            if (!oldStatus || (oldStatus !== status && terminalTransition)) {
                if (document.getElementById('tab-history')?.classList.contains('active')) {
                    clearTimeout(_state.boardRefreshTimer);
                    _state.boardRefreshTimer = setTimeout(() => {
                        if (typeof loadHistoryBoard === 'function') {
                            loadHistoryBoard(_state.currentBoardTab || 'completed');
                        }
                        if (terminalTransition) updateDownloadLists();
                    }, terminalTransition ? 300 : 800);
                }
            }
        });

        _state.socket.on('connected', function(data) {
            acceptWebSocketMessage('connected', data);
        });

        _state.socket.on('subscribed', function(data) {
            acceptWebSocketMessage('subscribed', data);
        });

        _state.socket.on('bili:risk-control', function(data) {
            if (!acceptWebSocketMessage('bili:risk-control', data)) return;
            showSystemModal('B站风控', data.message || '请求触发 B站风控，请稍后重试。');
        });

        _state.socket.on('bili:auth-expired', function(data) {
            if (!acceptWebSocketMessage('bili:auth-expired', data)) return;
            showSystemModal('登录已过期', data.message || '登录凭证已失效，请重新登录。', () => showQRCodeLogin());
        });

        _state.socket.on('download:disk-full', function(data) {
            if (!acceptWebSocketMessage('download:disk-full', data)) return;
            showSystemModal('磁盘空间不足', data.message || '下载已暂停，请释放磁盘空间后重试。');
        });

    } catch (e) {
        updateServerStatus('disconnected');
    }
}

window.addEventListener('beforeunload', () => {
    if (_state.socket) {
        const blogger = _state.bloggers.find(item => item.id === _state.selectedBloggerId);
        if (blogger) {
            _state.socket.emit('blogger:logs:unsubscribe', { uid: blogger.uid });
        }
        _state.socket.disconnect();
    }
    cleanupControllers();
    if (_state.progressUpdateInterval) clearInterval(_state.progressUpdateInterval);
    if (_state.logRefreshInterval) clearInterval(_state.logRefreshInterval);
    if (_state.qrcodePollInterval) clearInterval(_state.qrcodePollInterval);
    if (_state.aria2StatusInterval) clearInterval(_state.aria2StatusInterval);
});

// 更新服务器连接状态指示器
export function updateServerStatus(status) {
    const indicator = document.getElementById('server-status-indicator');
    if (!indicator) return;

    indicator.classList.remove('connected', 'connecting', 'disconnected');
    indicator.classList.add(status);

    const textEl = indicator.querySelector('.server-status-text');
    switch (status) {
        case 'connected':
            indicator.title = '已连接到服务器';
            if (textEl) textEl.textContent = '已连接';
            setElementHandler(indicator, 'click', () => showToast('服务器连接正常', 'success'));
            break;
        case 'connecting':
            indicator.title = '正在连接服务器...';
            if (textEl) textEl.textContent = '连接中';
            setElementHandler(indicator, 'click', () => showToast('正在尝试连接服务器...', 'info'));
            break;
        case 'disconnected':
            indicator.title = '未连接到服务器，请检查后端服务是否已启动';
            if (textEl) textEl.textContent = '未连接';
            setElementHandler(indicator, 'click', () => showToast('无法连接到服务器，请检查后端服务是否已启动', 'error'));
            break;
    }
}

// 用于取消请求的AbortController
_state.currentControllers = [];

export function createAbortController() {
    const controller = new AbortController();
    _state.currentControllers.push(controller);
    return controller;
}

export function cleanupControllers() {
    _state.currentControllers.forEach(ctrl => {
        ctrl.abort();
    });
    _state.currentControllers = [];
}

export async function showSystemModal(title, message, onConfirm = null) {
    const confirmed = await confirmDialog(message, {
        title,
        okText: onConfirm ? '立即处理' : '知道了',
        cancelText: '关闭',
    });
    if (confirmed && onConfirm) onConfirm();
}

export async function apiRequest(url, options = {}) {
    const controller = createAbortController();
    try {
        const method = (options.method || 'GET').toUpperCase();
        const headers = { ...(options.headers || {}) };
        if (!['GET', 'HEAD', 'OPTIONS'].includes(method) && _state.csrfToken) {
            headers['X-CSRF-Token'] = _state.csrfToken;
        }
        const envelope = await requestEnvelope(url, {
            signal: controller.signal,
            ...options,
            headers,
        }, {
            onUnauthorized: error => {
                if (error.status === 401) {
                    window.location.reload();
                    return;
                }
                showSystemModal('登录已过期', error.message, () => showQRCodeLogin());
            },
            onRiskControl: error => showSystemModal('B站风控', error.message),
        });
        onNetworkRecovered();
        // 所有 API 调用统一返回 `{ code, message, data }`，禁止把 data 展开到顶层。
        // 调用方必须显式读取 response.data，避免同名字段覆盖信封元数据。
        return envelope;
    } catch (error) {
        if (error.name === 'AbortError' || error instanceof ApiError) throw error;
        _state.networkFailCount++;
        _state.isNetworkOnline = false;
        updateNetworkBanner();
        updateNetworkDisabledButtons();
        showToast(_NETWORK_ERR_MSG, 'error', 5000);
        throw new ApiError(0, _NETWORK_ERR_MSG, { offline: true, cause: error });
    } finally {
        const index = _state.currentControllers.indexOf(controller);
        if (index > -1) _state.currentControllers.splice(index, 1);
    }
}
export function updateNetworkBanner() {
    let banner = document.getElementById('network-error-banner');
    if (!banner) return;

    if (_state.networkFailCount >= _NETWORK_FAIL_THRESHOLD) {
        banner.classList.add('show');
    } else {
        banner.classList.remove('show');
    }
}

// 给需要联网的按钮统一加/解 disabled 类（不破坏原有 disabled 属性语义）
export function updateNetworkDisabledButtons() {
    const selectors = [
        '#tab-manual .btn-primary',
        '#tab-manual .manual-download-btn',
        '.drawer-btn-primary',
        '#detail-start-btn',
        '#board-refresh-btn',
    ];
    document.querySelectorAll(selectors.join(',')).forEach(btn => {
        if (!_state.isNetworkOnline) {
            btn.dataset.networkDisabled = 'true';
            btn.classList.add('network-disabled');
        } else {
            delete btn.dataset.networkDisabled;
            btn.classList.remove('network-disabled');
        }
    });
}

// 检查按钮是否因断网被禁用，若是则提示用户
export function checkNetworkBeforeAction() {
    if (!_state.isNetworkOnline) {
        showToast('网络未恢复，请检查网络后重试', 'warning');
        return false;
    }
    return true;
}

// 网络恢复：清零计数、隐藏顶栏横幅、关闭右上角网络错误 toast
export function onNetworkRecovered() {
    if (_state.networkFailCount !== 0) {
        _state.networkFailCount = 0;
        updateNetworkBanner();
    }
    dismissNetworkToast();
    _state.isNetworkOnline = true;
    updateNetworkDisabledButtons();
}

// 关闭当前的网络错误 toast（若有）
export function dismissNetworkToast() {
    if (_state.networkToastEl) {
        const el = _state.networkToastEl;
        _state.networkToastEl = null;
        el.classList.add('msg-toast-leaving');
        setTimeout(() => el.remove(), 300);
    }
}

export async function apiPost(url, data) {
    return apiRequest(url, {
        method: 'POST',
        body: JSON.stringify(data)
    });
}

export async function apiPut(url, data) {
    return apiRequest(url, {
        method: 'PUT',
        body: JSON.stringify(data)
    });
}

export async function apiGet(url) {
    return apiRequest(url, {
        method: 'GET'
    });
}

// ==================== 全局状态 ====================
_state.manualDownloadProgress = {};  // 存储手动下载的进度 {bvid: {progress, status, speed, etc}}
_state.serverOffset = 0;
_state.nextCheckTimestamp = 0;
_state.bloggers = [];
_state.bloggerIdCounter = 0;
_state.isTaskRunning = false;
_state.progressUpdateInterval = null;
_state.urlExpiryTimers = {};
_state.videoTitles = {};
_state.cookieWarningShown = false;
_state.loginValid = false;  // 由 updateLoginCard 维护：当前是否已登录且有效

// ==================== Cookies 警告横幅 ====================
export async function checkCookiesStatus() {
    const banner = document.getElementById('cookie-warning-banner');
    try {
        const result = await apiGet('/api/cookies/status');
        // 断网时不改动登录态/横幅（避免断网误报“未登录”），网络提示统一由 toast + 顶栏横幅处理
        if (result.offline) return;
        // 顺带刷新顶部登录卡片与设置页登录状态
        const data = result.data || {};
        updateLoginCard(data);
        if (data.valid) {
            if (banner) banner.hidden = true;
            _state.cookieWarningShown = false;
        } else {
            if (banner) {
                const span = banner.querySelector('span');
                if (span) span.textContent = data.has_cookies
                    ? '当前 B 站登录已失效，请重新登录，部分功能受限（仅能获取低清晰度视频）。'
                    : '未登录 B 站账号，部分功能受限（仅能获取低清晰度视频）。';
                banner.hidden = false;
            }
            _state.cookieWarningShown = true;
        }
    } catch (e) {
        updateLoginCard(null);
        if (banner) banner.hidden = false;
        _state.cookieWarningShown = true;
    }
}

export function dismissCookieWarning() {
    const banner = document.getElementById('cookie-warning-banner');
    if (banner) {
        banner.hidden = true;
    }
}

// 渲染顶部登录用户卡片 + 设置页登录状态（info 来自 /api/cookies/status）
export function updateLoginCard(info) {
    _state.loginValid = !!(info && info.valid);
    const card = document.getElementById('login-user-card');
    const prompt = document.getElementById('login-prompt-btn');
    const settingsStatus = document.getElementById('cookie-login-status');

    if (_state.loginValid) {
        const face = info.face ? `/api/video/proxy-image?url=${encodeURIComponent(info.face)}` : '';
        const isVip = (info.vip_status || 0) > 0;
        const vipText = info.vip_label || (isVip ? '大会员' : '');
        const faceImg = face
            ? `<img class="login-user-face" src="${face}" alt="" data-image-error="hide">`
            : `<div class="login-user-face login-user-face-ph"><i class="fa-solid fa-user"></i></div>`;
        const vipBadge = isVip ? `<span class="login-vip-badge">${escapeHtml(vipText || '大会员')}</span>` : '';
        if (card) {
            card.hidden = false;
            card.innerHTML = `
                ${faceImg}
                <div class="login-user-meta">
                    <span class="login-user-name">${escapeHtml(info.uname || '')}</span>
                    <span class="login-user-sub">Lv${Number(info.level) || 0} ${vipBadge}</span>
                </div>
                <button class="login-switch-btn" data-action="show-qr-login" title="切换账号"><i class="fa-solid fa-right-left"></i></button>`;
        }
        if (prompt) prompt.hidden = true;
        if (settingsStatus) {
            settingsStatus.innerHTML = `
                ${faceImg}
                <div class="login-user-meta">
                    <span class="login-user-name">${escapeHtml(info.uname || '')} ${vipBadge}</span>
                    <span class="login-user-sub">UID ${escapeHtml(String(info.mid || '--'))} · Lv${Number(info.level) || 0} · 已登录</span>
                </div>`;
        }
    } else {
        if (card) card.hidden = true;
        if (prompt) {
            prompt.hidden = false;
            prompt.innerHTML = `<i class="fa-solid fa-user"></i> ${info && info.has_cookies ? '登录失效·重新登录' : '未登录·点击登录'}`;
        }
        if (settingsStatus) {
            settingsStatus.innerHTML = `<span class="login-user-sub"><i class="fa-solid fa-circle-exclamation"></i> ${info && info.has_cookies ? '当前 Cookie 无效或已过期' : '尚未登录 B 站账号'}</span>`;
        }
    }
}

// 刷新登录信息（登录卡片 + 警告横幅）
export async function refreshLoginInfo() {
    await checkCookiesStatus();
}

// 当前选中的博主ID
_state.selectedBloggerId = null;
_state.selectedDownloadBloggerId = null;

// 每个博主的独立状态
_state.bloggerStates = {};
