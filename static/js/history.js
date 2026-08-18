import { _state, _NETWORK_ERR_MSG } from './state.js';
import { clampPercent, escapeHtml, formatFileSize, formatSpeed } from './utils.js';
import { setTone, checkNetworkBeforeAction, apiPost, apiGet } from './core.js';
import { updateDownloadLists } from './download-queue.js';
import { showToast } from './download-status.js';
import { openVideoDrawer } from './drawer.js';

// --- 看板（下载管理页） ---
// 当前看板子 tab：downloading / completed / failed
_state.currentBoardTab = 'completed';
// 看板视频缓存：{ bvid: video }，供抽屉快速查找
_state.currentBoardVideos = _state.currentBoardVideos || {};
// 手动刷新防抖
_state.lastManualRefreshTs = 0;
// 上次拉取的 server_time（秒）
_state.lastBoardServerTime = 0;
// 当前打开的抽屉对应的 bvid（用于双向同步）
_state.currentDrawerBvid = null;
_state.historyBoardRequestId = 0;
_state.historyBoardController = null;
_state.historyBoardInFlight = false;

// 切换看板子 tab。
export function switchBoardTab(tab) {
    _state.currentBoardTab = tab;
    document.querySelectorAll('.board-sub-tab').forEach(el => {
        el.classList.toggle('active', el.dataset.boardTab === tab);
    });
    loadHistoryBoard(tab);
}

// 加载看板数据。
export async function loadHistoryBoard(tab, { append = false } = {}) {
    const board = document.getElementById('history-board');
    if (!board) return;
    if (append && _state.historyBoardInFlight) return;
    if (!append && !_state.historyPagination) {
        board.innerHTML = '<div class="history-skeleton" aria-label="正在加载历史列表"><span class="skeleton skeleton-line"></span><span class="skeleton skeleton-line"></span><span class="skeleton skeleton-line"></span></div>';
    }
    const requestId = ++_state.historyBoardRequestId;
    _state.historyBoardController?.abort();
    const controller = new AbortController();
    _state.historyBoardController = controller;
    _state.historyBoardInFlight = true;
    try {
        const previous = _state.historyPagination?.tab === tab
            ? _state.historyPagination
            : { tab, page: 0, total: 0, groups: [] };
        const page = append ? previous.page + 1 : 1;
        const result = await apiGet(
            `/api/history/list?tab=${encodeURIComponent(tab)}&page=${page}&page_size=50`,
            { signal: controller.signal },
        );
        if (requestId !== _state.historyBoardRequestId || _state.currentBoardTab !== tab) return;
        if (result.code !== 0) {
            // 网络错误：统一用右上角 toast + 顶栏横幅，不在看板内联渲染，保留原有内容
            if (result.offline) { showToast(_NETWORK_ERR_MSG, 'error'); return; }
            board.innerHTML = `<div class="empty-state-grid"><i class="fa-solid fa-exclamation-circle status-error"></i><p>${escapeHtml(result.message || '加载看板失败')}</p></div>`;
            return;
        }
        const data = result.data || {};
        // 缓存 server_time，并更新“上次拉取”时间。
        _state.lastBoardServerTime = data.server_time || 0;
        updateLastPullTimeDisplay();

        // 计数：优先用后端返回的全局 counts（跨所有博主，随任意 tab 都是全量）；
        // 兜底：老后端无全局 counts 时退回累加各 group（可能因当前 tab 无博主而偏小）。
        const pageGroups = data.items || [];
        const groupMap = new Map();
        for (const group of append ? previous.groups : []) {
            groupMap.set(group.uid, { ...group, videos: [...(group.videos || [])] });
        }
        for (const group of pageGroups) {
            const existing = groupMap.get(group.uid);
            if (existing) {
                existing.videos.push(...(group.videos || []));
                existing.counts = group.counts || existing.counts;
            } else {
                groupMap.set(group.uid, { ...group, videos: [...(group.videos || [])] });
            }
        }
        const groups = [...groupMap.values()];
        _state.historyPagination = {
            tab,
            page,
            total: Number(data.total) || 0,
            groups,
        };
        let counts = data.counts;
        if (!counts) {
            counts = { downloading: 0, completed: 0, failed: 0, removed: 0, pay_blocked: 0 };
            groups.forEach(g => {
                const c = g.counts || {};
                counts.downloading += c.downloading || 0;
                counts.completed += c.completed || 0;
                counts.failed += c.failed || 0;
                counts.removed += c.removed || 0;
                counts.pay_blocked += c.pay_blocked || 0;
            });
        }
        // 更新 tab count：下载中 = downloading；已下载 = completed + removed + pay_blocked；下载失败 = failed。
        updateElement('board-count-downloading', counts.downloading || 0);
        updateElement('board-count-completed', (counts.completed || 0) + (counts.removed || 0) + (counts.pay_blocked || 0));
        updateElement('board-count-failed', counts.failed || 0);

        // 缓存视频信息供抽屉用
        _state.currentBoardVideos = {};
        groups.forEach(g => {
            (g.videos || []).forEach(v => {
                if (v.bvid) {
                    const cached = { ...v, blogger: { uid: g.uid, name: g.name, face: g.face } };
                    _state.currentBoardVideos[v.bvid] = cached;
                    if (v.history_id != null) _state.currentBoardVideos[`${v.bvid}:${v.history_id}`] = cached;
                }
            });
        });

        if (append) {
            appendHistoryBoardPage(pageGroups, tab);
        } else {
            renderHistoryBoard(groups, tab);
        }
        const loaded = groups.reduce((count, group) => count + (group.videos || []).length, 0);
        if (loaded < _state.historyPagination.total) {
            const loadMore = document.createElement('button');
            loadMore.className = 'btn btn-secondary history-load-more';
            loadMore.dataset.action = 'load-more-history';
            loadMore.dataset.tab = tab;
            loadMore.textContent = `加载更多（${loaded}/${_state.historyPagination.total}）`;
            board.appendChild(loadMore);
        }
    } catch (e) {
        if (e?.name === 'AbortError') return;
        console.error('加载看板失败:', e);
        // 离线时 apiRequest 已展示统一的持续提示，这里不再叠加其他弹窗。
        if (!e.offline) showToast('加载看板失败', 'error');
    } finally {
        if (requestId === _state.historyBoardRequestId) {
            _state.historyBoardInFlight = false;
            _state.historyBoardController = null;
        }
    }
}

// 渲染看板（按博主分组）。
export function renderHistoryBoard(groups, tab) {
    const board = document.getElementById('history-board');
    if (!board) return;
    _state.boardCardIndex?.clear();

    if (!groups || groups.length === 0) {
        const hints = {
            downloading: '当前没有下载中的视频',
            completed: '还没有已下载的视频，去博主搜索或手动查询添加吧',
            failed: '没有下载失败的视频',
        };
        board.innerHTML = `
            <div class="empty-state-grid">
                <i class="fa-solid fa-inbox"></i>
                <p>${hints[tab] || '暂无数据'}</p>
                <p class="empty-hint"><a href="#" data-action="switch-tab" data-tab="search">去博主搜索</a> 或 <a href="#" data-action="switch-tab" data-tab="manual">手动查询</a></p>
            </div>
        `;
        return;
    }

    // 按 uid 排序，保证稳定
    const sorted = [...groups].sort((a, b) => (a.uid || '').localeCompare(b.uid || ''));
    board.innerHTML = sorted.map(g => renderBloggerSection(g, tab)).join('');
}

function appendHistoryBoardPage(groups, tab) {
    const board = document.getElementById('history-board');
    if (!board) return;

    board.querySelector('.history-load-more')?.remove();
    for (const group of groups || []) {
        const uid = String(group.uid || '');
        const existing = [...board.querySelectorAll('.blogger-section')]
            .find(section => section.dataset.uid === uid);
        if (existing) {
            const videos = existing.querySelector('.blogger-section-videos');
            videos?.insertAdjacentHTML(
                'beforeend',
                (group.videos || []).map(video => renderBoardVideoCard(video)).join(''),
            );
            continue;
        }

        const sections = [...board.querySelectorAll('.blogger-section')];
        const next = sections.find(section => (section.dataset.uid || '').localeCompare(uid) > 0);
        const html = renderBloggerSection(group, tab);
        if (next) {
            next.insertAdjacentHTML('beforebegin', html);
        } else {
            board.insertAdjacentHTML('beforeend', html);
        }
    }
}

// 渲染单个博主分组。
export function renderBloggerSection(g, tab) {
    const uid = escapeHtml(g.uid || '');
    const name = escapeHtml(g.name || g.uid || '未知博主');
    const face = g.face || '';
    const faceUrl = face ? `/api/video/proxy-image?url=${encodeURIComponent(face)}` : '';
    const avatarHtml = faceUrl
        ? `<img src="${faceUrl}" class="blogger-section-avatar" alt="" data-image-error="show-next"><div class="blogger-section-avatar blogger-section-avatar-fallback" hidden>${escapeHtml((g.name || g.uid || '?').slice(0, 1))}</div>`
        : `<div class="blogger-section-avatar blogger-section-avatar-fallback">${escapeHtml((g.name || g.uid || '?').slice(0, 1))}</div>`;

    const videos = g.videos || [];
    const videoCards = videos.map(v => renderBoardVideoCard(v)).join('');

    return `
        <div class="blogger-section" data-uid="${uid}">
            <div class="blogger-section-header">
                <div class="blogger-section-info">
                    ${avatarHtml}
                    <div class="blogger-section-text">
                        <div class="blogger-section-name">${name}</div>
                        <div class="blogger-section-uid">UID: ${uid}</div>
                    </div>
                </div>
                <button class="btn btn-sm btn-ghost" data-action="cleanup-blogger" data-uid="${uid}" title="立即按保留数清理该博主">
                    <i class="fa-solid fa-broom"></i> 立即整理
                </button>
            </div>
            <div class="blogger-section-videos">
                ${videoCards}
            </div>
        </div>
    `;
}

// 渲染单个视频卡片。
export function renderBoardVideoCard(v) {
    const bvid = escapeHtml(v.bvid || '');
    const title = escapeHtml(v.title || '未知标题');
    const state = v.state || 'completed';
    const stateDot = stateDotClass(state, v);
    const coverQuery = v.history_id == null ? '' : `?history_id=${encodeURIComponent(v.history_id)}`;
    const coverUrl = `/api/cover/${bvid}${coverQuery}`;
    const duration = v.duration ? formatDuration(v.duration) : '';
    const pubDate = v.pub_date || (v.pub_timestamp ? formatTimestamp(v.pub_timestamp) : '');
    const view = formatViewCount(v.view);

    // 进度条（仅活跃状态显示：下载中 / 等待 / 已暂停）
    const task = v.task || {};
    const isActive = (task.status === 'downloading' || task.status === 'pending' || task.status === 'paused') && (state === 'pending' || state === 'downloading' || state === 'paused' || _state.currentBoardTab === 'downloading');
    const progress = clampPercent(task.progress_percent);
    const speed = task.speed ? formatSpeed(task.speed) : '';
    const downloadedSize = task.downloaded_size ? formatFileSize(task.downloaded_size) : '';
    const totalSize = task.total_size ? formatFileSize(task.total_size) : '';
    const isPaused = task.status === 'paused';

    // sidecar 状态
    const sidecar = v.sidecar || {};
    const sidecarHtml = renderSidecarIcons(sidecar);

    // 重投提示
    const reuploadBadge = v.reupload_of ? `<span class="reupload-badge" title="可能是 ${escapeHtml(v.reupload_of)} 的重传">重投?</span>` : '';

    // 路径展示由后端统一决定；相对路径单独作为打开目录的安全标识。
    const filePath = v.file_path || v.relative_path ? `
        <div class="board-card-path" title="${escapeHtml(v.file_path || '路径已隐藏')}">
            <i class="fa-solid fa-file-video"></i>
            <span>${escapeHtml(v.file_path || '路径已隐藏')}</span>
            ${v.file_path ? `<button class="btn btn-sm btn-ghost" data-copy-path="${escapeHtml(v.file_path)}" title="复制路径"><i class="fa-solid fa-copy"></i></button>` : ''}
            ${v.can_open_directory && v.relative_path ? `<button class="btn btn-sm btn-ghost" data-action="open-history-directory" data-bvid="${bvid}" data-history-id="${escapeHtml(String(v.history_id ?? ''))}" data-path="${escapeHtml(v.relative_path)}" title="打开文件所在目录"><i class="fa-solid fa-folder-open"></i></button>` : ''}
        </div>
    ` : '';

    const progressLabel = isPaused ? `已暂停 ${progress}%` : `${progress}%`;
    const progressHtml = isActive ? `
        <div class="board-card-progress">
            <progress class="board-card-progress-bar" max="100" value="${progress}"></progress>
        </div>
        <div class="board-card-progress-text">
            <span>${progressLabel}</span>
            ${!isPaused && speed ? `<span class="board-card-speed">${speed}</span>` : ''}
            ${downloadedSize && totalSize ? `<span class="board-card-size">${downloadedSize} / ${totalSize}</span>` : ''}
        </div>
    ` : '';

        // 暂停 / 恢复按钮：仅活跃任务且后端返回 task_id 时显示。
    // 嵌在卡片内，依靠事件委托（closest）优先匹配按钮自身的 data-action，不会触发卡片 open-video
    const taskId = task.task_id || '';
    const pauseResumeHtml = taskId && (task.status === 'downloading' || task.status === 'pending' || task.status === 'paused') ? `
        <button class="board-card-action-btn" data-action="${isPaused ? 'resume-download' : 'pause-download'}" data-task-id="${escapeHtml(String(taskId))}" title="${isPaused ? '恢复下载' : '暂停下载'}">
            <i class="fa-solid ${isPaused ? 'fa-play' : 'fa-pause'}"></i>
        </button>
    ` : '';

    // 优先级 −/＋ 控件：与 pause/resume 同区展示，调整后与 ctl dl priority 共享同一后端。
    const taskPriority = Number(task.priority) || 100;
    const priorityHtml = taskId && (task.status === 'downloading' || task.status === 'pending' || task.status === 'paused') ? `
        <span class="board-card-priority" title="下载优先级（1-300，越大越先下载）">
            <button class="board-card-action-btn" data-action="priority-down" data-bvid="${bvid}" data-priority="${taskPriority}" title="降低优先级">−</button>
            <span class="board-card-priority-value">${taskPriority}</span>
            <button class="board-card-action-btn" data-action="priority-up" data-bvid="${bvid}" data-priority="${taskPriority}" title="提高优先级">+</button>
        </span>
    ` : '';

    return `
        <div class="board-video-card state-${stateDot}" data-action="open-video" data-bvid="${bvid}" data-history-id="${escapeHtml(String(v.history_id ?? ''))}">
            <div class="board-card-state-dot" title="${escapeHtml(stateLabel(state, v))}"></div>
            <div class="board-card-thumb">
                <img src="${coverUrl}" alt="" loading="lazy" data-image-error="thumb-fallback">
                ${duration ? `<span class="board-card-duration">${duration}</span>` : ''}
                ${reuploadBadge}
            </div>
            <div class="board-card-body">
                <div class="board-card-title" title="${title}">${title}</div>
                <div class="board-card-meta">
                    <span title="发布时间"><i class="fa-solid fa-calendar-alt"></i> ${pubDate || '--'}</span>
                    <span title="播放量"><i class="fa-solid fa-play"></i> ${view}</span>
                </div>
                ${progressHtml}
                <div class="board-card-sidecar">${sidecarHtml}</div>
                ${filePath}
                ${pauseResumeHtml}
                ${priorityHtml}
            </div>
        </div>
    `;
}

// sidecar 图标：视频、弹幕、字幕。
export function renderSidecarIcons(sidecar) {
    const items = [
        { key: 'video', label: '视频', icon: 'fa-film' },
        { key: 'danmaku', label: '弹幕', icon: 'fa-comment-dots' },
        { key: 'comments', label: '评论', icon: 'fa-comments' },
        { key: 'subtitle', label: '字幕', icon: 'fa-closed-captioning' },
    ];
    return items.map(it => {
        const ok = sidecar[it.key];
        return `<span class="sidecar-icon ${ok ? 'ok' : 'missing'}" title="${it.label}: ${ok ? '已下载' : '未下载'}">
            <i class="fa-solid ${it.icon}"></i>${ok ? '✓' : '—'}
        </span>`;
    }).join('');
}

// 状态点 class（左上角小点颜色）。
// 绿色表示完成，蓝色表示下载中，黄色表示已暂停或可下载充电视频，
// 红色表示下架、重投或失败，灰色表示不可下载充电视频或数据陈旧。
export function stateDotClass(state, v) {
    if (v && v.reupload_of) return 'removed';
    const payNote = (v && v.pay_note) || '';
    switch (state) {
        case 'completed':
        case 'merged':
            return 'completed';
        case 'pending':
            return 'downloading';
        case 'downloading':
            return 'downloading';
        case 'paused':
            return 'paused';
        case 'removed':
            return 'removed';
        case 'pay_blocked':
            if (payNote.endsWith('_paid')) return 'pay_blocked';
            return 'stale';
        case 'failed':
        case 'merge_failed':
            return 'removed';
        case 'tampered':
            return 'removed';
        default:
            return 'completed';
    }
}

// 状态文本。
export function stateLabel(state, v) {
    if (v && v.reupload_of) return `疑似重传（${v.reupload_of}）`;
    const payNote = (v && v.pay_note) || '';
    const map = {
        completed: '已下载',
        merged: '已合并',
        pending: '待下载',
        downloading: '下载中',
        paused: '已暂停',
        failed: '下载失败',
        merge_failed: '合并失败',
        removed: '已下架',
        pay_blocked: payNote.endsWith('_paid') ? '充电专属（可下载）' : '充电专属（不可下载）',
        tampered: 'MD5 不一致',
    };
    return map[state] || state;
}

// 格式化时长（秒 → mm:ss 或 hh:mm:ss）。
export function formatDuration(sec) {
    sec = Number(sec) || 0;
    if (sec <= 0) return '';
    const h = Math.floor(sec / 3600);
    const m = Math.floor((sec % 3600) / 60);
    const s = sec % 60;
    if (h > 0) return `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
    return `${m}:${String(s).padStart(2, '0')}`;
}

// 格式化时间戳为 YYYY-MM-DD HH:MM。
export function formatTimestamp(ts) {
    ts = Number(ts) || 0;
    if (ts <= 0) return '';
    const d = new Date(ts * 1000);
    const pad = n => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

// 格式化播放量。
export function formatViewCount(view) {
    view = Number(view) || 0;
    if (view >= 100000000) return (view / 100000000).toFixed(1) + '亿';
    if (view >= 10000) return (view / 10000).toFixed(1) + '万';
    return view.toString();
}

// 更新“上次拉取”时间显示。
export function updateLastPullTimeDisplay() {
    const el = document.getElementById('last-pull-time');
    if (!el) return;
    if (!_state.lastBoardServerTime) {
        el.textContent = '--';
        return;
    }
    const d = new Date(_state.lastBoardServerTime * 1000);
    const pad = n => String(n).padStart(2, '0');
    el.textContent = `上次拉取：${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

// 手动刷新看板（5s 防抖）。
export async function manualRefreshBoard() {
    if (!checkNetworkBeforeAction()) return;
    const now = Date.now();
    if (now - _state.lastManualRefreshTs < 5000) {
        showToast('刷新太频繁，请稍候', 'warning');
        return;
    }
    _state.lastManualRefreshTs = now;
    const btn = document.getElementById('board-refresh-btn');
    if (btn) {
        const icon = btn.querySelector('i');
        if (icon) icon.classList.add('fa-spin');
    }
    try {
        // 触发后端 L1 worker
        await apiPost('/api/refresh?kind=board', {});
        // 重新拉取看板
        await loadHistoryBoard(_state.currentBoardTab);
        // 若抽屉打开，同步刷新抽屉
        if (_state.currentDrawerBvid) {
            await openVideoDrawer(_state.currentDrawerBvid, _state.currentDrawerHistoryId);
        }
    } catch (e) {
        // 手动操作失败必须给出可见反馈。
        showToast('刷新失败：' + (e && e.message ? e.message : e), 'error');
    } finally {
        if (btn) {
            const icon = btn.querySelector('i');
            if (icon) icon.classList.remove('fa-spin');
        }
    }
}

// 安全更新 element 文本。
export function updateElement(id, value) {
    const el = document.getElementById(id);
    if (el) el.textContent = value;
}

export async function retryDownload(bvid, taskType = 'video') {
    try {
        const result = await apiPost('/api/download/retry', { bvid, type: taskType });
        if (result.code === 0) {
            showToast('开始重新下载...', 'info');
            setTimeout(updateDownloadLists, 1000);
        } else {
            showToast(result.message || '重试失败', 'error');
        }
    } catch (e) {
        showToast('重试下载失败', 'error');
    }
}

// 实时更新下载管理页面中的进度条以及手动下载按钮
export function updateDownloadProgressInList(bvid, data) {
    const taskType = data.type || 'video';
    const stateKey = `${bvid}_${taskType}`;
    const oldStatus = _state.manualDownloadProgress[stateKey]?.status;

    // 1. 更新手动下载的状态跟踪
    if (_state.manualDownloadProgress[stateKey]) {
        _state.manualDownloadProgress[stateKey] = {
            ..._state.manualDownloadProgress[stateKey],
            ...data
        };
        
        // 更新手动查询页面的按钮状态
        const escapedBvid = CSS.escape(bvid);
        const downloadBtn = document.querySelector(`[data-role="manual-video-download"][data-bvid="${escapedBvid}"]`);
        const audioBtn = document.querySelector(`[data-role="manual-audio-download"][data-bvid="${escapedBvid}"]`);
        const status = data.status;
        
        const updateBtn = (btn) => {
            if (!btn) return;
            if (status === 'downloading') {
                btn.disabled = true;
                const step = data.step || 1;
                const totalSteps = data.total_steps || 2;
                const pct = clampPercent(data.progress_percent);
                const spd = data.speed ? formatSpeed(data.speed) : '';
                btn.innerHTML = `<span class="loading"></span> (${step}/${totalSteps}) ${pct}%${spd ? ' ' + spd : ''}`;
            } else if (status === 'completed' || status === 'merged') {
                btn.disabled = false;
                btn.innerHTML = '<i class="fa-solid fa-check"></i> 已完成';
                setTone(btn, 'success');
                
            } else if (status === 'failed') {
                btn.disabled = false;
                btn.innerHTML = '<i class="fa-solid fa-redo"></i> 重试';
                setTone(btn, 'error');
                
            }
        };
        
        if (taskType === 'audio') {
            updateBtn(audioBtn);
        } else {
            updateBtn(downloadBtn);
        }
    }

    // 2. 查找下载队列中对应的任务元素并更新
    const uniqueId = `${bvid}_${taskType}`;
    
    const downloadItem = document.querySelector(`.download-item[data-id="${uniqueId}"]`);
    if (downloadItem) {
        updateDownloadItemProgress(downloadItem, data);
        
        // 如果状态发生变化（例如从 downloading 变为 completed/failed），需要刷新整个列表。
        if (oldStatus && oldStatus !== data.status && (data.status === 'completed' || data.status === 'failed' || data.status === 'merged')) {
            setTimeout(() => updateDownloadLists(), 500);
        }
        return;
    }
    
    // 兼容旧数据匹配
    const items = document.querySelectorAll(`.download-item[data-bvid="${bvid}"]`);
    for (const item of items) {
        if ((item.dataset.type || 'video') === taskType) {
            updateDownloadItemProgress(item, data);
            
            // 如果状态发生变化，需要刷新整个列表
            if (oldStatus && oldStatus !== data.status && (data.status === 'completed' || data.status === 'failed' || data.status === 'merged')) {
                setTimeout(() => updateDownloadLists(), 500);
            }
            return;
        }
    }
    
    // 3. 如果元素不存在但我们在下载管理页面，可能需要刷新列表
    if (document.getElementById('tab-history')?.classList.contains('active')) {
        // 检查是否是新任务或状态变化
        if (!oldStatus || oldStatus !== data.status) {
            setTimeout(() => updateDownloadLists(), 300);
        }
    }
}

// 更新单个下载项的进度
export function updateDownloadItemProgress(downloadItem, data) {
    const progressFill = downloadItem.querySelector('.download-item-progress-fill');
    const progressText = downloadItem.querySelector('.download-item-progress-text');
    const metaInfo = downloadItem.querySelector('.download-item-meta');

    if (!progressFill || !progressText) return;

    const progress = clampPercent(data.progress_percent);
    const status = data.status;
    const downloaded = data.downloaded_size ? formatFileSize(data.downloaded_size) : '0 B';
    const total = data.total_size ? formatFileSize(data.total_size) : '未知';
    const speed = data.speed ? formatSpeed(data.speed) : '';
    const taskType = data.type || 'video';
    // 步骤信息（后端 broadcast 字段），用于分数式进度文案
    const step = data.step || 1;
    const totalSteps = data.total_steps || 2;
    const stepLabel = data.step_label || '';

    // 更新进度条宽度
    progressFill.value = progress;

    // 根据状态更新样式
    progressFill.classList.remove('completed', 'failed');
    if (status === 'completed' || status === 'merged') {
        progressFill.classList.add('completed');
    } else if (status === 'failed') {
        progressFill.classList.add('failed');
    }

    // 更新进度文本
    if (status === 'completed' || status === 'merged') {
        progressText.textContent = '100% · 已完成';
        setTone(progressText, 'success');
    } else if (status === 'failed') {
        progressText.textContent = '失败';
        setTone(progressText, 'error');
    } else if (status === 'downloading') {
        progressText.textContent = `(${step}/${totalSteps})${stepLabel ? ` ${stepLabel}` : ''} ${progress}% · ${downloaded} / ${total}${speed ? ` · ${speed}` : ''}`;
        setTone(progressText);
    } else {
        progressText.textContent = '等待中';
        setTone(progressText);
    }

    // 更新状态图标和速度信息
    if (metaInfo) {
        let statusText = '等待中';
        let statusIcon = '<i class="fa-solid fa-clock status-pending"></i>';

        if (status === 'downloading') {
            statusText = `(${step}/${totalSteps}) ${stepLabel || '下载中'}`;
            statusIcon = '<i class="fa-solid fa-spinner fa-spin status-progress"></i>';
        } else if (status === 'completed' || status === 'merged') {
            statusText = '已完成';
            statusIcon = '<i class="fa-solid fa-check-circle status-success"></i>';
        } else if (status === 'failed') {
            statusText = '失败';
            statusIcon = '<i class="fa-solid fa-exclamation-circle status-error"></i>';
        }

        metaInfo.innerHTML = `${statusIcon} ${statusText}${speed && status === 'downloading' ? ` · ${speed}` : ''} ${taskType === 'audio' ? '· 音频' : ''}`;
    }
}
