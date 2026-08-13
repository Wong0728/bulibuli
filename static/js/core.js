import { _state, webSocketMessageKey, _NETWORK_ERR_MSG } from './state.js';
import { ApiError, requestEnvelope } from './api.js';
import {
    updateNetworkBanner,
    updateNetworkDisabledButtons,
    checkNetworkBeforeAction,
    onNetworkRecovered,
    dismissNetworkToast,
    setBackendAvailability,
} from './network.js';
import { checkCookiesStatus, dismissCookieWarning, updateLoginCard, refreshLoginInfo } from './auth-card.js';
export { updateNetworkBanner, updateNetworkDisabledButtons, checkNetworkBeforeAction, onNetworkRecovered, dismissNetworkToast, setBackendAvailability } from './network.js';
export { checkCookiesStatus, dismissCookieWarning, updateLoginCard, refreshLoginInfo } from './auth-card.js';
import { switchTab, showQRCodeLogin, refreshQRCode, closeQRCodeModal, toggleManualCookie, saveManualCookie, logoutAccount } from './bootstrap.js';
import { loadMoreManualQuery, doManualResolve } from './manual.js';
import { getDownloadLinks, downloadAudioWithQuality, downloadVideoWithQuality, downloadDanmaku, downloadComments } from './media-links.js';
import {
    showAddBloggerModal,
    closeBloggerModal,
    confirmBloggerModal,
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
import { startProgressUpdates, showToast, confirmDialog, closeConfirmDialog } from './download-status.js';
import { addTimePoint, removeTimePoint, browseFFmpegPath, testFFmpeg, saveSettings, resetSettings, loadSettings, testDownload, restartAria2 } from './settings.js';
import { openVideoDrawer, closeVideoDrawer, openVideoDrawerFromManual } from './drawer.js';
import { burnMedia, deleteVideoRecord, refreshVideoInfo, loadBvidLogs, selectQualityPill, startVideoDownload, retryVideoDownload, startVideoDownloadFromManual, toggleAllPages, openVideoPage, downloadCover, startSeasonDownload, toggleAllEpisodes } from './media-actions.js';
import { loadDrawerComments, loadDrawerDanmaku } from './drawer-sidecars.js';
import { openHistoryDirectory } from './directory-actions.js';
import './live.js';

// B 站视频监控助手的功能模块。
// 连接后端API

// --- API 基础配置 ---
const _API_BASE = '';

// --- WebSocket 配置 ---
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
    'close-blogger-modal': () => closeBloggerModal(),
    'confirm-blogger-modal': () => confirmBloggerModal(),
    'close-add-blogger-modal': () => closeAddBloggerModal(),
    'confirm-add-blogger': () => confirmAddBlogger(),
    'close-edit-blogger-modal': () => closeEditBloggerModal(),
    'confirm-edit-blogger': () => confirmEditBlogger(),
    'add-active-window': el => addActiveWindowRow(el.dataset.scope || 'blogger'),
    'active-window-preset': el => addActiveWindowPreset(el.dataset.scope || 'blogger', el.dataset.preset),
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
    'download-danmaku': el => downloadDanmaku(el.dataset.bvid, el.dataset.source, el.dataset.historyId ? Number(el.dataset.historyId) : undefined, el.dataset.page ? Number(el.dataset.page) : undefined),
    'download-comments': el => downloadComments(el.dataset.bvid, el.dataset.source, el.dataset.historyId ? Number(el.dataset.historyId) : undefined),
    'select-blogger': el => selectBlogger(Number(el.dataset.bloggerId)),
    'switch-tab': el => switchTab(el.dataset.tab),
    'cleanup-blogger': el => cleanupBloggerNowByUid(el.dataset.uid),
    'open-video': el => openVideoDrawer(el.dataset.bvid, el.dataset.historyId ? Number(el.dataset.historyId) : undefined),
    'remove-time-point': el => removeTimePoint(Number(el.dataset.hours)),
    'remove-known-blogger': el => removeKnownBlogger(el.dataset.uid),
    'clear-known-bloggers': () => clearKnownBloggers(),
    'save-bloggers': () => saveBloggers(),
    'acknowledge-blogger-change': el => acknowledgeBloggerChange(el.dataset.uid),
    'retry-video': el => retryVideoDownload(el.dataset.bvid),
    'start-video': el => startVideoDownload(el.dataset.bvid),
    'pause-download': el => pauseDownload(Number(el.dataset.taskId)),
    'resume-download': el => resumeDownload(Number(el.dataset.taskId)),
    'delete-video': el => deleteVideoRecord(el.dataset.bvid, el.dataset.historyId ? Number(el.dataset.historyId) : undefined),
    'load-drawer-comments': el => loadDrawerComments(el.dataset.bvid, el.dataset.path, el.dataset.historyId ? Number(el.dataset.historyId) : undefined),
    'load-drawer-danmaku': el => loadDrawerDanmaku(el.dataset.bvid, el.dataset.path, el.dataset.historyId ? Number(el.dataset.historyId) : undefined),
    'refresh-video-info': el => refreshVideoInfo(el.dataset.bvid),
    'download-cover': el => downloadCover(el.dataset.bvid),
    'open-video-page': el => openVideoPage(el.dataset.bvid),
    'open-history-directory': el => openHistoryDirectory(el.dataset.bvid, el.dataset.path, el.dataset.historyId ? Number(el.dataset.historyId) : undefined),
    'load-bvid-logs': el => loadBvidLogs(el.dataset.bvid),
    'burn-media': el => burnMedia(el.dataset.bvid, el.dataset.kind, el, el.dataset.historyId ? Number(el.dataset.historyId) : undefined),
    'select-quality': el => selectQualityPill(el, Number(el.dataset.qn)),
    'start-manual-video': el => startVideoDownloadFromManual(el.dataset.bvid),
    'toggle-all-pages': () => toggleAllPages(),
    'resolve-link': () => doManualResolve(),
    'start-season-download': el => startSeasonDownload(el.dataset.mediaType, el.dataset.seasonTitle),
    'toggle-all-episodes': () => toggleAllEpisodes(),
    'show-add-blogger-modal': () => showAddBloggerModal(),
};

const _declarativeChangeActions = {
    'toggle-active-window-mode': el => toggleActiveWindowMode(el.dataset.scope || 'blogger'),
    'load-known-blogger-config': () => loadKnownBloggerIntoAddForm(),
};

document.addEventListener('click', event => {
    const target = event.target.closest('[data-action]');
    if (!target) return;
    const handler = _declarativeActions[target.dataset.action];
    if (!handler) return;
    if (target.dataset.networkDisabled === 'true') {
        event.preventDefault();
        checkNetworkBeforeAction();
        return;
    }
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

        _state.socket.on('download:disk-recovered', function(data) {
            if (!acceptWebSocketMessage('download:disk-recovered', data)) return;
            closeConfirmDialog('磁盘空间不足');
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
        const callerSignal = options.signal;
        const headers = { ...(options.headers || {}) };
        if (!['GET', 'HEAD', 'OPTIONS'].includes(method) && _state.csrfToken) {
            headers['X-CSRF-Token'] = _state.csrfToken;
        }
        const envelope = await requestEnvelope(url, {
            ...options,
            signal: callerSignal || controller.signal,
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
        if (error.name === 'AbortError') throw error;
        if (error instanceof ApiError) {
            if (error.code === 502 || error.message === '响应格式异常') setBackendAvailability(false);
            throw error;
        }
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
export async function apiPost(url, data, options = {}) {
    return apiRequest(url, {
        ...options,
        method: 'POST',
        body: JSON.stringify(data)
    });
}

export async function apiPut(url, data, options = {}) {
    return apiRequest(url, {
        ...options,
        method: 'PUT',
        body: JSON.stringify(data)
    });
}

export async function apiGet(url, options = {}) {
    return apiRequest(url, {
        ...options,
        method: 'GET'
    });
}

// --- 全局状态 ---
_state.manualDownloadProgress = {};  // 存储手动下载的进度 {bvid: {progress, status, speed, etc}}
_state.serverOffset = 0;
_state.nextCheckTimestamp = 0;
_state.bloggers = [];
_state.bloggerIdCounter = 0;
_state.isTaskRunning = false;
_state.progressUpdateInterval = null;
_state.urlExpiryTimers = {};
_state.videoTitles = {};

// 当前选中的博主ID
_state.selectedBloggerId = null;
_state.selectedDownloadBloggerId = null;

// 每个博主的独立状态
_state.bloggerStates = {};
