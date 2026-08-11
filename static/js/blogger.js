import { _state } from './state.js';
import { escapeHtml } from './utils.js';
import { checkNetworkBeforeAction, apiPost, apiGet } from './core.js';
import { switchTab } from './bootstrap.js';
import { loadHistoryBoard } from './history.js';
import { showToast, confirmDialog } from './download-status.js';

// --- 博主管理（从服务器加载） ---
export async function loadBloggersFromServer() {
    try {
        const result = await apiGet('/api/blogger/list');
        if (result.code === 0) {
            const data = result.data || {};
            _state.serverUtcOffset = data.server_utc_offset || _state.serverUtcOffset || '';
            _state.bloggers = (data.bloggers || []).map((b, index) => ({
                id: b.id,
                element: null,
                uid: b.uid,
                name: b.name,
                face: b.face,
                notice_visible: b.notice_visible === true,
            }));

            // 初始化博主状态（含下载/烧录策略，后端未返回时取默认值）。
            // 注意：保留已加载的日志，避免编辑备注名等操作触发重载后详情面板日志显示为空
            const prevStates = _state.bloggerStates;
            _state.bloggerStates = {};
            (data.bloggers || []).forEach(b => {
                _state.bloggerStates[b.id] = {
                    id: b.id,
                    uid: b.uid,
                    name: b.name,
                    isRunning: b.monitor_enabled ?? b.is_running,
                    runtimeState: b.runtime_state || (b.is_running ? 'scheduled' : 'stopped'),
                    pauseReason: b.pause_reason || null,
                    withinActiveWindow: b.within_active_window !== false,
                    nextActionKind: b.next_action_kind || (b.is_running ? 'check' : null),
                    nextCheckTime: b.next_action_at || b.next_check,
                    logs: (prevStates[b.id] && prevStates[b.id].logs) || [],
                    minInterval: b.min_interval,
                    maxInterval: b.max_interval,
                    download_video: b.download_video !== false,
                    download_danmaku: b.download_danmaku !== false,
                    download_comments: b.download_comments !== false,
                    download_cover: b.download_cover !== false,
                    burn_danmaku: b.burn_danmaku === true,
                    burn_subtitle: b.burn_subtitle === true,
                    series_filter_regex: b.series_filter_regex || '',
                    active_windows: Array.isArray(b.active_windows) ? b.active_windows : []
                };
            });

            renderBloggerSidebar();
        }
    } catch (e) {
        // 忽略取消请求的错误，只显示其他错误
        if (e.name !== 'AbortError') {
            console.error('加载博主列表失败:', e);
            // 不显示错误消息，避免页面加载时的干扰
        }
    }
}

// 配置在增删改时已实时落库；此入口只负责从后端刷新列表。
export async function saveBloggers() {
    try {
        await loadBloggersFromServer();
        showToast(`已刷新 ${_state.bloggers.length} 个博主`, 'success');
    } catch (e) {
        showToast('刷新列表失败，请检查网络', 'error');
    }
}

// 计算博主"下次检查"倒计时的显示文本与样式类（供整列渲染与逐秒 tick 复用）
export function computeNextCheckDisplay(state) {
    if (!state || !state.isRunning) return null;
    if (state.runtimeState === 'waiting_window') {
        const now = Math.floor(Date.now() / 1000);
        const diff = Math.max(0, (state.nextCheckTime || 0) - now);
        if (diff > 0) {
            const hours = Math.floor(diff / 3600);
            const minutes = Math.floor((diff % 3600) / 60);
            const seconds = diff % 60;
            return {
                text: `暂停 · ${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')} 后恢复`,
                cls: 'paused'
            };
        }
        return { text: '等待监测窗口', cls: 'paused' };
    }
    if (state.nextCheckTime && state.nextCheckTime > 0) {
        const now = Math.floor(Date.now() / 1000);
        const diff = state.nextCheckTime - now;
        if (diff > 0) {
            const hours = Math.floor(diff / 3600);
            const minutes = Math.floor((diff % 3600) / 60);
            const seconds = diff % 60;
            const text = `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
            return { text, cls: diff < 60 ? 'warning' : '' };
        } else if (diff > -30) {
            return { text: '检查中...', cls: 'checking' };
        }
        return { text: '等待中', cls: 'waiting' };
    }
    return { text: '初始化...', cls: 'initializing' };
}

// 就地更新倒计时，避免每秒重建侧边栏并丢失滚动或焦点。
export function tickSidebarCountdowns() {
    updateAutoBoardSummary();
    const sidebar = document.getElementById('blogger-sidebar-list');
    if (!sidebar) return;
    const items = sidebar.querySelectorAll('.blogger-list-item[data-blogger-id]');
    if (items.length === 0) return;
    let needFullRender = false;
    items.forEach(item => {
        const id = parseInt(item.getAttribute('data-blogger-id'), 10);
        const state = _state.bloggerStates[id] || {};
        const disp = computeNextCheckDisplay(state);
        const el = item.querySelector('.blogger-next-check');
        if (disp && el) {
            const cls = 'blogger-next-check' + (disp.cls ? ' ' + disp.cls : '');
            if (el.className !== cls) el.className = cls;
            el.innerHTML = `<i class="fa-solid fa-clock"></i> ${disp.text}`;
        } else if ((disp && !el) || (!disp && el)) {
            // 运行状态发生切换（开始 / 停止），需要整列重渲染
            needFullRender = true;
        }
    });
    if (needFullRender) renderBloggerSidebar();
}

// 看板标题右侧概览：与其他页面顶栏一样提供一眼可见的状态摘要。
export function updateAutoBoardSummary() {
    const node = document.getElementById('auto-board-summary');
    if (!node) return;
    const total = _state.bloggers.length;
    if (!total) {
        node.textContent = '暂无监控博主';
        return;
    }
    const running = _state.bloggers.filter(b => (_state.bloggerStates[b.id] || {}).isRunning).length;
    node.textContent = `${total} 位博主 · ${running} 个监控运行中`;
}

export function renderBloggerSidebar() {
    const sidebar = document.getElementById('blogger-sidebar-list');
    if (!sidebar) return;
    updateAutoBoardSummary();

    if (_state.bloggers.length === 0) {
        sidebar.innerHTML = `
            <div class="empty-state" data-js-style="4">
                <i class="fa-solid fa-users-slash"></i>
                <p>暂无监控博主</p>
                <button class="btn btn-primary" data-action="show-add-blogger-modal">
                    <i class="fa-solid fa-plus"></i> 添加博主
                </button>
            </div>
        `;
        return;
    }

    sidebar.innerHTML = _state.bloggers.map(b => {
        const state = _state.bloggerStates[b.id] || {};
        const isRunning = state.isRunning || false;
        const isWaitingWindow = state.runtimeState === 'waiting_window';
        const uid = b.uid || '未设置UID';
        const name = state.name || '';
        const displayName = name ? `${name} (${uid})` : `博主 ${uid}`;
        const isActive = _state.selectedBloggerId === b.id;

        // 计算下次检查时间（与 tickSidebarCountdowns 复用同一逻辑）
        const _disp = computeNextCheckDisplay(state);
        const nextCheckText = _disp ? _disp.text : '';
        const nextCheckClass = _disp ? _disp.cls : '';

        return `
            <div class="blogger-list-item ${isActive ? 'active' : ''}"
                 data-action="select-blogger" data-blogger-id="${b.id}"
                 data-context-blogger-id="${b.id}"
                 tabindex="0"
                 aria-label="${escapeHtml(displayName)}，${isWaitingWindow ? '时段外暂停' : (isRunning ? '监测中' : '已停止')}">
                <div class="blogger-avatar">
                    ${b.face
                        ? `<img src="/api/video/proxy-image?url=${encodeURIComponent(b.face)}" alt="" data-image-error="avatar-fallback" data-fallback-text="${escapeHtml((name || uid).slice(0, 2).toUpperCase())}">`
                        : escapeHtml((name || uid).slice(0, 2).toUpperCase())}
                </div>
                <div class="blogger-info">
                    <div class="blogger-name" title="${escapeHtml(displayName)}">${escapeHtml(name) || '博主 ' + b.id}</div>
                    <div class="blogger-uid">${escapeHtml(uid)}</div>
                    ${nextCheckText ? `<div class="blogger-next-check ${nextCheckClass}"><i class="fa-solid fa-clock"></i> ${nextCheckText}</div>` : ''}
                </div>
                <div class="blogger-status ${isWaitingWindow ? 'paused' : (isRunning ? 'running' : 'stopped')}" title="${isWaitingWindow ? '时段外暂停，将自动恢复' : (isRunning ? '监测中' : '已停止')}"></div>
            </div>
        `;
    }).join('');
}

export async function selectBlogger(id) {
    // 切换前取消订阅前一个博主的日志推送，避免订阅泄漏
    if (_state.selectedBloggerId !== null && _state.selectedBloggerId !== id) {
        const prevBlogger = _state.bloggers.find(b => b.id === _state.selectedBloggerId);
        if (prevBlogger && prevBlogger.uid && _state.socket) {
        _state.socket.emit('blogger:logs:unsubscribe', { uid: prevBlogger.uid });
        }
    }

    _state.selectedBloggerId = id;
    renderBloggerSidebar();
    showBloggerDetail();
    await loadBloggerLogs(id);  // 从服务器加载日志
    updateDetailPanel();

    // 订阅该博主的日志更新
    const blogger = _state.bloggers.find(b => b.id === id);
    if (blogger && blogger.uid && _state.socket) {
        _state.socket.emit('blogger:logs:subscribe', { uid: blogger.uid });
    }

    // 启动日志自动刷新
    startLogRefresh();
}

export function showBloggerEmptyState() {
    document.getElementById('blogger-empty-state').hidden = false;
    document.getElementById('blogger-detail-content').hidden = true;
}

export function showBloggerDetail() {
    document.getElementById('blogger-empty-state').hidden = true;
    document.getElementById('blogger-detail-content').hidden = false;
}

export function updateDetailPanel() {
    if (_state.selectedBloggerId === null) return;

    const state = _state.bloggerStates[_state.selectedBloggerId];
    if (!state) return;

    const blogger = _state.bloggers.find(b => b.id === _state.selectedBloggerId);
    const uid = blogger ? blogger.uid : '未设置';
    const name = state.name || '';

    document.getElementById('detail-blogger-name').textContent = name ? `${name} (${uid})` : (uid ? `博主 UID: ${uid}` : '未设置UID');

    const startBtn = document.getElementById('detail-start-btn');
    const stopBtn = document.getElementById('detail-stop-btn');

    if (state.isRunning) {
        startBtn.hidden = true;
        stopBtn.hidden = false;
    } else {
        startBtn.hidden = false;
        stopBtn.hidden = true;
    }

    // 更新运行状态显示
    const runningStatusEl = document.getElementById('detail-running-status');
    if (runningStatusEl) {
        if (state.runtimeState === 'waiting_window') {
            runningStatusEl.textContent = '时段外暂停';
            runningStatusEl.className = 'status-value paused';
        } else if (state.runtimeState === 'checking') {
            runningStatusEl.textContent = '正在检查';
            runningStatusEl.className = 'status-value checking';
        } else if (state.isRunning) {
            runningStatusEl.textContent = '监测中';
            runningStatusEl.className = 'status-value running';
        } else {
            runningStatusEl.textContent = '已停止';
            runningStatusEl.className = 'status-value stopped';
        }
    }

    // 更新倒计时显示
    updateBloggerCountdown(_state.selectedBloggerId);

    // 下载/烧录策略摘要
    const strategyEl = document.getElementById('detail-blogger-strategy');
    if (strategyEl) {
        const s = state || {};
        const downloads = [];
        if (s.download_video !== false) downloads.push('视频');
        if (s.download_danmaku !== false) downloads.push('弹幕');
        if (s.download_comments !== false) downloads.push('评论');
        if (s.download_cover !== false) downloads.push('封面');
        const burns = [];
        if (s.burn_danmaku === true) burns.push('弹幕');
        if (s.burn_subtitle === true) burns.push('字幕');
        const downloadText = downloads.length > 0 ? downloads.join('·') : '不下载视频';
        const burnText = burns.length > 0 ? `自动烧录 ${burns.join('·')}` : '不自动烧录';
        const regexText = s.series_filter_regex ? ` / 合集正则：${escapeHtml(s.series_filter_regex)}` : '';
        const windowsText = Array.isArray(s.active_windows) && s.active_windows.length > 0
            ? ` / 检查时段：${escapeHtml(s.active_windows.join('、'))}${_state.serverUtcOffset ? `（UTC${escapeHtml(_state.serverUtcOffset)}）` : ''}`
            : '';
        strategyEl.innerHTML = `<i class="fa-solid fa-sliders-h"></i> ${escapeHtml(downloadText)} / ${escapeHtml(burnText)}${regexText}${windowsText}`;
    }

    renderBloggerLogs(_state.selectedBloggerId);
}

// 更新博主倒计时
export function updateBloggerCountdown(bloggerId) {
    const state = _state.bloggerStates[bloggerId];
    if (!state) return;

    const countdownEl = document.getElementById('detail-countdown');
    if (!countdownEl) return;

    const countdownLabel = document.getElementById('detail-countdown-label');
    if (countdownLabel) {
        countdownLabel.innerHTML = state.runtimeState === 'waiting_window'
            ? '<i class="fa-solid fa-clock"></i> 恢复监测'
            : '<i class="fa-solid fa-clock"></i> 下次检查';
    }

    if (state.runtimeState === 'waiting_window') {
        const now = Math.floor(Date.now() / 1000);
        const diff = Math.max(0, (state.nextCheckTime || 0) - now);
        if (diff > 0) {
            const hours = Math.floor(diff / 3600);
            const minutes = Math.floor((diff % 3600) / 60);
            const seconds = diff % 60;
            countdownEl.textContent = `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
        } else {
            countdownEl.textContent = '等待恢复';
        }
        countdownEl.className = 'status-value paused';
    } else if (state.isRunning) {
        if (state.nextCheckTime && state.nextCheckTime > 0) {
            const now = Math.floor(Date.now() / 1000);
            const diff = state.nextCheckTime - now;

            if (diff > 0) {
                // 正常倒计时
                const hours = Math.floor(diff / 3600);
                const minutes = Math.floor((diff % 3600) / 60);
                const seconds = diff % 60;
                countdownEl.textContent = `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
                countdownEl.className = 'status-value running';
            } else if (diff > -30) {
                // 刚刚过期（30秒内），显示检查中
                countdownEl.textContent = '检查中...';
                countdownEl.className = 'status-value checking';
            } else {
                // 已经过期很久，可能出错了
                countdownEl.textContent = '等待中';
                countdownEl.className = 'status-value waiting';
            }
        } else {
            // 正在运行但没有下次检查时间，可能是首次启动
            countdownEl.textContent = '初始化...';
            countdownEl.className = 'status-value initializing';
        }
    } else {
        countdownEl.textContent = '--:--:--';
        countdownEl.className = 'status-value stopped';
    }
}

export async function loadBloggerLogs(bloggerId) {
    // 从服务器加载博主日志
    const blogger = _state.bloggers.find(b => b.id === bloggerId);
    if (!blogger || !blogger.uid) return;

    try {
        const result = await apiGet(`/api/logs/blogger?uid=${encodeURIComponent(blogger.uid)}&limit=100`);
        const data = result.data || {};
        if (result.code === 0 && data.logs) {
            if (!_state.bloggerStates[bloggerId]) {
                _state.bloggerStates[bloggerId] = {};
            }
            // 直接使用服务器返回的日志格式
            _state.bloggerStates[bloggerId].logs = data.logs;
        }
    } catch (e) {
        console.error('加载日志失败:', e);
    }
}

export function renderBloggerLogs(bloggerId) {
    const logsContainer = document.getElementById('detail-blogger-logs');
    if (!logsContainer) return;

    const state = _state.bloggerStates[bloggerId];
    if (!state || !state.logs || state.logs.length === 0) {
        logsContainer.innerHTML = `
            <div class="empty-state" data-js-style="5">
                <i class="fa-solid fa-info-circle"></i>
                <p>暂无日志</p>
            </div>
        `;
        return;
    }

    // 按时间排序（最新的在最后）
    // 优先使用 timestamp 字段（完整时间戳），避免跨天排序问题
    const sortedLogs = [...state.logs].sort((a, b) => {
        const timestampA = a.timestamp || 0;
        const timestampB = b.timestamp || 0;

        // 使用时间戳排序
        if (timestampA && timestampB) {
            return timestampA - timestampB;
        }

        // 降级：使用 time 字段
        const timeA = a.time || '';
        const timeB = b.time || '';
        return timeA.localeCompare(timeB);
    });

    logsContainer.innerHTML = sortedLogs.map(l => {
        const level = l.level || 'info';
        const time = l.time || '--:--:--';
        const msg = l.msg || l.message || '';
        return `<div class="log-entry log-level-${escapeHtml(level)}"><span class="log-time">${escapeHtml(time)}</span><span>${escapeHtml(msg)}</span></div>`;
    }).join('');
    logsContainer.scrollTop = logsContainer.scrollHeight;
}

// 日志自动刷新定时器
_state.logRefreshInterval = null;

export function startLogRefresh() {
    // 如果 WebSocket 已连接，不需要 HTTP 轮询日志
    if (_state.wsConnected) return;
    // 每2秒刷新一次当前选中博主的日志
    if (_state.logRefreshInterval) clearInterval(_state.logRefreshInterval);
    _state.logRefreshInterval = setInterval(async () => {
        if (_state.logRefreshInFlight) return;
        _state.logRefreshInFlight = true;
        if (_state.selectedBloggerId !== null) {
            try {
                await loadBloggerLogs(_state.selectedBloggerId);
                renderBloggerLogs(_state.selectedBloggerId);
            } finally {
                _state.logRefreshInFlight = false;
            }
        } else {
            _state.logRefreshInFlight = false;
        }
    }, 2000);
}

export async function startSelectedBlogger() {
    if (_state.selectedBloggerId === null) return;
    if (!checkNetworkBeforeAction()) return;
    
    const state = _state.bloggerStates[_state.selectedBloggerId];
    const blogger = _state.bloggers.find(b => b.id === _state.selectedBloggerId);
    
    if (!blogger || !blogger.uid) {
        showToast('请先设置博主UID', 'error');
        return;
    }
    
    if (!_state.loginValid) {
        showToast('请先在系统设置中登录 B 站账号', 'error');
        switchTab('settings');
        return;
    }
    
    try {
        const result = await apiPost('/api/task/start', {
            uid: blogger.uid
        });
        
        if (result.code === 0) {
            const data = result.data || {};
            const schedule = data.schedule || {};
            state.isRunning = schedule.monitor_enabled ?? true;
            state.runtimeState = schedule.runtime_state || 'scheduled';
            state.pauseReason = schedule.pause_reason || null;
            state.withinActiveWindow = schedule.within_active_window !== false;
            state.nextActionKind = schedule.next_action_kind || 'check';
            state.nextCheckTime = schedule.next_action_at || data.next_check;
            showToast(
                state.runtimeState === 'waiting_window'
                    ? `博主 ${blogger.uid} 监控已启用，将在下个时段自动恢复`
                    : `博主 ${blogger.uid} 监控已启动`,
                'success'
            );
            updateDetailPanel();
            renderBloggerSidebar();
        } else {
            showToast(result.message || '启动失败', 'error');
        }
    } catch (e) {
        showToast('启动监控失败', 'error');
    }
}

export async function stopSelectedBlogger() {
    if (_state.selectedBloggerId === null) return;

    const state = _state.bloggerStates[_state.selectedBloggerId];
    const blogger = _state.bloggers.find(b => b.id === _state.selectedBloggerId);

    if (!blogger) return;

    try {
        const result = await apiPost('/api/task/stop', {
            uid: blogger.uid
        });

        if (result.code === 0) {
            state.isRunning = false;
            state.runtimeState = 'stopped';
            state.pauseReason = null;
            state.nextActionKind = null;
            state.nextCheckTime = 0;
            showToast('监控已停止', 'info');
            updateDetailPanel();
            renderBloggerSidebar();
        } else {
            showToast(result.message || '停止失败', 'error');
        }
    } catch (e) {
        showToast('停止监控失败', 'error');
    }
}

// 按全局保留策略立即整理指定博主。
export async function cleanupBloggerNowByUid(uid) {
    if (!uid) return;
    if (!(await confirmDialog(`确认立即整理博主 ${uid}？\n\n将按保留数删除多余的旧视频（文件 + 记录）。`, { title: '立即整理', okText: '开始整理' }))) return;
    try {
        const result = await apiPost('/api/blogger/cleanup-now', { uid });
        if (result.code === 0) {
            showToast('整理完成', 'success');
            await loadHistoryBoard(_state.currentBoardTab);
        } else {
            showToast(result.message || '整理失败', 'error');
        }
    } catch (e) {
        showToast('整理失败', 'error');
    }
}

// --- 状态轮询 ---
_state.countdownInterval = null;
_state.statusPollingInterval = null;

export function startStatusPolling() {
    // 启动倒计时更新
    startCountdownUpdates();

    // 定期从服务器获取最新状态
    if (_state.statusPollingInterval) clearInterval(_state.statusPollingInterval);
    _state.statusPollingInterval = setInterval(async () => {
        if (_state.statusPollingInFlight) return;
        // 页面不可见时跳过状态轮询，节省资源
        if (document.hidden) return;
        // 侧边栏不可见时无需请求倒计时状态。
        const autoTab = document.getElementById('tab-auto');
        if (!autoTab || !autoTab.classList.contains('active')) return;
        _state.statusPollingInFlight = true;
        try {
            // 获取各博主的下次检查时间
            const nextCheckResult = await apiGet('/api/task/next-check');
            const data = nextCheckResult.data || {};
            if (nextCheckResult.code === 0 && data.bloggers) {
                let stateChanged = false;
                for (const uid in data.bloggers) {
                    const blogger = _state.bloggers.find(b => b.uid === uid);
                    if (blogger && _state.bloggerStates[blogger.id]) {
                        const info = data.bloggers[uid];
                        // 检查状态是否有变化
                        if (_state.bloggerStates[blogger.id].isRunning !== (info.monitor_enabled ?? info.is_running) ||
                            _state.bloggerStates[blogger.id].runtimeState !== info.runtime_state ||
                            _state.bloggerStates[blogger.id].nextCheckTime !== (info.next_action_at || info.next_check)) {
                            stateChanged = true;
                        }
                        _state.bloggerStates[blogger.id].isRunning = info.monitor_enabled ?? info.is_running;
                        _state.bloggerStates[blogger.id].runtimeState = info.runtime_state || (info.is_running ? 'scheduled' : 'stopped');
                        _state.bloggerStates[blogger.id].pauseReason = info.pause_reason || null;
                        _state.bloggerStates[blogger.id].withinActiveWindow = info.within_active_window !== false;
                        _state.bloggerStates[blogger.id].nextActionKind = info.next_action_kind || null;
                        _state.bloggerStates[blogger.id].nextCheckTime = info.next_action_at || info.next_check;
                    }
                }
                // 只有当状态变化或者是第一次加载时才更新显示
                if (stateChanged || !_state.initialStateLoaded) {
                    _state.initialStateLoaded = true;
                    renderBloggerSidebar();
                    if (_state.selectedBloggerId !== null) {
                        updateDetailPanel();
                    }
                }
            }
        } catch (e) {
            // 静默处理网络错误
        } finally {
            _state.statusPollingInFlight = false;
        }
    }, 2000); // 每2秒更新一次
}

// 启动倒计时更新（每秒更新一次）
export function startCountdownUpdates() {
    if (_state.countdownInterval) {
        clearInterval(_state.countdownInterval);
    }
    
    _state.countdownInterval = setInterval(() => {
        // 页面不可见时跳过，节省 CPU / 电量
        if (document.hidden) return;
        // 侧边栏不可见时无需刷新倒计时。
        const autoTab = document.getElementById('tab-auto');
        if (!autoTab || !autoTab.classList.contains('active')) return;
        // 仅就地更新侧边栏倒计时文本，不重建整列
        tickSidebarCountdowns();
        
        // 更新详情面板中的倒计时
        if (_state.selectedBloggerId !== null) {
            updateBloggerCountdown(_state.selectedBloggerId);
        }
    }, 1000);
}
