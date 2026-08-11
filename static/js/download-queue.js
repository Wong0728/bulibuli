import { _state } from './state.js';
import { clampPercent, escapeHtml, formatFileSize, formatSpeed } from './utils.js';
import { apiPost } from './core.js';
import { fetchDownloadSnapshot } from './download-status-store.js';
import { retryDownload, loadHistoryBoard } from './history.js';
import { showToast, confirmDialog } from './download-status.js';

// --- 下载管理页面功能 ---

        // 暂停/恢复成功后刷新看板（仅当下载管理 tab 激活时），让按钮与状态点及时切换。
        // 若抽屉打开则同步刷新抽屉，使暂停/恢复按钮与进度立即更新。
function refreshBoardIfActive() {
    if (document.getElementById('tab-history')?.classList.contains('active')) {
        loadHistoryBoard(_state.currentBoardTab || 'completed');
    }
    if (_state.currentDrawerBvid) {
        import('./drawer.js').then(({ openVideoDrawer }) => {
            openVideoDrawer(_state.currentDrawerBvid);
        });
    }
}

export async function updateDownloadLists() {
    try {
        const result = await fetchDownloadSnapshot();
        if (result.code === 0) {
            const statuses = result.data?.statuses || {};

            _state.currentDownloadStatuses = statuses;

            // 高频进度补丁：直接更新看板卡片的进度条，避免整版重渲染
            patchBoardCardProgress(statuses);
        }
    } catch (e) {
        // 静默处理网络错误
    }
}

// 高频进度补丁：遍历看板上的 .board-video-card，按 bvid 匹配 statuses 更新进度条和速度。
// 用于 L0 高频轮询（1.5s），避免每次都整版重渲染看板。
export function patchBoardCardProgress(statuses) {
    _state.boardCardIndex ||= new Map();
    const byBvid = new Map();
    Object.values(statuses || {}).forEach(status => {
        if (!status?.bvid) return;
        const previous = byBvid.get(status.bvid);
        if (!previous || taskStatusPriority(status.status) > taskStatusPriority(previous.status)) {
            byBvid.set(status.bvid, status);
        }
    });
    byBvid.forEach((status, bvid) => {
        let card = _state.boardCardIndex.get(bvid);
        if (!card?.isConnected) {
            card = document.querySelector(`.board-video-card[data-bvid="${CSS.escape(bvid)}"]`);
            if (card) _state.boardCardIndex.set(bvid, card);
        }
        if (!card) return;
        // download/status 的 key 是 bvid 或 bvid_type。
        // 注意不能用 startsWith 兜底：会先匹配到秒完成的 bvid_audio（字母序在 video 前），
        // 导致进度条永远显示 100%。这里明确优先取正在下载中的任务（video > audio）。
        _applyProgressToCard(card, status);
    });
}

function taskStatusPriority(status) {
    return ({ downloading: 4, pending: 3, retrying: 3, paused: 3, failed: 2, completed: 1 })[status] || 0;
}

// WebSocket 实时推送时直接更新单张看板卡片，无需等待 HTTP 轮询。
export function patchSingleCardProgress(bvid, data) {
    _state.boardCardIndex ||= new Map();
    let card = _state.boardCardIndex.get(bvid);
    if (!card?.isConnected) {
        card = document.querySelector(`.board-video-card[data-bvid="${CSS.escape(bvid)}"]`);
        if (card) _state.boardCardIndex.set(bvid, card);
    }
    if (!card) return;
    _applyProgressToCard(card, data);
}

// 内部：将进度数据应用到卡片 DOM，包含“(X/N) 步骤名 进度%”展示。
function _applyProgressToCard(card, status) {
    const progress = clampPercent(status.progress_percent);
    const speed = status.speed ? formatSpeed(status.speed) : '';
    const downloadedSize = status.downloaded_size ? formatFileSize(status.downloaded_size) : '';
    const totalSize = status.total_size ? formatFileSize(status.total_size) : '';
    const isDownloading = status.status === 'downloading';
    const isPending = status.status === 'pending';
    const isPaused = status.status === 'paused';

    // 步骤信息（后端 broadcast 字段），用于分数式进度文本
    const step = status.step || 1;
    const totalSteps = status.total_steps || 2;
    const stepLabel = status.step_label || '';
    const stepText = `(${step}/${totalSteps})`;
    const progressDisplay = isDownloading
        ? `${stepText}${stepLabel ? ` ${escapeHtml(stepLabel)}` : ''} ${progress}%`
        : isPending ? `${stepText} 等待中`
        : isPaused ? `${stepText} 已暂停 ${progress}%`
        : '';
    const speedDisplay = isDownloading && speed ? speed : '';

    // 查找或创建进度条容器
    let progWrap = card.querySelector('.board-card-progress');
    let progText = card.querySelector('.board-card-progress-text');
    if (isDownloading || isPending || isPaused) {
        const textHtml = `<span>${progressDisplay}</span>${speedDisplay ? `<span class="board-card-speed">${speedDisplay}</span>` : ''}${downloadedSize && totalSize ? `<span class="board-card-size">${downloadedSize} / ${totalSize}</span>` : ''}`;
        if (!progWrap) {
            const body = card.querySelector('.board-card-body');
            if (body) {
                const sidecar = body.querySelector('.board-card-sidecar');
                const tmp = document.createElement('div');
                tmp.innerHTML = `<div class="board-card-progress"><progress class="board-card-progress-bar" max="100" value="${progress}"></progress></div><div class="board-card-progress-text">${textHtml}</div>`;
                const frags = Array.from(tmp.childNodes);
                frags.forEach(f => sidecar ? body.insertBefore(f, sidecar) : body.appendChild(f));
            }
        } else {
            const bar = progWrap.querySelector('.board-card-progress-bar');
            if (bar) bar.value = progress;
            if (progText) {
                progText.innerHTML = textHtml;
            }
        }
    } else if (progWrap && progText) {
        // 已完成 / 失败：移除进度条
        progWrap.remove();
        progText.remove();
    }
}

// 绑定下载项事件（事件委托）
export function bindDownloadItemEvents(container) {
    if (!container) return;
    
    // 重复初始化时先解绑，避免一次点击触发多次。
    container.removeEventListener('click', handleDownloadItemClick);
    container.addEventListener('click', handleDownloadItemClick);
}

// 处理下载项点击事件
export function handleDownloadItemClick(e) {
    // 查找最近的按钮元素
    const button = e.target.closest('[data-action]');
    if (!button) return;

    const action = button.dataset.action;
    const bvid = button.dataset.bvid;
    const taskType = button.dataset.type || 'video';

    if (!action || !bvid) return;

    e.preventDefault();
    e.stopPropagation();

    switch (action) {
        case 'retry':
        case 'download':
            retryDownload(bvid, taskType);
            break;
        case 'remove':
            removeDownload(bvid, taskType);
            break;
        case 'burn':
            import('./media-actions.js').then(({ burnMedia }) => {
                burnMedia(bvid, 'subtitle', button);
            });
            break;
    }
}

// 移除下载任务
export async function removeDownload(bvid, taskType = 'video') {
    if (!(await confirmDialog('确定要移除这个下载任务吗？', { title: '移除任务', okText: '移除', danger: true }))) return;

    try {
        const result = await apiPost('/api/download/remove', { bvid, type: taskType });
        if (result.code === 0) {
            showToast('已移除下载任务', 'success');
            updateDownloadLists();
        } else {
            showToast(result.message || '移除失败', 'error');
        }
    } catch (e) {
        showToast('移除下载任务失败', 'error');
    }
}

// 暂停单个下载任务（task_id 来自 /api/download/status 返回的 task_id 字段）。
export async function pauseDownload(taskId) {
    if (!taskId) {
        showToast('缺少任务 ID，无法暂停', 'error');
        return;
    }
    try {
        const result = await apiPost('/api/download/pause', { task_id: taskId });
        if (result.code === 0) {
            showToast(result.message || '已暂停', 'success');
            updateDownloadLists();
            refreshBoardIfActive();
        } else {
            showToast(result.message || '暂停失败', 'error');
        }
    } catch (e) {
        showToast('暂停下载任务失败', 'error');
    }
}

// 恢复单个下载任务。
export async function resumeDownload(taskId) {
    if (!taskId) {
        showToast('缺少任务 ID，无法恢复', 'error');
        return;
    }
    try {
        const result = await apiPost('/api/download/resume', { task_id: taskId });
        if (result.code === 0) {
            showToast(result.message || '已恢复', 'success');
            updateDownloadLists();
            refreshBoardIfActive();
        } else {
            showToast(result.message || '恢复失败', 'error');
        }
    } catch (e) {
        showToast('恢复下载任务失败', 'error');
    }
}

// 全局暂停所有下载任务。
export async function pauseAllDownloads() {
    if (!(await confirmDialog('确定要暂停所有下载任务吗？', { title: '全部暂停', okText: '暂停' }))) return;
    try {
        const result = await apiPost('/api/download/pause', { task_id: null });
        if (result.code === 0) {
            showToast(result.message || '已暂停全部任务', 'success');
            updateDownloadLists();
            refreshBoardIfActive();
        } else {
            showToast(result.message || '全局暂停失败', 'error');
        }
    } catch (e) {
        showToast('全局暂停失败', 'error');
    }
}

// 全局恢复所有暂停任务。
export async function resumeAllDownloads() {
    try {
        const result = await apiPost('/api/download/resume', { task_id: null });
        if (result.code === 0) {
            showToast(result.message || '已恢复全部任务', 'success');
            updateDownloadLists();
            refreshBoardIfActive();
        } else {
            showToast(result.message || '全局恢复失败', 'error');
        }
    } catch (e) {
        showToast('全局恢复失败', 'error');
    }
}
