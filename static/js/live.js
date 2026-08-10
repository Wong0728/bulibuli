import { apiGet, apiPost } from './core.js';
import { showToast } from './download-status.js';
import { escapeHtml } from './utils.js';
import { getLiveActionState, mergeLiveEvents } from './live-contract.js';

const weekdays = [
    ['mon', '周一'],
    ['tue', '周二'],
    ['wed', '周三'],
    ['thu', '周四'],
    ['fri', '周五'],
    ['sat', '周六'],
    ['sun', '周日'],
];

const liveState = {
    dashboard: null,
    query: null,
    lastSync: 0,
    failedAt: 0,
    selectedRoom: 0,
    afterSeq: 0,
    events: [],
};

async function queryLiveRoom() {
    const input = document.getElementById('live-room-input');
    const roomId = Number.parseInt(input?.value?.trim(), 10);
    if (!roomId) {
        showToast('请输入有效的直播间号', 'warning');
        return;
    }
    const button = document.getElementById('live-query-btn');
    button.disabled = true;
    button.textContent = '查询中…';
    try {
        const response = await apiGet(`/api/live/room-info?room_id=${roomId}`);
        liveState.query = response.data || {};
        renderRoomInfo(liveState.query);
    } catch (error) {
        showToast(`查询失败：${error.message}`, 'error');
    } finally {
        button.disabled = false;
        button.textContent = '查询';
    }
}

function renderRoomInfo(data) {
    const card = document.getElementById('live-room-info-card');
    const content = document.getElementById('live-room-info-content');
    card.hidden = false;
    const action = getLiveActionState(data);
    const recordButton = action === 'stop'
        ? `<button class="btn btn-danger" data-action="live-stop" data-room-id="${data.room_id}">停止录制</button>`
        : `<button class="btn btn-primary" data-action="live-start" data-room-id="${data.room_id}" ${action === 'disabled' ? 'disabled' : ''}>开始录制</button>`;
    const sourceButton = data.is_saved
        ? '<span class="live-source-saved"><i class="fa-solid fa-check"></i> 已添加</span>'
        : `<button class="btn btn-ghost" data-action="source-add" data-room-id="${data.room_id}">添加直播源</button>`;
    const cover = data.user_cover ? `/api/video/proxy-image?url=${encodeURIComponent(data.user_cover)}` : '';
    content.innerHTML = `
        <div class="live-room-detail">
            ${cover ? `<img class="live-cover" src="${escapeHtml(cover)}" alt="直播封面">` : ''}
            <div class="live-info-text">
                <div class="live-title">${escapeHtml(data.title || '未命名直播间')}</div>
                <div class="live-meta">
                    <span>${escapeHtml(data.anchor_name || `UID ${data.uid}`)}</span>
                    <span>房间 ${data.room_id}</span>
                </div>
                <div class="live-meta">
                    <span class="${data.live_status === 1 ? 'live-status-on' : 'live-status-off'}">${escapeHtml(data.live_status_text || '未知')}</span>
                    <span>${escapeHtml(data.parent_area_name || '未分类')} / ${escapeHtml(data.area_name || '未分类')}</span>
                </div>
            </div>
        </div>
        <div class="live-actions">${sourceButton}${recordButton}</div>
    `;
}

export async function refreshDashboard(silent = false) {
    try {
        const response = await apiGet('/api/live/dashboard');
        liveState.dashboard = response.data || {};
        liveState.lastSync = Date.now();
        liveState.failedAt = 0;
        renderDashboard();
        setSyncText('刚刚已同步', 'ok');
        const sessions = liveState.dashboard.sessions || [];
        if (!sessions.some(item => item.room_id === liveState.selectedRoom)) {
            selectInteractionRoom(sessions[0]?.room_id || 0);
        }
    } catch (error) {
        if (!liveState.failedAt) liveState.failedAt = Date.now();
        setSyncText('同步中断', 'error');
        if (!silent) showToast(`直播状态同步失败：${error.message}`, 'error');
    }
}

function renderDashboard() {
    renderSources(liveState.dashboard?.sources || []);
    renderRecordings(liveState.dashboard?.sessions || []);
    const notice = document.getElementById('live-risk-notice');
    const text = liveState.dashboard?.risk_notice;
    notice.hidden = !text;
    notice.textContent = text || '';
}

function renderSources(sources) {
    const container = document.getElementById('live-source-list');
    if (!sources.length) {
        container.innerHTML = '<p class="empty-hint">暂无直播源，请先在上方查询并添加</p>';
        return;
    }
    container.innerHTML = sources.map(source => {
        const runtime = source.runtime || {};
        const live = runtime.live_status === 1;
        let status;
        if (runtime.risk_limited) {
            status = 'B站状态检查受限';
        } else if (runtime.error) {
            status = '状态未知';
        } else if (live) {
            status = '直播中';
        } else if (runtime.live_status == null) {
            status = '未开播';
        } else {
            status = '未开播';
        }
        const schedule = source.schedule_all_day ? '全天' : scheduleSummary(source.weekly_schedule);
        const recording = (liveState.dashboard?.sessions || []).find(item => item.room_id === source.room_id);
        return `
            <article class="live-source-card ${live ? 'is-live' : ''}">
                <div class="live-source-head">
                    ${source.face ? `<img src="/api/video/proxy-image?url=${encodeURIComponent(source.face)}" alt="" loading="lazy">` : '<i class="fa-solid fa-user"></i>'}
                    <div>
                        <strong>${escapeHtml(source.anchor_name || `UID ${source.uid}`)}</strong>
                        <small>房间 ${source.room_id}</small>
                    </div>
                    <span class="live-source-status ${live ? 'live-status-on' : 'live-status-off'}">${escapeHtml(status)}</span>
                </div>
                <div class="live-source-title">${escapeHtml(source.title || '尚未获取标题')}</div>
                <div class="live-source-meta">
                    <span>${source.auto_record_enabled ? '自动录制已开启' : '自动录制已关闭'}</span>
                    <span>${escapeHtml(source.capture_mode)}</span>
                    <span>${escapeHtml(schedule)}</span>
                </div>
                ${source.manual_stop_latched ? '<div class="live-lock-note">本场已手动停止，等待真正下播后解除</div>' : ''}
                ${runtime.schedule_overrun ? '<div class="live-lock-note">已越过计划结束时间，将录制至下播</div>' : ''}
                <div class="live-source-actions">
                    ${recording
                        ? `<button class="btn btn-sm btn-danger" data-action="live-stop" data-room-id="${source.room_id}">停止</button>`
                        : `<button class="btn btn-sm btn-primary" data-action="live-start" data-room-id="${source.room_id}" ${live ? '' : 'disabled'}>手动录制</button>`}
                    <button class="btn btn-sm btn-ghost" data-action="source-edit" data-room-id="${source.room_id}">设置</button>
                    <button class="btn btn-sm btn-ghost" data-action="source-delete" data-room-id="${source.room_id}">删除</button>
                </div>
            </article>
        `;
    }).join('');
}

function renderRecordings(sessions) {
    const container = document.getElementById('live-recording-list');
    if (!sessions.length) {
        container.innerHTML = '<p class="empty-hint">暂无录制中的任务</p>';
        renderStats(null);
        selectInteractionRoom(0);
        return;
    }
    container.innerHTML = sessions.map(session => `
        <article class="live-recording-item" data-room-id="${session.room_id}">
            <div class="live-recording-info">
                <span class="live-recording-room">#${session.room_id}</span>
                <span class="live-recording-title">${escapeHtml(session.title)}</span>
            </div>
            <div class="live-recording-meta">
                <span class="live-duration" data-started-at="${escapeHtml(session.started_at)}">${formatDuration(session.duration_secs)}</span>
                <span>${formatFileSize(session.file_size)}</span>
                <span class="live-status-on">${recordingStatusText(session.status)}</span>
                <span>${session.trigger === 'auto' ? '自动' : '手动'}</span>
                <span>${escapeHtml(session.interaction_capture_status || 'off')}</span>
                ${session.dropped_event_count ? `<span class="live-status-off">丢失 ${session.dropped_event_count}</span>` : ''}
            </div>
            <div class="live-recording-actions">
                <button class="btn btn-sm btn-ghost" data-action="interaction-select" data-room-id="${session.room_id}">查看互动</button>
                <button class="btn btn-sm btn-danger" data-action="live-stop" data-room-id="${session.room_id}">停止</button>
            </div>
        </article>
    `).join('');
    renderStats(sessions.find(item => item.room_id === liveState.selectedRoom) || sessions[0]);
}

function renderStats(session) {
    const node = document.getElementById('live-interaction-stats');
    if (!session) {
        node.innerHTML = '';
        return;
    }
    const stats = [
        ['弹幕', session.danmaku_count],
        ['互动人数', session.unique_user_count],
        ['免费礼物', session.free_gift_count],
        ['付费礼物', session.paid_gift_count],
        ['SC', session.sc_count],
        ['上舰', session.guard_count],
        ['观看峰值', session.peak_watched],
        ['估算付费价值', `¥${Number(session.estimated_paid_value || 0).toFixed(2)}`],
    ];
    node.innerHTML = stats.map(([label, value]) => `<div><strong>${value || 0}</strong><span>${label}</span></div>`).join('');
}

async function pollEvents() {
    if (!liveState.selectedRoom) return;
    try {
        const response = await apiGet(`/api/live/events?room_id=${liveState.selectedRoom}&after_seq=${liveState.afterSeq}&limit=100`);
        const events = response.data?.events || [];
        if (events.length) {
            liveState.afterSeq = response.data.next_seq || liveState.afterSeq;
            liveState.events = [...liveState.events, ...events].slice(-100);
            renderEvents();
        }
    } catch (error) {
        console.error('[live] 轮询互动事件失败：', error);
    }
}

function renderEvents() {
    const filter = document.getElementById('live-event-filter')?.value || 'all';
    const events = mergeLiveEvents(liveState.events).filter(event => filter === 'all' || event.event_type === filter);
    const timeline = document.getElementById('live-event-timeline');
    timeline.innerHTML = events.length
        ? events.slice().reverse().map(eventRow).join('')
        : '<p class="empty-hint">暂无符合条件的互动</p>';
    const pins = liveState.events.filter(event => event.event_type === 'super_chat').slice(-3).reverse();
    document.getElementById('live-sc-pins').innerHTML = pins.map(event => `
        <div class="live-sc-card">
            <strong>${escapeHtml(event.data?.uname || 'SC')}</strong>
            <span>¥${event.data?.price || 0}</span>
            <p>${escapeHtml(event.data?.message || '')}</p>
        </div>
    `).join('');
    renderHeatBar();
}

function eventRow(event) {
    const data = event.data || {};
    const type = event.event_type;
    const text = type === 'danmaku'
        ? data.text
        : type === 'gift'
            ? `${data.gift_name || '礼物'} ×${data.num || 1}`
            : type === 'super_chat'
                ? `SC ¥${data.price || 0}：${data.message || ''}`
                : type === 'guard'
                    ? `上舰 等级 ${data.guard_level || '-'}`
                    : type === 'link_mic_pk'
                        ? event.cmd
                        : type === 'interact'
                            ? '进入直播间'
                            : event.cmd;
    return `
        <div class="live-event-row live-event-${escapeHtml(type)}">
            <time>${formatMediaTime(event.media_time_ms)}</time>
            <strong>${escapeHtml(data.uname || '')}</strong>
            <span>${escapeHtml(text)}</span>
            ${event.merged_count > 1 ? `<em>×${event.merged_count}</em>` : ''}
        </div>
    `;
}

function renderHeatBar() {
    const buckets = [];
    liveState.events.filter(event => event.event_type === 'danmaku').forEach(event => {
        const index = Math.floor((event.media_time_ms || 0) / 30000);
        buckets[index] = (buckets[index] || 0) + 1;
    });
    const max = Math.max(1, ...buckets.filter(Boolean));
    document.getElementById('live-heat-bar').innerHTML = buckets.map((count = 0, index) => `
        <button title="${formatMediaTime(index * 30000)} · ${count} 条弹幕" style="--heat:${count / max}" data-time-ms="${index * 30000}"></button>
    `).join('');
}

function selectInteractionRoom(roomId) {
    if (liveState.selectedRoom === roomId) return;
    liveState.selectedRoom = roomId;
    liveState.afterSeq = 0;
    liveState.events = [];
    document.getElementById('live-interaction-room').textContent = roomId ? `房间 ${roomId}` : '';
    renderEvents();
    pollEvents();
}

async function addSource(roomId) {
    try {
        await apiPost('/api/live/source/add', {
            room_id: roomId,
            auto_record_enabled: true,
            capture_mode: 'standard',
        });
        showToast('直播源已添加', 'success');
        await refreshDashboard(true);
        await queryLiveRoom();
    } catch (error) {
        showToast(`添加失败：${error.message}`, 'error');
    }
}

function openSourceEditor(roomId) {
    const source = liveState.dashboard?.sources?.find(item => item.room_id === roomId);
    if (!source) return;
    document.getElementById('live-source-room-id').value = roomId;
    document.getElementById('live-source-auto').checked = source.auto_record_enabled;
    document.getElementById('live-source-mode').value = source.capture_mode;
    document.getElementById('live-source-all-day').checked = source.schedule_all_day;
    weekdays.forEach(([key]) => {
        document.getElementById(`live-schedule-${key}`).value = (source.weekly_schedule?.[key] || []).join(', ');
    });
    toggleScheduleEditor();
    document.getElementById('live-source-dialog').showModal();
}

async function saveSource() {
    const roomId = Number(document.getElementById('live-source-room-id').value);
    const allDay = document.getElementById('live-source-all-day').checked;
    const schedule = Object.fromEntries(weekdays.map(([key]) => [
        key,
        document.getElementById(`live-schedule-${key}`).value.split(',').map(v => v.trim()).filter(Boolean),
    ]));
    try {
        await apiPost('/api/live/source/update', {
            room_id: roomId,
            auto_record_enabled: document.getElementById('live-source-auto').checked,
            capture_mode: document.getElementById('live-source-mode').value,
            clear_schedule: allDay,
            weekly_schedule: allDay ? null : schedule,
        });
        document.getElementById('live-source-dialog').close();
        showToast('直播源设置已保存', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`保存失败：${error.message}`, 'error');
    }
}

async function deleteSource(roomId) {
    if (!window.confirm('确定删除这个直播源吗？录制文件不会被删除。')) return;
    try {
        await apiPost('/api/live/source/delete', { room_id: roomId });
        showToast('直播源已删除', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`删除失败：${error.message}`, 'error');
    }
}

async function startRecording(roomId) {
    try {
        await apiPost('/api/live/start', { room_id: roomId });
        showToast('录制已开始', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`开始录制失败：${error.message}`, 'error');
    }
}

async function stopRecording(roomId) {
    try {
        await apiPost('/api/live/stop', { room_id: roomId });
        showToast('录制已停止，本场不会自动拉起', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`停止录制失败：${error.message}`, 'error');
    }
}

function scheduleSummary(schedule = {}) {
    const days = weekdays.filter(([key]) => schedule[key]?.length).length;
    return days ? `每周 ${days} 天` : '计划未启用';
}

function setSyncText(text, kind) {
    const node = document.getElementById('live-sync-state');
    node.textContent = text;
    node.dataset.state = kind;
}

function toggleScheduleEditor() {
    document.getElementById('live-weekly-schedule').hidden = document.getElementById('live-source-all-day').checked;
}

function recordingStatusText(status) {
    const map = {
        starting: '启动中',
        recording: '录制中',
        stopping: '停止中',
        stopped: '已停止',
        completed: '已完成',
        failed: '失败',
    };
    return map[status] || '未知';
}

function formatDuration(value = 0) {
    const seconds = Math.max(0, Math.floor(value));
    return [Math.floor(seconds / 3600), Math.floor(seconds % 3600 / 60), seconds % 60]
        .map(v => String(v).padStart(2, '0'))
        .join(':');
}

function formatMediaTime(ms = 0) {
    return formatDuration(Math.floor(ms / 1000));
}

function formatFileSize(bytes = 0) {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    if (!bytes) return '0 B';
    const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), 4);
    return `${(bytes / 1024 ** i).toFixed(1)} ${units[i]}`;
}

function tickUi() {
    document.querySelectorAll('.live-duration[data-started-at]').forEach(node => {
        const start = Date.parse(node.dataset.startedAt);
        if (!Number.isNaN(start) && start > 0) {
            node.textContent = formatDuration((Date.now() - start) / 1000);
        }
    });
    if (liveState.failedAt && Date.now() - liveState.failedAt > 60000) {
        setSyncText('状态已陈旧', 'stale');
    } else if (liveState.lastSync && !liveState.failedAt) {
        setSyncText(`${Math.floor((Date.now() - liveState.lastSync) / 1000)} 秒前同步`, 'ok');
    }
}

function initLiveTab() {
    const schedule = document.getElementById('live-weekly-schedule');
    schedule.innerHTML = weekdays.map(([key, label]) => `
        <label>
            <span>${label}</span>
            <input id="live-schedule-${key}" class="form-control" placeholder="例如 18:00-23:30">
        </label>
    `).join('');

    document.getElementById('live-query-btn')?.addEventListener('click', queryLiveRoom);
    document.getElementById('live-room-input')?.addEventListener('keydown', event => {
        if (event.key === 'Enter') queryLiveRoom();
    });
    document.getElementById('live-refresh-status-btn')?.addEventListener('click', () => refreshDashboard(false));
    document.getElementById('live-source-save')?.addEventListener('click', saveSource);
    document.getElementById('live-source-all-day')?.addEventListener('change', toggleScheduleEditor);
    document.getElementById('live-event-filter')?.addEventListener('change', renderEvents);
    document.getElementById('tab-live')?.addEventListener('click', event => {
        const button = event.target.closest('[data-action]');
        if (!button || button.disabled) return;
        const roomId = Number(button.dataset.roomId);
        const action = button.dataset.action;
        if (action === 'live-start') startRecording(roomId);
        else if (action === 'live-stop') stopRecording(roomId);
        else if (action === 'source-add') addSource(roomId);
        else if (action === 'source-edit') openSourceEditor(roomId);
        else if (action === 'source-delete') deleteSource(roomId);
        else if (action === 'interaction-select') selectInteractionRoom(roomId);
    });

    refreshDashboard(true);
    window.setInterval(() => refreshDashboard(true), 30000);
    window.setInterval(pollEvents, 2000);
    window.setInterval(tickUi, 1000);
}

if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initLiveTab);
} else {
    initLiveTab();
}

window.refreshDashboard = refreshDashboard;
