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
    history: [],
    pendingRooms: new Set(),
    queryPending: false,
    dashboardInFlight: false,
    eventsInFlight: false,
    eventFailedAt: 0,
    visible: document.visibilityState !== 'hidden',
};

async function queryLiveRoom() {
    const input = document.getElementById('live-room-input');
    const rawRoomId = input?.value?.trim() || '';
    const roomId = Number(rawRoomId);
    if (!/^\d+$/.test(rawRoomId) || !Number.isSafeInteger(roomId) || roomId <= 0) {
        showToast('请输入有效的直播间号', 'warning');
        return;
    }
    const button = document.getElementById('live-query-btn');
    if (liveState.queryPending || button.disabled) return;
    liveState.queryPending = true;
    button.disabled = true;
    button.textContent = '查询中…';
    try {
        const response = await apiGet(`/api/live/room-info?room_id=${roomId}`);
        liveState.query = response.data || {};
        renderRoomInfo(liveState.query);
    } catch (error) {
        liveState.query = null;
        document.getElementById('live-room-info-card').hidden = true;
        showToast(`查询失败：${error.message}`, 'error');
    } finally {
        button.disabled = false;
        button.textContent = '查询';
        liveState.queryPending = false;
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
    if (liveState.dashboardInFlight || document.visibilityState === 'hidden') return;
    liveState.dashboardInFlight = true;
    try {
        const response = await apiGet('/api/live/dashboard');
        liveState.dashboard = response.data || {};
        liveState.lastSync = Date.now();
        liveState.failedAt = 0;
        renderDashboard();
        renderHealth(liveState.dashboard);
        setSyncText('本地页面已刷新', 'ok');
        try {
            const history = await apiGet('/api/live/history?limit=20');
            liveState.history = history.data?.items || [];
            renderHistory(liveState.history);
        } catch (error) {
            console.error('[live] 刷新录制历史失败：', error);
        }
        const sessions = liveState.dashboard.sessions || [];
        if (!sessions.some(item => item.room_id === liveState.selectedRoom)) {
            selectInteractionRoom(sessions[0]?.room_id || 0);
        }
    } catch (error) {
        if (!liveState.failedAt) liveState.failedAt = Date.now();
        setSyncText('同步中断', 'error');
        if (!silent) showToast(`直播状态同步失败：${error.message}`, 'error');
    } finally {
        liveState.dashboardInFlight = false;
    }
}

function renderDashboard() {
    renderSources(liveState.dashboard?.sources || []);
    renderRecordings(liveState.dashboard?.sessions || []);
    renderRecovery(liveState.dashboard?.recovery || [], liveState.dashboard?.merge_jobs || []);
    const notice = document.getElementById('live-risk-notice');
    const text = liveState.dashboard?.risk_notice;
    notice.hidden = !text;
    notice.textContent = text || '';
}

function renderRecovery(items, jobs) {
    const node = document.getElementById('live-recovery-list');
    if (!node) return;
    const jobRows = jobs.map(job => `<article class="live-recording-item"><div class="live-recording-info"><strong>后台合并 ${escapeHtml(job.id)}</strong><small>录制 #${job.recording_id}</small></div><progress max="100" value="${job.progress}"></progress><span>${escapeHtml(job.status)}</span>${['queued', 'running', 'cancelling'].includes(job.status) ? `<button class="btn btn-sm btn-ghost" data-action="merge-cancel" data-job-id="${escapeHtml(job.id)}" ${job.cancel_requested ? 'disabled' : ''}>取消</button>` : ''}</article>`);
    const recoveryRows = items.map(item => `<article class="live-recording-item"><div class="live-recording-info"><strong>#${item.recording_id} ${escapeHtml(item.title || '')}</strong><small>保留源分段 ${item.segment_count} 个 · ${escapeHtml(item.error_msg || '可恢复')}</small></div><button class="btn btn-sm btn-primary" data-action="history-merge" data-recording-id="${item.recording_id}">重试合并</button></article>`);
    node.innerHTML = [...jobRows, ...recoveryRows].join('') || '<p class="empty-hint">暂无待恢复任务</p>';
}

function renderHealth(dashboard) {
    const monitor = dashboard?.monitor || {};
    const stale = (dashboard?.sources || []).filter(source => source.runtime?.stale).length;
    const node = document.getElementById('live-health-summary');
    node.innerHTML = `<span>监控：${monitor.running ? '运行中' : '未运行'}</span><span>B站状态：${stale ? `${stale} 个待重试` : '新鲜'}</span><span>录制：${(dashboard?.sessions || []).length}</span>`;
    if (dashboard?.server_timezone) {
        node.insertAdjacentHTML('beforeend', `<span>服务器时区：${escapeHtml(dashboard.server_timezone)}</span>`);
    }
}

function renderSourcesLegacy(sources) {
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
        } else if (runtime.stale) {
            status = '状态已过期';
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
                        ? `<button class="btn btn-sm btn-danger" data-action="live-stop" data-room-id="${source.room_id}" ${liveState.pendingRooms.has(source.room_id) ? 'disabled' : ''}>${liveState.pendingRooms.has(source.room_id) ? '处理中…' : '停止'}</button>`
                        : `<button class="btn btn-sm btn-primary" data-action="live-start" data-room-id="${source.room_id}" ${live && !liveState.pendingRooms.has(source.room_id) ? '' : 'disabled'}>${liveState.pendingRooms.has(source.room_id) ? '处理中…' : '手动录制'}</button>`}
                    <button class="btn btn-sm btn-ghost" data-action="source-edit" data-room-id="${source.room_id}" ${liveState.pendingRooms.has(source.room_id) ? 'disabled' : ''}>设置</button>
                    <button class="btn btn-sm btn-ghost" data-action="source-delete" data-room-id="${source.room_id}" ${recording || liveState.pendingRooms.has(source.room_id) ? 'disabled' : ''}>删除</button>
                </div>
            </article>
        `;
    }).join('');
}

function renderSources(sources) {
    const container = document.getElementById('live-source-list');
    if (!sources.length) {
        container.innerHTML = '<p class="empty-hint">暂无直播源，请先查询并添加</p>';
        return;
    }
    const current = new Map([...container.querySelectorAll('[data-live-source-key]')]
        .map(node => [node.dataset.liveSourceKey, node]));
    const fragment = document.createDocumentFragment();
    for (const source of sources) {
        const key = String(source.room_id);
        const signature = JSON.stringify({ source, recording: liveState.dashboard?.sessions?.find(item => item.room_id === source.room_id), pending: liveState.pendingRooms.has(source.room_id) });
        let node = current.get(key);
        if (!node || node.dataset.liveSignature !== encodeURIComponent(signature)) {
            const template = document.createElement('template');
            template.innerHTML = sourceCardMarkup(source, encodeURIComponent(signature));
            node = template.content.firstElementChild;
        }
        current.delete(key);
        fragment.append(node);
    }
    container.replaceChildren(fragment);
}

function sourceCardMarkup(source, signature) {
    const runtime = source.runtime || {};
    const recording = liveState.dashboard?.sessions?.find(item => item.room_id === source.room_id);
    const pending = liveState.pendingRooms.has(source.room_id);
    const live = runtime.live_status === 1;
    const state = runtime.risk_limited ? 'risk' : runtime.stale ? 'stale' : runtime.error ? 'unknown' : live ? 'live' : runtime.live_status == null ? 'waiting' : 'offline';
    const labels = { risk: 'B站检查受限', stale: '状态已过期', unknown: '状态未知', live: '直播中', offline: '未开播', waiting: '等待首次检查' };
    const checked = runtime.last_checked_at ? `最近检查：${escapeHtml(runtime.last_checked_at)}` : '尚未检查';
    const schedule = source.schedule_all_day ? '全天' : scheduleSummary(source.weekly_schedule);
    return `<article class="live-source-card ${live ? 'is-live' : ''}" data-live-source-key="${source.room_id}" data-live-signature='${escapeHtml(signature)}'>
        <div class="live-source-head">
            ${source.face ? `<img src="/api/video/proxy-image?url=${encodeURIComponent(source.face)}" alt="" loading="lazy">` : '<i class="fa-solid fa-user"></i>'}
            <div><strong>${escapeHtml(source.anchor_name || `UID ${source.uid}`)}</strong><small>房间 ${source.room_id}</small></div>
            <span class="live-source-status live-state-${state}" data-state="${state}" aria-label="${labels[state]}">${labels[state]}</span>
        </div>
        <div class="live-source-title">${escapeHtml(source.title || '尚未获取标题')}</div>
        <div class="live-source-meta"><span>${source.auto_record_enabled ? '自动录制已开启' : '自动录制已关闭'}</span><span>${escapeHtml(source.capture_mode)}</span><span>${escapeHtml(schedule)}</span></div>
        <small class="live-source-checked">${checked}${runtime.error ? ` · ${escapeHtml(runtime.error)}` : ''}${runtime.next_schedule_at ? ` · 下次执行：${escapeHtml(runtime.next_schedule_at)}` : ''}</small>
        ${source.manual_stop_latched ? '<div class="live-lock-note">本场已手动停止，等待真正下播后解除</div>' : ''}
        ${runtime.schedule_overrun ? '<div class="live-lock-note">已超过计划结束时间，将录制至下播</div>' : ''}
        <div class="live-source-actions">
            ${recording ? `<button class="btn btn-sm btn-danger" data-action="live-stop" data-room-id="${source.room_id}" ${pending ? 'disabled' : ''}>${pending ? '处理中…' : '停止'}</button>` : `<button class="btn btn-sm btn-primary" data-action="live-start" data-room-id="${source.room_id}" ${live && !pending ? '' : 'disabled'}>${pending ? '处理中…' : '手动录制'}</button>`}
            <button class="btn btn-sm btn-ghost" data-action="source-edit" data-room-id="${source.room_id}" ${pending ? 'disabled' : ''}>设置</button>
            <button class="btn btn-sm btn-ghost" data-action="source-delete" data-room-id="${source.room_id}" ${recording || pending ? 'disabled' : ''}>删除</button>
        </div>
    </article>`;
}

function renderHistory(items) {
    const node = document.getElementById('live-history-list');
    if (!items.length) {
        node.innerHTML = '<p class="empty-hint">暂无已结束的录制</p>';
        return;
    }
    node.innerHTML = items.map(item => `<article class="live-recording-item">
        <div class="live-recording-info"><span class="live-recording-room">#${item.room_id} · ${escapeHtml(recordingStatusText(item.status))}</span><span class="live-recording-title">${escapeHtml(item.title || '未命名直播')}</span><small>${escapeHtml(item.error_msg || `${formatDuration(item.duration)} · ${formatFileSize(item.file_size)}`)}</small></div>
        <div class="live-recording-actions">${item.has_output ? `<button class="btn btn-sm btn-ghost" data-action="history-open" data-recording-id="${item.id}">打开目录</button>` : ''}</div>
    </article>`).join('');
}

function renderRecordingsLegacy(sessions) {
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

function renderRecordings(sessions) {
    const container = document.getElementById('live-recording-list');
    if (!sessions.length) {
        container.innerHTML = '<p class="empty-hint">暂无录制中的任务</p>';
        renderStats(null);
        return;
    }
    const current = new Map([...container.querySelectorAll('[data-live-recording-key]')]
        .map(node => [node.dataset.liveRecordingKey, node]));
    const fragment = document.createDocumentFragment();
    for (const session of [...sessions].sort((a, b) => String(a.started_at).localeCompare(String(b.started_at)))) {
        const key = String(session.room_id);
        const signature = JSON.stringify(session);
        let node = current.get(key);
        if (!node || node.dataset.liveSignature !== encodeURIComponent(signature)) {
            const template = document.createElement('template');
            template.innerHTML = recordingCardMarkup(session, encodeURIComponent(signature));
            node = template.content.firstElementChild;
        }
        current.delete(key);
        fragment.append(node);
    }
    container.replaceChildren(fragment);
    renderStats(sessions.find(item => item.room_id === liveState.selectedRoom) || sessions[0]);
}

function recordingCardMarkup(session, signature) {
    const state = session.status || 'unknown';
    const interaction = session.interaction_capture_status || 'off';
    return `<article class="live-recording-item" data-live-recording-key="${session.room_id}" data-live-signature='${escapeHtml(signature)}'>
        <div class="live-recording-info"><span class="live-recording-room">#${session.room_id}</span><span class="live-recording-title">${escapeHtml(session.title || '')}</span><small>${escapeHtml(session.error_msg || '')}</small></div>
        <div class="live-recording-meta"><span class="live-duration" data-started-at="${escapeHtml(session.started_at)}">${formatDuration(session.duration_secs)}</span><span>${formatFileSize(session.file_size)}</span><span class="recording-state recording-state-${state}" data-state="${state}">${recordingStatusText(state)}</span><span>${session.trigger === 'auto' ? '自动' : '手动'}</span><span class="interaction-state interaction-state-${interaction}">${escapeHtml(interaction)}</span>${session.dropped_event_count ? `<span class="live-status-off">丢失 ${session.dropped_event_count}</span>` : ''}</div>
        <div class="live-recording-actions"><button class="btn btn-sm btn-ghost" data-action="interaction-select" data-room-id="${session.room_id}">查看互动</button><button class="btn btn-sm btn-danger" data-action="live-stop" data-room-id="${session.room_id}" ${liveState.pendingRooms.has(session.room_id) ? 'disabled' : ''}>${liveState.pendingRooms.has(session.room_id) ? '处理中…' : '停止'}</button></div>
    </article>`;
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
    if (!liveState.selectedRoom || liveState.eventsInFlight || document.visibilityState === 'hidden') return;
    liveState.eventsInFlight = true;
    try {
        const response = await apiGet(`/api/live/events?room_id=${liveState.selectedRoom}&after_seq=${liveState.afterSeq}&limit=100`);
        liveState.eventFailedAt = 0;
        const events = response.data?.events || [];
        if (events.length) {
            liveState.afterSeq = response.data.next_seq || liveState.afterSeq;
            liveState.events = [...liveState.events, ...events].slice(-100);
            renderEvents();
        }
    } catch (error) {
        liveState.eventFailedAt = Date.now();
        const status = document.getElementById('live-events-status');
        if (status) status.textContent = `实时互动状态：暂时不可用（${error.message}）`;
        console.error('[live] 轮询互动事件失败：', error);
    } finally {
        const status = document.getElementById('live-events-status');
        if (status && !liveState.eventFailedAt) status.textContent = '实时互动状态：正常';
        liveState.eventsInFlight = false;
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
    liveState.eventFailedAt = 0;
    const eventStatus = document.getElementById('live-events-status');
    if (eventStatus) eventStatus.textContent = roomId ? '实时互动状态：等待检查' : '实时互动状态：未选择房间';
    document.getElementById('live-interaction-room').textContent = roomId ? `房间 ${roomId}` : '';
    renderEvents();
    pollEvents();
}

async function addSource(roomId) {
    try {
        await apiPost('/api/live/source/add', {
            room_id: roomId,
            auto_record_enabled: false,
            capture_mode: 'standard',
        });
        showToast('直播源已添加', 'success');
        await refreshDashboard(true);
        await queryLiveRoom();
    } catch (error) {
        showToast(`添加失败：${error.message}`, 'error');
    }
}

function scheduleRows(key) {
    return [0, 1].map(index => ({
        start: document.getElementById(`live-schedule-${key}-${index}-start`),
        end: document.getElementById(`live-schedule-${key}-${index}-end`),
    }));
}

function validateSchedule(schedule) {
    const intervals = [];
    for (const [day, windows] of Object.entries(schedule)) {
        for (const value of windows) {
            const [start, end] = value.split('-');
            if (!/^\d{2}:\d{2}$/.test(start) || !/^\d{2}:\d{2}$/.test(end) || start === end) {
                return `${day}: 时间格式应为 HH:MM-HH:MM，且开始和结束不能相同`;
            }
            intervals.push({ day, start, end });
        }
    }
    for (const day of weekdays.map(([key]) => key)) {
        const sameDay = intervals.filter(item => item.day === day).sort((a, b) => a.start.localeCompare(b.start));
        for (let index = 1; index < sameDay.length; index += 1) {
            if (sameDay[index - 1].end > sameDay[index].start && sameDay[index - 1].start < sameDay[index - 1].end) {
                return `${day}: 排期窗口重叠`;
            }
        }
    }
    return '';
}

function validateScheduleStrict(schedule) {
    const intervals = [];
    const toMinutes = value => {
        if (!/^\d{2}:\d{2}$/.test(value)) return NaN;
        const [hour, minute] = value.split(':').map(Number);
        return hour < 24 && minute < 60 ? hour * 60 + minute : NaN;
    };
    for (const [day, windows] of Object.entries(schedule)) {
        const dayIndex = weekdays.findIndex(([key]) => key === day);
        for (const value of windows) {
            const [start, end] = value.split('-');
            const beginValue = toMinutes(start);
            const endValue = toMinutes(end);
            if (dayIndex < 0 || !Number.isFinite(beginValue) || !Number.isFinite(endValue) || beginValue === endValue) {
                return `${day}: schedule must use HH:MM-HH:MM with different endpoints`;
            }
            const begin = dayIndex * 1440 + beginValue;
            let finish = dayIndex * 1440 + endValue;
            if (finish <= begin) finish += 1440;
            [-10080, 0, 10080].forEach(offset => intervals.push({ begin: begin + offset, finish: finish + offset }));
        }
    }
    intervals.sort((left, right) => left.begin - right.begin);
    for (let index = 1; index < intervals.length; index += 1) {
        if (intervals[index - 1].finish > intervals[index].begin) return 'schedule windows overlap';
    }
    return '';
}

function readScheduleFromEditor() {
    return Object.fromEntries(weekdays.map(([key]) => [key, scheduleRows(key)
        .map(({ start, end }) => start?.value && end?.value ? `${start.value}-${end.value}` : '')
        .filter(Boolean)]));
}

function writeScheduleToEditor(schedule = {}) {
    weekdays.forEach(([key]) => {
        const values = schedule[key] || [];
        scheduleRows(key).forEach(({ start, end }, index) => {
            const [startValue, endValue] = (values[index] || '').split('-');
            if (start) start.value = startValue || '';
            if (end) end.value = endValue || '';
        });
    });
}

function openSourceEditor(roomId) {
    const source = liveState.dashboard?.sources?.find(item => item.room_id === roomId);
    if (!source) return;
    document.getElementById('live-source-room-id').value = roomId;
    document.getElementById('live-source-auto').checked = source.auto_record_enabled;
    document.getElementById('live-source-mode').value = source.capture_mode;
    document.getElementById('live-source-all-day').checked = source.schedule_all_day;
    writeScheduleToEditor(source.weekly_schedule || {});
    toggleScheduleEditor();
    document.getElementById('live-source-dialog').showModal();
}

async function saveSource() {
    const roomId = Number(document.getElementById('live-source-room-id').value);
    const allDay = document.getElementById('live-source-all-day').checked;
    const schedule = readScheduleFromEditor();
    if (!allDay) {
        const validationError = validateScheduleStrict(schedule);
        if (validationError) {
            showToast(validationError, 'warning');
            return;
        }
    }
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
    if (liveState.pendingRooms.has(roomId)) return;
    liveState.pendingRooms.add(roomId);
    renderDashboard();
    try {
        await apiPost('/api/live/start', { room_id: roomId });
        showToast('录制已开始', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`开始录制失败：${error.message}`, 'error');
    } finally {
        liveState.pendingRooms.delete(roomId);
        renderDashboard();
    }
}

async function stopRecording(roomId) {
    if (liveState.pendingRooms.has(roomId)) return;
    if (!window.confirm('停止会先收尾互动数据，再合并并校验录制文件；该过程可能需要一些时间。继续吗？')) return;
    liveState.pendingRooms.add(roomId);
    renderDashboard();
    try {
        const response = await apiPost('/api/live/stop', { room_id: roomId });
        const operationId = response.data?.operation_id;
        if (operationId) {
            showToast(`停止请求已接受，后台任务 ${operationId} 正在收尾`, 'success');
        }
        showToast('录制已停止，本场不会自动拉起', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`停止录制失败：${error.message}`, 'error');
    } finally {
        liveState.pendingRooms.delete(roomId);
        renderDashboard();
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
        finalizing: '收尾合并中',
        stopped: '已停止',
        completed: '已完成',
        failed: '失败',
        cancelled: '已取消',
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

function scheduleEditorMarkup() {
    return weekdays.map(([key, label]) => `<div class="live-schedule-day" data-day="${key}"><span>${label}</span>${[0, 1].map(index => `<div class="live-schedule-window"><input type="time" id="live-schedule-${key}-${index}-start" aria-label="${label} start ${index + 1}"><span aria-hidden="true">–</span><input type="time" id="live-schedule-${key}-${index}-end" aria-label="${label} end ${index + 1}"></div>`).join('')}</div>`).join('');
}

function initLiveTab() {
    const schedule = document.getElementById('live-weekly-schedule');
    schedule.innerHTML = weekdays.map(([key, label]) => `
        <label>
            <span>${label}</span>
            <input id="live-schedule-${key}" class="form-control" placeholder="例如 18:00-23:30">
        </label>
    `).join('');

    schedule.addEventListener('input', () => {
        const error = validateScheduleStrict(readScheduleFromEditor());
        schedule.dataset.valid = error ? 'false' : 'true';
        schedule.setAttribute('aria-invalid', error ? 'true' : 'false');
    });
    schedule.innerHTML = scheduleEditorMarkup();
    document.getElementById('live-query-btn')?.addEventListener('click', queryLiveRoom);
    document.getElementById('live-room-input')?.addEventListener('keydown', event => {
        if (event.key === 'Enter') queryLiveRoom();
    });
    document.getElementById('live-refresh-status-btn')?.addEventListener('click', () => refreshDashboard(false));
    document.getElementById('live-source-save')?.addEventListener('click', saveSource);
    document.getElementById('live-source-all-day')?.addEventListener('change', toggleScheduleEditor);
    document.getElementById('live-event-filter')?.addEventListener('change', renderEvents);
    document.getElementById('live-heat-bar')?.addEventListener('click', event => {
        const button = event.target.closest('[data-time-ms]');
        if (!button) return;
        const target = [...document.querySelectorAll('.live-event-row')].find(row => row.querySelector('time')?.textContent === formatMediaTime(Number(button.dataset.timeMs)));
        target?.scrollIntoView({ behavior: 'smooth', block: 'center' });
    });
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
        else if (action === 'history-merge') {
            apiPost(`/api/live/history/${button.dataset.recordingId}/merge`, {})
                .then(() => refreshDashboard(true))
                .catch(error => showToast(`合并任务创建失败：${error.message}`, 'error'));
        }
        else if (action === 'merge-cancel') {
            apiPost(`/api/live/merge/${button.dataset.jobId}/cancel`, {})
                .then(() => refreshDashboard(true))
                .catch(error => showToast(`取消合并失败：${error.message}`, 'error'));
        }
        else if (action === 'history-open') {
            apiPost(`/api/live/history/${button.dataset.recordingId}/open-directory`, {})
                .catch(error => showToast(`打开目录失败：${error.message}`, 'error'));
        }
    });

    document.addEventListener('visibilitychange', () => {
        liveState.visible = document.visibilityState !== 'hidden';
        if (liveState.visible) {
            refreshDashboard(true);
            pollEvents();
        }
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
