import { _state } from './state.js';
import { apiPost } from './core.js';
import { downloadDanmaku, downloadComments } from './media-links.js';
import { loadBloggersFromServer, renderBloggerSidebar, showBloggerEmptyState, updateDetailPanel } from './blogger.js';
import { loadKnownBloggers, buildBloggerOption } from './blogger-search.js';
import { showToast, confirmDialog } from './download-status.js';
import { downloadCover } from './media-actions.js';

// ==================== 模态框功能 ====================
export function showAddBloggerModal(prefill = null) {
    const modal = document.getElementById('add-blogger-modal');
    if (modal) {
        modal.classList.add('active');
        document.body.classList.add('modal-open');
        _state.activeModalTrigger = document.activeElement;
        document.getElementById('modal-blogger-uid').value = prefill?.uid || '';
        document.getElementById('modal-blogger-name').value = prefill?.name || '';
        document.getElementById('modal-min-interval').value = '60';
        document.getElementById('modal-max-interval').value = '300';
        document.getElementById('modal-download-video').checked = true;
        document.getElementById('modal-download-danmaku').checked = true;
        document.getElementById('modal-download-comments').checked = true;
        document.getElementById('modal-download-cover').checked = true;
        document.getElementById('modal-burn-danmaku').checked = false;
        document.getElementById('modal-burn-subtitle').checked = false;
        document.getElementById('modal-series-filter-regex').value = '';
        document.getElementById('modal-start-monitoring').checked = true;
        renderActiveWindowRows([], 'add');
        // 填充已知博主下拉（富内容选项）- 异步加载
        const select = document.getElementById('modal-known-blogger-select');
        if (select) {
            loadKnownBloggers().then(list => {
                select.innerHTML = '<option value="">-- 请从列表选择博主 --</option>' +
                    list.map(b => buildBloggerOption(b)).join('');
                select.value = prefill?.uid && list.some(b => String(b.uid) === String(prefill.uid))
                    ? String(prefill.uid)
                    : '';
                if (prefill?.uid) {
                    const existing = _state.bloggers.find(
                        b => String(b.uid) === String(prefill.uid)
                    );
                    if (existing) {
                        closeAddBloggerModal();
                        showEditBloggerModal(existing.id);
                    }
                }
            });
        }
        setTimeout(() => document.getElementById('modal-blogger-uid')?.focus(), 50);
    }
}

export function closeAddBloggerModal() {
    const modal = document.getElementById('add-blogger-modal');
    if (modal) {
        modal.classList.remove('active');
        document.body.classList.remove('modal-open');
        _state.activeModalTrigger?.focus?.();
        _state.activeModalTrigger = null;
    }
}

export async function loadKnownBloggerIntoAddForm() {
    const select = document.getElementById('modal-known-blogger-select');
    if (!select?.value) return;
    const list = await loadKnownBloggers();
    const blogger = list.find(item => String(item.uid) === String(select.value));
    if (!blogger) return;
    const state = _state.bloggerStates[blogger.id] || {};
    document.getElementById('modal-blogger-uid').value = blogger.uid || '';
    document.getElementById('modal-blogger-name').value = state.name || blogger.name || '';
    document.getElementById('modal-min-interval').value = state.minInterval || blogger.min_interval || 60;
    document.getElementById('modal-max-interval').value = state.maxInterval || blogger.max_interval || 300;
    document.getElementById('modal-download-video').checked = state.download_video !== false;
    document.getElementById('modal-download-danmaku').checked = state.download_danmaku !== false;
    document.getElementById('modal-download-comments').checked = state.download_comments !== false;
    document.getElementById('modal-download-cover').checked = state.download_cover !== false;
    document.getElementById('modal-burn-danmaku').checked = state.burn_danmaku === true;
    document.getElementById('modal-burn-subtitle').checked = state.burn_subtitle === true;
    document.getElementById('modal-series-filter-regex').value = state.series_filter_regex || '';
    document.getElementById('modal-start-monitoring').checked = state.isRunning === true;
    renderActiveWindowRows(Array.isArray(state.active_windows) ? state.active_windows : [], 'add');
}

export async function confirmAddBlogger() {
    const select = document.getElementById('modal-known-blogger-select');
    const uid = document.getElementById('modal-blogger-uid').value.trim() || select.value;
    const nameInput = document.getElementById('modal-blogger-name').value.trim();
    const minInterval = parseInt(document.getElementById('modal-min-interval').value) || 60;
    const maxInterval = parseInt(document.getElementById('modal-max-interval').value) || 300;
    const activeWindows = collectActiveWindows('add');
    if (activeWindows === null) return;

    if (!uid) {
        showToast('请输入博主 UID，或从现有博主列表中选择', 'error');
        return;
    }
    if (minInterval < 30 || minInterval > 3600) {
        showToast('最小检查间隔必须在 30-3600 秒之间', 'error');
        return;
    }
    if (maxInterval < minInterval || maxInterval > 7200) {
        showToast('最大检查间隔必须在最小间隔与 7200 秒之间', 'error');
        return;
    }

    // 从下拉框的 data 属性获取博主名称
    const selectedOption = select.options[select.selectedIndex];
    const name = nameInput || selectedOption?.dataset.name || '';
    const payload = {
        uid,
        name,
        min_interval: minInterval,
        max_interval: maxInterval,
        download_video: document.getElementById('modal-download-video').checked,
        download_danmaku: document.getElementById('modal-download-danmaku').checked,
        download_comments: document.getElementById('modal-download-comments').checked,
        download_cover: document.getElementById('modal-download-cover').checked,
        burn_danmaku: document.getElementById('modal-burn-danmaku').checked,
        burn_subtitle: document.getElementById('modal-burn-subtitle').checked,
        series_filter_regex: document.getElementById('modal-series-filter-regex').value.trim(),
        active_windows: activeWindows,
        start_monitoring: document.getElementById('modal-start-monitoring').checked
    };

    try {
        showToast('正在添加博主...', 'info');

        // 收藏列表与自动任务独立：只有自动任务列表里已存在时才更新，
        // 单纯从收藏下拉选中仍应创建一条新的自动任务配置。
        const existing = _state.bloggers.find(b => String(b.uid) === String(uid));
        const result = existing
            ? await apiPost('/api/blogger/update', {
                ...payload,
                id: existing.id,
                monitor_enabled: payload.start_monitoring
            })
            : await apiPost('/api/blogger/add', payload);

        if (result.code === 0) {
            await loadBloggersFromServer();
            closeAddBloggerModal();
            showToast(`博主 ${name || uid} 的监控配置已保存`, 'success');
        } else {
            showToast(result.message || '添加失败', 'error');
        }
    } catch (e) {
        if (!e.offline) {
            showToast(e.message || '添加博主失败', 'error');
        }
    }
}

// ==================== 右键菜单与编辑功能 ====================
_state.contextMenuBloggerId = null;

export function showContextMenu(event, bloggerId) {
    event.preventDefault();
    _state.contextMenuBloggerId = bloggerId;
    const menu = document.getElementById('context-menu');
    if (!menu) return;
    _state.contextMenuTrigger = event.target instanceof HTMLElement ? event.target : null;
    menu.classList.remove('hidden');
    Object.assign(menu.style, { left: '0px', top: '0px' });
    const rect = menu.getBoundingClientRect();
    const targetRect = event.target instanceof Element ? event.target.getBoundingClientRect() : null;
    const anchorX = event.clientX || targetRect?.left || 0;
    const anchorY = event.clientY || targetRect?.bottom || 0;
    const margin = 8;
    const offset = 6;
    let left = anchorX + offset;
    let top = anchorY + offset;
    if (left + rect.width + margin > window.innerWidth) left = anchorX - rect.width - offset;
    if (top + rect.height + margin > window.innerHeight) top = anchorY - rect.height - offset;
    Object.assign(menu.style, {
        left: `${Math.max(margin, left)}px`,
        top: `${Math.max(margin, top)}px`,
    });
    menu.querySelector('[role="menuitem"]')?.focus();
}

export function hideContextMenu(restoreFocus = false) {
    document.getElementById('context-menu')?.classList.add('hidden');
    if (restoreFocus) _state.contextMenuTrigger?.focus?.();
    _state.contextMenuTrigger = null;
}

export function handleContextMenuEdit() {
    hideContextMenu();
    if (_state.contextMenuBloggerId) showEditBloggerModal(_state.contextMenuBloggerId);
}

export async function handleContextMenuDelete() {
    hideContextMenu();
    const bloggerId = _state.contextMenuBloggerId;
    if (!bloggerId) return;
    const blogger = _state.bloggers.find(item => item.id === bloggerId);
    if (!blogger) return;
    const confirmed = await confirmDialog(
        `确定要删除博主 ${blogger.uid} 吗？\n这会停止监控并删除配置，但不会删除已下载的视频。`,
        { title: '删除博主', okText: '删除', danger: true },
    );
    if (!confirmed) return;
    try {
        await apiPost('/api/blogger/delete', { id: bloggerId });
        showToast('自动任务已删除，已添加博主列表不受影响', 'success');
        if (_state.selectedBloggerId === bloggerId) {
            _state.selectedBloggerId = null;
            showBloggerEmptyState();
        }
        await loadBloggersFromServer();
    } catch (error) {
        showToast(`删除请求失败：${error.message}`, 'error');
    }
}

document.addEventListener('click', event => {
    if (!event.target.closest('.context-menu')) hideContextMenu();
});
window.addEventListener('resize', () => hideContextMenu());
window.addEventListener('scroll', () => hideContextMenu(), true);
document.addEventListener('keydown', event => {
    const menu = document.getElementById('context-menu');
    if (event.key === 'Escape' && menu && !menu.classList.contains('hidden')) {
        event.preventDefault();
        hideContextMenu(true);
    }
});

export function showEditBloggerModal(bloggerId) {
    const state = _state.bloggerStates[bloggerId];
    if (!state) return;
    const modal = document.getElementById('edit-blogger-modal');
    if (!modal) return;
    document.getElementById('edit-blogger-id').value = bloggerId;
    document.getElementById('edit-blogger-uid').value = state.uid;
    document.getElementById('edit-blogger-name').value = state.name || '';
    document.getElementById('edit-min-interval').value = state.minInterval || 60;
    document.getElementById('edit-max-interval').value = state.maxInterval || 300;
    document.getElementById('edit-download-video').checked = state.download_video !== false;
    document.getElementById('edit-download-danmaku').checked = state.download_danmaku !== false;
    document.getElementById('edit-download-comments').checked = state.download_comments !== false;
    document.getElementById('edit-download-cover').checked = state.download_cover !== false;
    document.getElementById('edit-burn-danmaku').checked = state.burn_danmaku === true;
    document.getElementById('edit-burn-subtitle').checked = state.burn_subtitle === true;
    document.getElementById('edit-series-filter-regex').value = state.series_filter_regex || '';
    document.getElementById('edit-monitor-enabled').checked = state.isRunning === true;
    renderActiveWindowRows(Array.isArray(state.active_windows) ? state.active_windows : [], 'edit');
    modal.classList.add('active');
    document.body.classList.add('modal-open');
    _state.activeModalTrigger = document.activeElement;
    setTimeout(() => document.getElementById('edit-blogger-name').focus(), 100);
}

export function closeEditBloggerModal() {
    const modal = document.getElementById('edit-blogger-modal');
    if (modal) {
        modal.classList.remove('active');
        document.body.classList.remove('modal-open');
        _state.activeModalTrigger?.focus?.();
        _state.activeModalTrigger = null;
    }
}

// ==================== 活跃检查时段（闹钟式窗口）编辑 ====================

const MAX_ACTIVE_WINDOWS = 6;

function updateActiveWindowRowMeta(row) {
    const start = row.querySelector('.aw-start')?.value || '';
    const end = row.querySelector('.aw-end')?.value || '';
    const nextDay = row.querySelector('.active-window-next-day');
    if (nextDay) nextDay.hidden = !(start && end && end < start);
}

function createActiveWindowRow(start, end) {
    const row = document.createElement('div');
    row.className = 'active-window-row';
    row.innerHTML = `
        <label class="active-window-time">
            <span>开始</span>
            <span class="time-input-shell"><i class="fa-regular fa-clock"></i><input type="time" step="300" class="aw-start" value="${start}"></span>
        </label>
        <span class="active-window-separator" aria-hidden="true">至</span>
        <label class="active-window-time">
            <span>结束</span>
            <span class="time-input-shell"><i class="fa-regular fa-clock"></i><input type="time" step="300" class="aw-end" value="${end}"></span>
        </label>
        <span class="active-window-next-day" hidden>次日</span>
        <button type="button" class="btn btn-icon btn-danger" data-action="remove-active-window" aria-label="删除该时段" title="删除该时段">
            <i class="fa-solid fa-trash"></i>
        </button>`;
    row.querySelectorAll('input').forEach(input => {
        input.addEventListener('input', () => updateActiveWindowRowMeta(row));
    });
    updateActiveWindowRowMeta(row);
    return row;
}

function renderActiveWindowRows(windows, scope = 'edit') {
    const list = document.getElementById(`${scope}-active-windows-list`);
    if (!list) return;
    list.innerHTML = '';
    windows.forEach(w => {
        const [start, end] = String(w).split('-');
        if (start && end) list.appendChild(createActiveWindowRow(start, end));
    });
    const allDay = document.getElementById(`${scope}-all-day`);
    if (allDay) allDay.checked = windows.length === 0;
    toggleActiveWindowMode(scope);
}

export function addActiveWindowRow(scope = 'edit', start = '18:00', end = '23:00') {
    const list = document.getElementById(`${scope}-active-windows-list`);
    if (!list) return;
    if (list.children.length >= MAX_ACTIVE_WINDOWS) {
        showToast(`最多设置 ${MAX_ACTIVE_WINDOWS} 条活跃时段`, 'error');
        return;
    }
    list.appendChild(createActiveWindowRow(start, end));
    list.lastElementChild?.querySelector('.aw-start')?.focus();
}

export function removeActiveWindowRow(el) {
    el.closest('.active-window-row')?.remove();
}

export function toggleActiveWindowMode(scope = 'edit') {
    const allDay = document.getElementById(`${scope}-all-day`)?.checked === true;
    const list = document.getElementById(`${scope}-active-windows-list`);
    const controls = document.getElementById(`${scope}-active-window-controls`);
    if (list) list.hidden = allDay;
    if (controls) controls.hidden = allDay;
    if (!allDay && list && list.children.length === 0) {
        list.appendChild(createActiveWindowRow('18:00', '23:00'));
    }
}

export function addActiveWindowPreset(scope, preset) {
    const presets = {
        evening: ['18:00', '23:00'],
        overnight: ['22:00', '02:00'],
        daytime: ['08:00', '18:00']
    };
    const value = presets[preset];
    if (value) addActiveWindowRow(scope, value[0], value[1]);
}

// 收集时段行为 ["HH:MM-HH:MM"]；起止有一边为空或相同的行视为无效，返回 null 并提示
function collectActiveWindows(scope = 'edit') {
    if (document.getElementById(`${scope}-all-day`)?.checked) return [];
    const list = document.getElementById(`${scope}-active-windows-list`);
    if (!list) return [];
    const windows = [];
    const seen = new Set();
    for (const row of list.querySelectorAll('.active-window-row')) {
        const start = row.querySelector('.aw-start').value;
        const end = row.querySelector('.aw-end').value;
        row.classList.remove('invalid');
        if (!start || !end || start === end) {
            row.classList.add('invalid');
            showToast('活跃时段的起止时间不能为空或相同', 'error');
            return null;
        }
        const value = `${start}-${end}`;
        if (!seen.has(value)) {
            seen.add(value);
            windows.push(value);
        }
    }
    if (windows.length === 0) {
        showToast('请添加至少一个监测时段，或选择全天监测', 'error');
        return null;
    }
    return windows;
}

export async function confirmEditBlogger() {
    const id = parseInt(document.getElementById('edit-blogger-id').value);
    const name = document.getElementById('edit-blogger-name').value.trim();
    const minInterval = parseInt(document.getElementById('edit-min-interval').value) || 60;
    const maxInterval = parseInt(document.getElementById('edit-max-interval').value) || 300;
    const downloadVideo = document.getElementById('edit-download-video').checked;
    const downloadDanmaku = document.getElementById('edit-download-danmaku').checked;
    const downloadComments = document.getElementById('edit-download-comments').checked;
    const downloadCover = document.getElementById('edit-download-cover').checked;
    const burnDanmaku = document.getElementById('edit-burn-danmaku').checked;
    const burnSubtitle = document.getElementById('edit-burn-subtitle').checked;
    const seriesFilterRegex = document.getElementById('edit-series-filter-regex').value.trim();
    const activeWindows = collectActiveWindows('edit');
    if (activeWindows === null) return;

    if (!id) return;
    if (minInterval < 30 || minInterval > 3600) {
        showToast('最小检查间隔必须在 30-3600 秒之间', 'error');
        return;
    }
    if (maxInterval < minInterval || maxInterval > 7200) {
        showToast('最大检查间隔必须在最小间隔与 7200 秒之间', 'error');
        return;
    }

    try {
        const result = await apiPost('/api/blogger/update', {
            id: id,
            name: name,
            min_interval: minInterval,
            max_interval: maxInterval,
            download_video: downloadVideo,
            download_danmaku: downloadDanmaku,
            download_comments: downloadComments,
            download_cover: downloadCover,
            burn_danmaku: burnDanmaku,
            burn_subtitle: burnSubtitle,
            series_filter_regex: seriesFilterRegex,
            active_windows: activeWindows,
            monitor_enabled: document.getElementById('edit-monitor-enabled')?.checked === true
        });

        if (result.code === 0) {
            showToast('博主配置已更新', 'success');
            closeEditBloggerModal();

            // 后端尚未返回策略字段时，先本地回写保证 UI 不重置
            const state = _state.bloggerStates[id];
            if (state) {
                state.name = name || state.name;
                state.minInterval = minInterval;
                state.maxInterval = maxInterval;
                state.download_video = downloadVideo;
                state.download_danmaku = downloadDanmaku;
                state.download_comments = downloadComments;
                state.download_cover = downloadCover;
                state.burn_danmaku = burnDanmaku;
                state.burn_subtitle = burnSubtitle;
                state.series_filter_regex = seriesFilterRegex;
                state.active_windows = activeWindows;
            }

            await loadBloggersFromServer();

            // 如果正在显示详情，刷新详情
            if (_state.selectedBloggerId === id) {
                updateDetailPanel();
                renderBloggerSidebar();
            }
        } else {
            showToast(result.message || '更新失败', 'error');
        }
    } catch (e) {
        showToast(e.message || '更新请求失败', 'error');
    }
}

// 跟踪 mousedown 起始位置，避免拖选文本时误关闭模态框
_state.modalMouseDownTarget = null;
document.addEventListener('mousedown', function(event) {
    if (event.target.classList.contains('modal-overlay')) {
        _state.modalMouseDownTarget = event.target;
    } else {
        _state.modalMouseDownTarget = null;
    }
});

window.addEventListener('click', function(event) {
    // 仅当 mousedown 与 mouseup 均落在同一遮罩层时才视为"点击遮罩关闭"
    if (event.target.classList.contains('modal-overlay') && _state.modalMouseDownTarget === event.target) {
        if (event.target.id === 'add-blogger-modal') closeAddBloggerModal();
        else if (event.target.id === 'edit-blogger-modal') closeEditBloggerModal();
        else event.target.classList.remove('active');
    }
    _state.modalMouseDownTarget = null;
});

// 弹窗内循环焦点，避免 Tab 跳到遮罩层后的页面。
document.addEventListener('keydown', event => {
    if (event.key !== 'Tab') return;
    const modal = document.querySelector('.modal-overlay.active');
    if (!modal || modal.id === 'confirm-modal') return;
    const focusable = Array.from(modal.querySelectorAll(
        'button:not(:disabled), input:not(:disabled), select:not(:disabled), [tabindex]:not([tabindex="-1"])'
    )).filter(element => !element.hidden && element.offsetParent !== null);
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
    }
});
