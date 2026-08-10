import { apiGet, apiPost } from './core.js';
import { showToast, confirmDialog } from './download-status.js';
import { escapeHtml } from './utils.js';
import { mergeLiveEvents } from './live-contract.js';

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
    dashboardFailedAt: 0,
    selectedRoom: 0,
    afterSeq: 0,
    events: [],
    history: [],
    pendingRooms: new Set(),
    addPending: false,
    dashboardInFlight: false,
    eventsInFlight: false,
    eventFailedAt: 0,
    boardTab: 'recording',
    liveTabActive: false,
};

// ==================== 文案映射：内部英文值一律转中文展示 ====================

function videoStatusText(status) {
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

function videoStatusClass(status) {
    if (status === 'recording') return 'recording';
    if (['starting', 'stopping', 'finalizing'].includes(status)) return 'starting';
    if (status === 'failed') return 'failed';
    if (['stopped', 'completed'].includes(status)) return 'completed';
    return '';
}

function interactionStateText(state) {
    const map = {
        off: '关闭',
        connecting: '连接中',
        capturing: '采集中',
        degraded: '已降级',
        unavailable: '不可用',
        completed: '已完成',
    };
    return map[state] || state || '关闭';
}

function interactionStateClass(state) {
    if (state === 'capturing') return 'capturing';
    if (state === 'connecting') return 'connecting';
    if (state === 'degraded') return 'degraded';
    if (state === 'unavailable') return 'unavailable';
    if (state === 'completed') return 'completed';
    return '';
}

function captureModeText(mode) {
    const map = { standard: '标准', full: '完整原始数据', off: '关闭' };
    return map[mode] || mode || '标准';
}

function qualityText(qn) {
    if (!qn) return '';
    const map = { 10000: '原画', 400: '蓝光', 250: '超清', 150: '高清', 80: '流畅' };
    return map[qn] ? `${map[qn]} (${qn})` : String(qn);
}

function stopReasonText(reason) {
    const map = {
        manual_stop: '手动停止',
        stream_ended_after_offline_confirmation: '自然下播',
        ffmpeg_exit_while_live_or_unconfirmed: 'FFmpeg 异常退出',
        recording_failed: '录制失败',
        recording_completed: '已完成',
    };
    return map[reason] || '';
}

function sourceState(source) {
    const runtime = source.runtime || {};
    if (runtime.risk_limited) return ['risk', 'B站检查受限'];
    if (runtime.stale) return ['stale', '状态已过期'];
    if (runtime.error) return ['unknown', '状态未知'];
    if (runtime.live_status === 1) return ['live', '直播中'];
    if (runtime.live_status === 2) return ['live', '轮播中'];
    if (runtime.live_status == null) return ['waiting', '等待首次检查'];
    return ['offline', '未开播'];
}

function badge(cls, text) {
    return `<span class="live-badge ${cls}">${escapeHtml(text)}</span>`;
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

function relativeTime(rfc3339) {
    if (!rfc3339) return '尚未';
    const time = Date.parse(rfc3339);
    if (Number.isNaN(time)) return escapeHtml(rfc3339);
    const delta = Math.max(0, Math.floor((Date.now() - time) / 1000));
    if (delta < 60) return `${delta} 秒前`;
    if (delta < 3600) return `${Math.floor(delta / 60)} 分钟前`;
    return `${Math.floor(delta / 3600)} 小时前`;
}

function scheduleSummary(source) {
    if (source.schedule_all_day) return '全天自动';
    const schedule = source.weekly_schedule || {};
    const parts = weekdays
        .filter(([key]) => (schedule[key] || []).length)
        .map(([key, label]) => `${label} ${(schedule[key] || []).join('、')}`);
    return parts.length ? `按周排期：${parts.join('；')}` : '排期为空（永不自动开始）';
}

// ==================== 数据拉取 ====================

export async function refreshDashboard(silent = false) {
    if (liveState.dashboardInFlight || document.visibilityState === 'hidden') return;
    liveState.dashboardInFlight = true;
    try {
        const response = await apiGet('/api/live/dashboard');
        liveState.dashboard = response.data || {};
        liveState.dashboardFailedAt = 0;
        setSyncStates();
        renderAll();
        try {
            const history = await apiGet('/api/live/history?limit=30');
            liveState.history = history.data?.items || [];
        } catch (error) {
            console.error('[live] 刷新录制历史失败：', error);
        }
        renderBoard();
        const sessions = liveState.dashboard.sessions || [];
        if (!sessions.some(item => item.room_id === liveState.selectedRoom)
            && !(liveState.dashboard.sources || []).some(item => item.room_id === liveState.selectedRoom)) {
            selectRoom(sessions[0]?.room_id || (liveState.dashboard.sources || [])[0]?.room_id || 0, { silent: true });
        }
    } catch (error) {
        liveState.dashboardFailedAt = liveState.dashboardFailedAt || Date.now();
        setSyncStates();
        if (!silent) showToast(`直播状态同步失败：${error.message}`, 'error');
    } finally {
        liveState.dashboardInFlight = false;
    }
}

function setSyncStates() {
    const pageNode = document.getElementById('live-sync-page');
    const monitorNode = document.getElementById('live-sync-monitor');
    const biliNode = document.getElementById('live-sync-bili');
    const dashboard = liveState.dashboard;

    if (liveState.dashboardFailedAt) {
        pageNode.dataset.state = 'error';
        pageNode.innerHTML = '<span class="aria2-dot failed"></span>页面同步中断，保留上次数据';
    } else {
        pageNode.dataset.state = 'ok';
        pageNode.innerHTML = '<span class="aria2-dot connected"></span>页面已连接';
    }

    const monitor = dashboard?.monitor || {};
    if (!dashboard) {
        monitorNode.dataset.state = 'stale';
        monitorNode.innerHTML = '<span class="aria2-dot"></span>监控：等待数据';
    } else if (monitor.running) {
        monitorNode.dataset.state = 'ok';
        monitorNode.innerHTML = `<span class="aria2-dot connected"></span>监控运行中${monitor.last_heartbeat_at ? ` · 心跳 ${relativeTime(monitor.last_heartbeat_at)}` : ''}`;
    } else {
        monitorNode.dataset.state = 'error';
        monitorNode.innerHTML = '<span class="aria2-dot failed"></span>监控未运行';
    }

    if (!dashboard) {
        biliNode.dataset.state = 'stale';
        biliNode.innerHTML = '<span class="aria2-dot"></span>B站状态：等待检查';
        return;
    }
    if (dashboard.risk_notice) {
        biliNode.dataset.state = 'stale';
        biliNode.innerHTML = '<span class="aria2-dot connecting"></span>B站检查受限，退避中';
        return;
    }
    const lastSuccess = monitor.last_success_at;
    if (lastSuccess) {
        biliNode.dataset.state = 'ok';
        biliNode.innerHTML = `<span class="aria2-dot connected"></span>B站状态：${relativeTime(lastSuccess)}更新`;
    } else {
        biliNode.dataset.state = 'stale';
        biliNode.innerHTML = '<span class="aria2-dot connecting"></span>B站状态：尚未成功检查';
    }
}

// ==================== 渲染 ====================

function renderAll() {
    renderSidebar();
    renderDetail();
    renderRiskNotice();
    renderDiskHint();
}

function renderRiskNotice() {
    const node = document.getElementById('live-risk-notice');
    const text = liveState.dashboard?.risk_notice;
    node.hidden = !text;
    node.textContent = text ? `B 站限制了状态检查：${text} 期间显示的是最后一次成功结果，系统会自动重试并在恢复后降低退避等级。` : '';
}

function renderDiskHint() {
    const node = document.getElementById('live-disk-hint');
    const disk = liveState.dashboard?.disk;
    node.textContent = disk ? `磁盘余量 ${formatFileSize(disk.available_bytes)} / ${formatFileSize(disk.total_bytes)}` : '';
}

function renderSidebar() {
    const sources = liveState.dashboard?.sources || [];
    document.getElementById('live-source-count').textContent = String(sources.length);
    const emptyState = document.getElementById('live-empty-state');
    const detailContent = document.getElementById('live-detail-content');
    if (!sources.length && !liveState.selectedRoom) {
        emptyState.hidden = false;
        detailContent.hidden = true;
    } else {
        emptyState.hidden = true;
        detailContent.hidden = false;
    }
    const sessions = liveState.dashboard?.sessions || [];
    const container = document.getElementById('live-room-list');
    container.innerHTML = sources.length ? sources.map(source => {
        const [stateCls, stateText] = sourceState(source);
        const recording = sessions.some(item => item.room_id === source.room_id);
        const dotCls = recording ? 'recording' : stateCls === 'live' ? 'live' : ['risk', 'stale', 'unknown'].includes(stateCls) ? 'warn' : '';
        const face = source.face ? `<img class="live-room-avatar" src="/api/video/proxy-image?url=${encodeURIComponent(source.face)}" alt="" loading="lazy">`
            : `<div class="live-room-avatar">${escapeHtml((source.anchor_name || '直').slice(0, 1))}</div>`;
        return `
            <div class="live-room-item ${source.room_id === liveState.selectedRoom ? 'active' : ''}" data-room-id="${source.room_id}" role="button" tabindex="0">
                ${face}
                <div class="live-room-info">
                    <div class="live-room-name"><span>${escapeHtml(source.anchor_name || `UID ${source.uid}`)}</span><span class="live-room-dot ${dotCls}"></span></div>
                    <div class="live-room-meta">#${source.room_id} · ${escapeHtml(stateText)}${recording ? ' · 录制中' : source.auto_record_enabled ? ' · 自动录制' : ''}</div>
                </div>
            </div>`;
    }).join('') : '<p class="empty-hint">暂无关注房间</p>';
}

function selectedSource() {
    return (liveState.dashboard?.sources || []).find(item => item.room_id === liveState.selectedRoom) || null;
}

function selectedSession() {
    return (liveState.dashboard?.sessions || []).find(item => item.room_id === liveState.selectedRoom) || null;
}

function renderDetail() {
    const content = document.getElementById('live-detail-content');
    const source = selectedSource();
    if (!source) {
        if (!(liveState.dashboard?.sources || []).length && !liveState.selectedRoom) return;
        content.innerHTML = '<div class="live-empty-state"><i class="fa-solid fa-tower-broadcast"></i><p>请选择左侧房间</p></div>';
        return;
    }
    const session = selectedSession();
    const runtime = source.runtime || {};
    const [stateCls, stateText] = sourceState(source);
    const pending = liveState.pendingRooms.has(source.room_id);
    const cover = source.cover ? `<img class="live-cover-thumb" src="/api/video/proxy-image?url=${encodeURIComponent(source.cover)}" alt="" loading="lazy">` : '';

    let badges = badge(stateCls, stateText);
    if (session) badges += badge(videoStatusClass(session.status), `视频 ${videoStatusText(session.status)}`);
    if (session) badges += badge(interactionStateClass(session.interaction_capture_status), `互动 ${interactionStateText(session.interaction_capture_status)}`);

    const startStopButtons = session
        ? `<button class="btn btn-danger" data-action="live-stop" data-room-id="${source.room_id}" ${pending ? 'disabled' : ''}>${pending ? '处理中…' : '停止并合并'}</button>`
        : `<button class="btn btn-primary" data-action="live-start" data-room-id="${source.room_id}" ${runtime.live_status !== 1 || pending ? 'disabled' : ''}>${pending ? '处理中…' : '手动录制'}</button>`;

    let alerts = '';
    if (runtime.risk_limited) {
        alerts += `<div class="live-alert error"><i class="fa-solid fa-triangle-exclamation"></i><span>B 站当前限制状态检查：下方显示的是最后一次成功结果，不代表当前真实开播状态。${runtime.next_retry_at ? `预计 ${relativeTime(runtime.next_retry_at)}自动重试。` : '系统会自动重试。'}</span></div>`;
    }
    if (session?.interaction_capture_status === 'degraded') {
        alerts += `<div class="live-alert warn"><i class="fa-solid fa-circle-exclamation"></i><span>互动采集已降级：${escapeHtml(session.error_msg || '弹幕连接不可用')}。视频录制不受影响。</span></div>`;
    } else if (session?.error_msg) {
        alerts += `<div class="live-alert warn"><i class="fa-solid fa-circle-exclamation"></i><span>${escapeHtml(session.error_msg)}</span></div>`;
    }
    if (source.manual_stop_latched) {
        alerts += `<div class="live-alert warn"><i class="fa-solid fa-lock"></i><span>本场已手动停止，等待真正下播后才会解除，期间不会自动重新拉起。</span></div>`;
    }
    if (runtime.schedule_overrun) {
        alerts += `<div class="live-alert warn"><i class="fa-solid fa-clock"></i><span>已超过排期结束时间，当前策略是录制至下播。</span></div>`;
    }

    const scheduleText = scheduleSummary(source);
    const nextSchedule = runtime.next_schedule_at ? `下次自动开始：${relativeTime(runtime.next_schedule_at).replace('前', '')}${new Date(runtime.next_schedule_at).toLocaleString('zh-CN', { hour12: false })}` : '';
    // 详情区每轮刷新会重建，先记住当前互动筛选，重建后恢复并立即重绘已有事件
    const previousFilter = document.getElementById('live-event-filter')?.value || 'all';

    content.innerHTML = `
        <div class="live-detail-header">
            ${cover}
            <div class="live-detail-main">
                <div class="live-detail-title-row">
                    <span class="live-detail-title">${escapeHtml(source.title || '（未开播，暂无标题）')}</span>
                    ${badges}
                </div>
                <div class="live-detail-meta">
                    <span>${escapeHtml(source.anchor_name || `UID ${source.uid}`)}</span>
                    <span>房间 ${source.room_id}${source.short_id ? `（短号 ${source.short_id}）` : ''}</span>
                    <span>最近检查：${runtime.last_checked_at ? relativeTime(runtime.last_checked_at) : '尚未'}</span>
                    ${runtime.error ? `<span>检查异常：${escapeHtml(runtime.error)}</span>` : ''}
                </div>
                <div class="live-detail-actions">
                    ${startStopButtons}
                    <button class="btn btn-ghost" data-action="source-edit" data-room-id="${source.room_id}" ${pending ? 'disabled' : ''}>
                        <i class="fa-solid fa-sliders"></i> 设置
                    </button>
                    <button class="btn btn-ghost" data-action="source-delete" data-room-id="${source.room_id}" ${session || pending ? 'disabled' : ''} title="${session ? '录制中不可删除，请先停止' : ''}">
                        <i class="fa-solid fa-trash"></i> 删除源
                    </button>
                </div>
            </div>
        </div>
        ${alerts}
        <div class="blogger-status-display">
            <div class="blogger-status-row">
                <div class="status-item">
                    <span class="status-label"><i class="fa-solid fa-circle"></i> B站开播状态</span>
                    <span class="status-value ${stateCls === 'live' ? 'running' : ''}">${escapeHtml(stateText)}</span>
                </div>
                <div class="status-item">
                    <span class="status-label"><i class="fa-solid fa-record-vinyl"></i> 视频录制</span>
                    <span class="status-value ${session ? 'running' : ''}">${session ? escapeHtml(videoStatusText(session.status)) : '未在录制'}</span>
                </div>
                <div class="status-item">
                    <span class="status-label"><i class="fa-solid fa-comments"></i> 互动采集</span>
                    <span class="status-value">${session ? escapeHtml(interactionStateText(session.interaction_capture_status)) : captureModeText(source.capture_mode)}</span>
                </div>
                <div class="status-item">
                    <span class="status-label"><i class="fa-solid fa-clock"></i> 最近检查</span>
                    <span class="status-value">${runtime.last_checked_at ? escapeHtml(relativeTime(runtime.last_checked_at)) : '--'}</span>
                </div>
            </div>
            <div class="live-strategy-summary">
                <i class="fa-solid fa-calendar-week"></i>
                <span>自动录制：${source.auto_record_enabled ? '开' : '关'} · ${escapeHtml(scheduleText)}${nextSchedule ? ` · ${escapeHtml(nextSchedule)}` : ''} · 互动采集：${captureModeText(source.capture_mode)}</span>
            </div>
        </div>
        ${session ? recordingInfoMarkup(session) : ''}
        ${session && session.interaction_capture_status !== 'off' ? interactionMarkup(session) : ''}
    `;
    const filterNode = document.getElementById('live-event-filter');
    if (filterNode) filterNode.value = previousFilter;
    if (session && liveState.events.length) renderEvents();
}

function recordingInfoMarkup(session) {
    return `
        <div class="blogger-status-display">
            <div class="blogger-status-row">
                <div class="status-item">
                    <span class="status-label"><i class="fa-solid fa-hourglass-half"></i> 录制时长</span>
                    <span class="status-value live-duration" data-started-at="${escapeHtml(session.started_at)}">${formatDuration(session.duration_secs)}</span>
                </div>
                <div class="status-item">
                    <span class="status-label"><i class="fa-solid fa-file"></i> 文件大小</span>
                    <span class="status-value">${formatFileSize(session.file_size)}</span>
                </div>
                <div class="status-item">
                    <span class="status-label"><i class="fa-solid fa-bolt"></i> 触发方式</span>
                    <span class="status-value">${session.trigger === 'auto' ? '自动' : '手动'}</span>
                </div>
                ${session.stream_quality ? `<div class="status-item"><span class="status-label"><i class="fa-solid fa-film"></i> 清晰度</span><span class="status-value">${escapeHtml(qualityText(session.stream_quality))}</span></div>` : ''}
                ${session.dropped_event_count ? `<div class="status-item"><span class="status-label"><i class="fa-solid fa-triangle-exclamation"></i> 丢失事件</span><span class="status-value paused">${session.dropped_event_count}</span></div>` : ''}
            </div>
        </div>`;
}

function interactionMarkup(session) {
    const stats = [
        ['弹幕', session.danmaku_count || 0],
        ['互动人数', session.unique_user_count || 0, '可识别互动事件的去重 UID'],
        ['免费礼物', session.free_gift_count || 0],
        ['付费礼物', session.paid_gift_count || 0],
        ['SC', session.sc_count || 0],
        ['上舰', session.guard_count || 0],
        ['累计看过', session.peak_watched || 0, 'B 站累计口径，非在线峰值'],
        ['估算付费价值', `¥${Number(session.estimated_paid_value || 0).toFixed(2)}`, '非结算估算值'],
    ];
    return `
        <div class="live-section-title">
            <span><i class="fa-solid fa-comments"></i> 实时互动（最近 100 条）</span>
            <span class="form-note">观看为 B 站累计"看过"口径；互动人数按可识别 UID 估算；金额为非结算估算值。</span>
        </div>
        <div class="live-stats-grid">
            ${stats.map(([label, value, title]) => `<div class="live-stats-cell" ${title ? `title="${escapeHtml(title)}"` : ''}><strong>${escapeHtml(String(value))}</strong><span>${label}</span></div>`).join('')}
        </div>
        <div id="live-sc-pins" class="live-sc-pins"></div>
        <div class="live-toolbar-row">
            <select id="live-event-filter" class="form-control" aria-label="互动类型筛选">
                <option value="all">全部互动</option><option value="danmaku">弹幕</option><option value="gift">礼物</option>
                <option value="super_chat">SC</option><option value="guard">上舰</option><option value="link_mic_pk">连麦 / PK</option>
            </select>
            <span id="live-events-status" class="live-events-status" role="status">实时互动状态：等待检查</span>
        </div>
        <div id="live-heat-bar" class="live-heat-bar" aria-label="30 秒弹幕热度"></div>
        <div id="live-event-timeline" class="live-event-timeline"><p class="empty-hint">录制开始后在这里显示互动</p></div>
    `;
}

// ==================== 录制任务看板（子 tab） ====================

function renderBoard() {
    const sessions = liveState.dashboard?.sessions || [];
    const mergeJobs = liveState.dashboard?.merge_jobs || [];
    const recovery = liveState.dashboard?.recovery || [];
    document.getElementById('live-count-recording').textContent = String(sessions.length);
    document.getElementById('live-count-history').textContent = String(liveState.history.length);
    document.getElementById('live-count-attention').textContent = String(mergeJobs.filter(job => ['queued', 'running', 'cancelling'].includes(job.status)).length + recovery.length);
    renderRecordingBoard(sessions);
    renderHistoryBoard(liveState.history);
    renderAttentionBoard(mergeJobs, recovery);
}

function switchBoardTab(name) {
    liveState.boardTab = name;
    document.querySelectorAll('#live-board-tabs .board-sub-tab').forEach(node => {
        node.classList.toggle('active', node.dataset.liveBoard === name);
    });
    document.querySelectorAll('.live-board-panel').forEach(node => {
        node.classList.toggle('active', node.id === `live-panel-${name}`);
    });
}

function renderRecordingBoard(sessions) {
    const node = document.getElementById('live-recording-list');
    if (!sessions.length) {
        node.innerHTML = '<p class="empty-hint">暂无录制中的任务</p>';
        return;
    }
    node.innerHTML = [...sessions].sort((a, b) => String(a.started_at).localeCompare(String(b.started_at))).map(session => {
        const pending = liveState.pendingRooms.has(session.room_id);
        const interaction = session.interaction_capture_status || 'off';
        return `
        <div class="live-recording-row">
            <div class="live-recording-main">
                <span class="live-recording-title">${escapeHtml(session.title || `房间 ${session.room_id}`)}
                    ${badge(videoStatusClass(session.status), videoStatusText(session.status))}
                    ${badge(interactionStateClass(interaction), `互动 ${interactionStateText(interaction)}`)}
                </span>
                <span class="live-recording-meta">
                    <span>#${session.room_id}</span>
                    <span>时长 <b class="live-duration" data-started-at="${escapeHtml(session.started_at)}">${formatDuration(session.duration_secs)}</b></span>
                    <span><b>${formatFileSize(session.file_size)}</b></span>
                    <span>${session.trigger === 'auto' ? '自动触发' : '手动触发'}</span>
                    ${session.dropped_event_count ? `<span>丢失 ${session.dropped_event_count} 条互动</span>` : ''}
                </span>
                ${session.error_msg ? `<span class="live-recording-note">${escapeHtml(session.error_msg)}</span>` : ''}
            </div>
            <div class="live-recording-actions">
                <button class="btn btn-sm btn-ghost" data-action="interaction-select" data-room-id="${session.room_id}">查看详情</button>
                <button class="btn btn-sm btn-danger" data-action="live-stop" data-room-id="${session.room_id}" ${pending ? 'disabled' : ''}>${pending ? '处理中…' : '停止并合并'}</button>
            </div>
        </div>`;
    }).join('');
}

function renderHistoryBoard(items) {
    const node = document.getElementById('live-history-list');
    if (!items.length) {
        node.innerHTML = '<p class="empty-hint">暂无已结束的录制</p>';
        return;
    }
    node.innerHTML = items.map(item => {
        const reason = stopReasonText(item.stop_reason);
        const statusCls = item.status === 'failed' ? 'failed' : item.status === 'completed' ? 'completed' : '';
        const failedInfo = item.status === 'failed'
            ? `${badge('failed', item.is_recoverable ? '失败可恢复' : '失败')}`
            : badge(statusCls, reason || videoStatusText(item.status));
        return `
        <div class="live-recording-row">
            <div class="live-recording-main">
                <span class="live-recording-title">${escapeHtml(item.title || `房间 ${item.room_id}`)} ${failedInfo}</span>
                <span class="live-recording-meta">
                    <span>#${item.room_id}</span>
                    <span>${escapeHtml(item.started_at || '').replace('T', ' ').slice(0, 16)}</span>
                    <span>${formatDuration(item.duration)}</span>
                    <span>${item.file_size ? formatFileSize(item.file_size) : '--'}</span>
                    ${item.error_msg ? `<span>${escapeHtml(item.error_msg)}</span>` : ''}
                </span>
            </div>
            <div class="live-recording-actions">
                ${item.is_recoverable ? `<button class="btn btn-sm btn-primary" data-action="history-merge" data-recording-id="${item.id}">重试合并</button>` : ''}
                ${item.has_output ? `<button class="btn btn-sm btn-ghost" data-action="history-open" data-recording-id="${item.id}">打开目录</button>` : ''}
            </div>
        </div>`;
    }).join('');
}

function renderAttentionBoard(jobs, recovery) {
    const node = document.getElementById('live-attention-list');
    const activeJobs = jobs.filter(job => ['queued', 'running', 'cancelling'].includes(job.status));
    if (!activeJobs.length && !recovery.length) {
        node.innerHTML = '<p class="empty-hint">暂无需要处理的项目</p>';
        return;
    }
    const jobRows = activeJobs.map(job => `
        <div class="live-recording-row">
            <div class="live-recording-main">
                <span class="live-recording-title">后台合并 · 录制 #${job.recording_id} ${badge('starting', job.status === 'cancelling' ? '取消中' : '进行中')}</span>
                <span class="live-recording-meta"><span>任务 ${escapeHtml(job.id.slice(0, 8))}</span><span>源分段 ${job.source_segment_count || '-'} 个</span></span>
            </div>
            <div class="live-recording-actions">
                <progress class="live-progress" max="100" value="${job.progress || 0}"></progress>
                <span style="font-size:12px">${job.progress || 0}%</span>
                <button class="btn btn-sm btn-ghost" data-action="merge-cancel" data-job-id="${escapeHtml(job.id)}" ${job.cancel_requested ? 'disabled' : ''}>取消</button>
            </div>
        </div>`);
    const recoveryRows = recovery.map(item => `
        <div class="live-recording-row">
            <div class="live-recording-main">
                <span class="live-recording-title">#${item.recording_id} ${escapeHtml(item.title || '')} ${badge('failed', '失败可恢复')}</span>
                <span class="live-recording-meta"><span>保留源分段 ${item.segment_count} 个</span><span>${escapeHtml(item.error_msg || '可恢复')}</span></span>
            </div>
            <div class="live-recording-actions">
                <button class="btn btn-sm btn-primary" data-action="history-merge" data-recording-id="${item.recording_id}">重试合并</button>
                ${item.has_output ? `<button class="btn btn-sm btn-ghost" data-action="history-open" data-recording-id="${item.recording_id}">打开目录</button>` : ''}
            </div>
        </div>`);
    node.innerHTML = [...jobRows, ...recoveryRows].join('');
}

// ==================== 实时互动轮询 ====================

async function pollEvents() {
    const session = selectedSession();
    if (!liveState.selectedRoom || !session || liveState.eventsInFlight
        || document.visibilityState === 'hidden' || !liveState.liveTabActive) return;
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
        const status = document.getElementById('live-events-status');
        if (status) {
            status.dataset.state = 'ok';
            status.textContent = '实时互动状态：正常';
        }
    } catch (error) {
        liveState.eventFailedAt = Date.now();
        const status = document.getElementById('live-events-status');
        if (status) {
            status.dataset.state = 'error';
            status.textContent = `实时互动状态：暂时不可用（${error.message}），下方保留最近数据`;
        }
        console.error('[live] 轮询互动事件失败：', error);
    } finally {
        liveState.eventsInFlight = false;
    }
}

function renderEvents() {
    const filter = document.getElementById('live-event-filter')?.value || 'all';
    const timeline = document.getElementById('live-event-timeline');
    if (!timeline) return;
    const events = mergeLiveEvents(liveState.events).filter(event => filter === 'all' || event.event_type === filter);
    timeline.innerHTML = events.length
        ? events.slice().reverse().map(eventRow).join('')
        : '<p class="empty-hint">暂无符合条件的互动</p>';
    const pins = liveState.events.filter(event => event.event_type === 'super_chat').slice(-3).reverse();
    const pinsNode = document.getElementById('live-sc-pins');
    if (pinsNode) {
        pinsNode.innerHTML = pins.map(event => `
            <div class="live-sc-card">
                <strong>${escapeHtml(event.data?.uname || 'SC')}</strong>
                <span>¥${event.data?.price || 0}</span>
                <p>${escapeHtml(event.data?.message || '')}</p>
            </div>
        `).join('');
    }
    renderHeatBar();
}

function eventRow(event) {
    const data = event.data || {};
    const type = event.event_type;
    const typeLabel = { danmaku: '弹幕', gift: '礼物', super_chat: 'SC', guard: '上舰', link_mic_pk: '连麦', interact: '进场' }[type] || '其他';
    const text = type === 'danmaku'
        ? data.text
        : type === 'gift'
            ? `${data.gift_name || '礼物'} ×${data.num || 1}`
            : type === 'super_chat'
                ? `SC ¥${data.price || 0}：${data.message || ''}`
                : type === 'guard'
                    ? `上舰 等级 ${data.guard_level || '-'}`
                    : type === 'interact'
                        ? '进入直播间'
                        : event.cmd;
    return `
        <div class="live-event-row live-event-${escapeHtml(type)}" data-time-ms="${event.media_time_ms || 0}">
            <time>${formatMediaTime(event.media_time_ms)}</time>
            <span class="live-event-user">${escapeHtml(data.uname || '')}</span>
            <span class="live-event-text">${escapeHtml(String(text || ''))}</span>
            ${event.merged_count > 1 ? `<em title="合并了 ${event.merged_count} 个连续事件">×${event.merged_count}</em>` : `<span class="live-event-type">${typeLabel}</span>`}
        </div>
    `;
}

function renderHeatBar() {
    const node = document.getElementById('live-heat-bar');
    if (!node) return;
    const buckets = [];
    liveState.events.filter(event => event.event_type === 'danmaku').forEach(event => {
        const index = Math.floor((event.media_time_ms || 0) / 30000);
        buckets[index] = (buckets[index] || 0) + 1;
    });
    const max = Math.max(1, ...buckets.filter(Boolean));
    node.innerHTML = buckets.map((count = 0, index) => `
        <button type="button" class="${count / max > 0.7 ? 'hot' : ''}" title="${formatMediaTime(index * 30000)} · ${count} 条弹幕" style="--heat:${count / max}" data-time-ms="${index * 30000}"><i></i></button>
    `).join('');
}

function selectRoom(roomId, options = {}) {
    if (liveState.selectedRoom === roomId && !options.force) return;
    liveState.selectedRoom = roomId;
    liveState.afterSeq = 0;
    liveState.events = [];
    liveState.eventFailedAt = 0;
    renderSidebar();
    renderDetail();
    renderEvents();
    if (!options.silent) pollEvents();
}

// ==================== 操作 ====================

async function startRecording(roomId) {
    if (liveState.pendingRooms.has(roomId)) return;
    liveState.pendingRooms.add(roomId);
    renderDetail();
    renderBoard();
    try {
        await apiPost('/api/live/start', { room_id: roomId });
        showToast('录制已开始', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`开始录制失败：${error.message}`, 'error');
    } finally {
        liveState.pendingRooms.delete(roomId);
        renderDetail();
        renderBoard();
    }
}

async function stopRecording(roomId) {
    if (liveState.pendingRooms.has(roomId)) return;
    const confirmed = await confirmDialog(
        '停止会先收尾互动数据，再合并并校验录制文件；该过程可能需要一些时间，期间本场不会自动重新拉起。继续吗？',
        { title: '停止录制', okText: '停止并合并', danger: true },
    );
    if (!confirmed) return;
    liveState.pendingRooms.add(roomId);
    renderDetail();
    renderBoard();
    try {
        const response = await apiPost('/api/live/stop', { room_id: roomId });
        const operationId = response.data?.operation_id;
        showToast(operationId ? `停止请求已接受，后台任务正在收尾（${operationId.slice(0, 8)}）` : '录制已停止', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`停止录制失败：${error.message}`, 'error');
    } finally {
        liveState.pendingRooms.delete(roomId);
        renderDetail();
        renderBoard();
    }
}

async function deleteSource(roomId) {
    const confirmed = await confirmDialog(
        '确定删除这个直播源吗？仅取消关注与自动录制策略，已录制的文件不会被删除，可在"最近录制"中找到。',
        { title: '删除直播源', okText: '删除', danger: true },
    );
    if (!confirmed) return;
    try {
        await apiPost('/api/live/source/delete', { room_id: roomId });
        showToast('直播源已删除', 'success');
        if (liveState.selectedRoom === roomId) liveState.selectedRoom = 0;
        await refreshDashboard(true);
    } catch (error) {
        showToast(`删除失败：${error.message}`, 'error');
    }
}

// ==================== 添加直播源弹窗 ====================

function openAddModal() {
    document.getElementById('live-add-input').value = '';
    document.getElementById('live-add-modal').classList.add('active');
    document.getElementById('live-add-input').focus();
}

function closeAddModal() {
    document.getElementById('live-add-modal').classList.remove('active');
}

function parseRoomTokens(raw) {
    const tokens = raw.split(/[\s,，、;；]+/).filter(Boolean);
    const roomIds = new Set();
    for (const token of tokens) {
        const match = token.match(/live\.bilibili\.com\/(?:h5\/)?(\d+)/i) || token.match(/^(\d+)$/);
        if (match) roomIds.add(Number(match[1]));
    }
    return [...roomIds].filter(id => Number.isSafeInteger(id) && id > 0);
}

async function submitAdd() {
    if (liveState.addPending) return;
    const input = document.getElementById('live-add-input');
    const roomIds = parseRoomTokens(input.value || '');
    if (!roomIds.length) {
        showToast('请输入有效的房间号或 live.bilibili.com 链接', 'warning');
        return;
    }
    liveState.addPending = true;
    const confirmBtn = document.getElementById('live-add-confirm-btn');
    confirmBtn.disabled = true;
    confirmBtn.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> 添加中…';
    const results = { ok: [], fail: [] };
    try {
        for (const roomId of roomIds) {
            try {
                await apiPost('/api/live/source/add', { room_id: roomId, auto_record_enabled: false, capture_mode: 'standard' });
                results.ok.push(roomId);
            } catch (error) {
                results.fail.push(`${roomId}：${error.message}`);
            }
        }
        if (results.ok.length) {
            showToast(`成功添加 ${results.ok.length} 个直播源（自动录制默认关闭）`, 'success');
            selectRoom(results.ok[0], { force: true });
            closeAddModal();
            await refreshDashboard(true);
        }
        if (results.fail.length) {
            showToast(`添加失败 ${results.fail.length} 个：${results.fail.join('；')}`, 'error');
        }
    } finally {
        liveState.addPending = false;
        confirmBtn.disabled = false;
        confirmBtn.innerHTML = '<i class="fa-solid fa-check"></i> 查询并添加';
    }
}

// ==================== 直播源设置弹窗 ====================

function scheduleRows(key) {
    return [0, 1].map(index => ({
        start: document.getElementById(`live-schedule-${key}-${index}-start`),
        end: document.getElementById(`live-schedule-${key}-${index}-end`),
    }));
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
                return `${dayLabel(day)}: 时间格式应为 HH:MM-HH:MM，且开始和结束不能相同`;
            }
            const begin = dayIndex * 1440 + beginValue;
            let finish = dayIndex * 1440 + endValue;
            if (finish <= begin) finish += 1440;
            [-10080, 0, 10080].forEach(offset => intervals.push({ begin: begin + offset, finish: finish + offset }));
        }
    }
    intervals.sort((left, right) => left.begin - right.begin);
    for (let index = 1; index < intervals.length; index += 1) {
        if (intervals[index - 1].finish > intervals[index].begin) return '排期窗口存在重叠，请调整后再保存';
    }
    return '';
}

function dayLabel(key) {
    return weekdays.find(([value]) => value === key)?.[1] || key;
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

function toggleScheduleEditor() {
    const allDay = document.getElementById('live-source-all-day').checked;
    document.querySelectorAll('#live-weekly-schedule input[type="time"]').forEach(input => {
        input.disabled = allDay;
    });
    updateScheduleValidation();
}

function updateScheduleValidation() {
    const errorNode = document.getElementById('live-schedule-error');
    if (!errorNode) return;
    const allDay = document.getElementById('live-source-all-day')?.checked;
    if (allDay) {
        errorNode.textContent = '';
        return;
    }
    errorNode.textContent = validateScheduleStrict(readScheduleFromEditor());
}

function buildScheduleEditor() {
    const node = document.getElementById('live-weekly-schedule');
    node.innerHTML = weekdays.map(([key, label]) => `
        <div class="live-schedule-day" data-day="${key}">
            <span>${label}</span>
            ${[0, 1].map(index => `
                <div class="live-schedule-window">
                    <input type="time" id="live-schedule-${key}-${index}-start" aria-label="${label} 时段 ${index + 1} 开始">
                    <span aria-hidden="true">–</span>
                    <input type="time" id="live-schedule-${key}-${index}-end" aria-label="${label} 时段 ${index + 1} 结束">
                    <button type="button" class="live-schedule-clear" data-day="${key}" data-index="${index}" title="清空该时段" aria-label="清空${label}时段${index + 1}">
                        <i class="fa-solid fa-xmark"></i>
                    </button>
                </div>`).join('')}
        </div>`).join('');
}

function openSettingsModal(roomId) {
    const source = (liveState.dashboard?.sources || []).find(item => item.room_id === roomId);
    if (!source) return;
    document.getElementById('live-settings-room-id').textContent = roomId;
    document.getElementById('live-source-room-id').value = roomId;
    document.getElementById('live-source-auto').checked = source.auto_record_enabled;
    document.getElementById('live-source-mode').value = source.capture_mode || 'standard';
    document.getElementById('live-source-all-day').checked = source.schedule_all_day;
    writeScheduleToEditor(source.weekly_schedule || {});
    toggleScheduleEditor();
    const dashboard = liveState.dashboard;
    document.getElementById('live-tz-note').textContent = dashboard?.server_timezone
        ? `排期按服务器时区生效：${dashboard.server_timezone} · 服务器当前时间 ${new Date(dashboard.server_now).toLocaleTimeString('zh-CN', { hour12: false })}`
        : '排期按服务器时区生效';
    document.getElementById('live-source-modal').classList.add('active');
}

function closeSettingsModal() {
    document.getElementById('live-source-modal').classList.remove('active');
}

async function saveSource() {
    const roomId = Number(document.getElementById('live-source-room-id').value);
    const allDay = document.getElementById('live-source-all-day').checked;
    const autoEnabled = document.getElementById('live-source-auto').checked;
    const schedule = readScheduleFromEditor();
    if (!allDay) {
        const validationError = validateScheduleStrict(schedule);
        if (validationError) {
            showToast(validationError, 'warning');
            return;
        }
        if (autoEnabled && !Object.values(schedule).some(windows => windows.length)) {
            const confirmed = await confirmDialog(
                '自动录制已开启，但排期为空，这样永远不会自动开始。要改为"全天允许"吗？',
                { title: '排期为空', okText: '改为全天', danger: false },
            );
            if (!confirmed) return;
            document.getElementById('live-source-all-day').checked = true;
        }
    }
    const finalAllDay = document.getElementById('live-source-all-day').checked;
    try {
        await apiPost('/api/live/source/update', {
            room_id: roomId,
            auto_record_enabled: autoEnabled,
            capture_mode: document.getElementById('live-source-mode').value,
            clear_schedule: finalAllDay,
            weekly_schedule: finalAllDay ? null : schedule,
        });
        closeSettingsModal();
        showToast('直播源设置已保存', 'success');
        await refreshDashboard(true);
    } catch (error) {
        showToast(`保存失败：${error.message}`, 'error');
    }
}

// ==================== 初始化 ====================

function tickUi() {
    document.querySelectorAll('.live-duration[data-started-at]').forEach(node => {
        const start = Date.parse(node.dataset.startedAt);
        if (!Number.isNaN(start) && start > 0) {
            node.textContent = formatDuration((Date.now() - start) / 1000);
        }
    });
    if (liveState.dashboardFailedAt && Date.now() - liveState.dashboardFailedAt > 60000) {
        setSyncStates();
    }
}

function initLiveTab() {
    buildScheduleEditor();

    document.getElementById('live-show-add-btn')?.addEventListener('click', openAddModal);
    document.getElementById('live-add-close-btn')?.addEventListener('click', closeAddModal);
    document.getElementById('live-add-cancel-btn')?.addEventListener('click', closeAddModal);
    document.getElementById('live-add-confirm-btn')?.addEventListener('click', submitAdd);
    document.getElementById('live-add-input')?.addEventListener('keydown', event => {
        if (event.key === 'Enter' && !event.shiftKey) {
            event.preventDefault();
            submitAdd();
        }
    });
    document.getElementById('live-refresh-btn')?.addEventListener('click', () => refreshDashboard(false));
    document.getElementById('live-refresh-list-btn')?.addEventListener('click', () => refreshDashboard(false));
    document.getElementById('live-settings-close-btn')?.addEventListener('click', closeSettingsModal);
    document.getElementById('live-settings-cancel-btn')?.addEventListener('click', closeSettingsModal);
    document.getElementById('live-source-save')?.addEventListener('click', saveSource);
    document.getElementById('live-source-all-day')?.addEventListener('change', toggleScheduleEditor);
    document.getElementById('live-weekly-schedule')?.addEventListener('input', event => {
        if (event.target.matches('input[type="time"]')) updateScheduleValidation();
    });
    document.getElementById('live-weekly-schedule')?.addEventListener('click', event => {
        const clearBtn = event.target.closest('.live-schedule-clear');
        if (!clearBtn) return;
        scheduleRows(clearBtn.dataset.day)[Number(clearBtn.dataset.index)].start.value = '';
        scheduleRows(clearBtn.dataset.day)[Number(clearBtn.dataset.index)].end.value = '';
        updateScheduleValidation();
    });

    document.getElementById('live-room-list')?.addEventListener('click', event => {
        const item = event.target.closest('.live-room-item');
        if (item) selectRoom(Number(item.dataset.roomId));
    });
    document.getElementById('live-room-list')?.addEventListener('keydown', event => {
        if (event.key !== 'Enter' && event.key !== ' ') return;
        const item = event.target.closest('.live-room-item');
        if (item) {
            event.preventDefault();
            selectRoom(Number(item.dataset.roomId));
        }
    });

    document.getElementById('live-board-tabs')?.addEventListener('click', event => {
        const tab = event.target.closest('[data-live-board]');
        if (tab) switchBoardTab(tab.dataset.liveBoard);
    });

    // 详情面板、录制看板与需要处理区的操作统一走事件委托
    document.getElementById('tab-live')?.addEventListener('click', event => {
        const button = event.target.closest('[data-action]');
        if (!button || button.disabled) return;
        const roomId = Number(button.dataset.roomId);
        const action = button.dataset.action;
        if (action === 'live-start') startRecording(roomId);
        else if (action === 'live-stop') stopRecording(roomId);
        else if (action === 'source-edit') openSettingsModal(roomId);
        else if (action === 'source-delete') deleteSource(roomId);
        else if (action === 'interaction-select') {
            selectRoom(roomId);
            document.getElementById('live-detail-panel')?.scrollIntoView({ behavior: 'smooth', block: 'start' });
        } else if (action === 'history-merge') {
            apiPost(`/api/live/history/${button.dataset.recordingId}/merge`, {})
                .then(() => refreshDashboard(true))
                .catch(error => showToast(`合并任务创建失败：${error.message}`, 'error'));
        } else if (action === 'merge-cancel') {
            apiPost(`/api/live/merge/${button.dataset.jobId}/cancel`, {})
                .then(() => refreshDashboard(true))
                .catch(error => showToast(`取消合并失败：${error.message}`, 'error'));
        } else if (action === 'history-open') {
            apiPost(`/api/live/history/${button.dataset.recordingId}/open-directory`, {})
                .catch(error => showToast(`打开目录失败：${error.message}`, 'error'));
        }
    });

    // 互动筛选变化只重绘事件区
    document.getElementById('tab-live')?.addEventListener('change', event => {
        if (event.target.id === 'live-event-filter') renderEvents();
    });

    // 热度条点击跳转到对应时间附近的事件
    document.getElementById('tab-live')?.addEventListener('click', event => {
        const heatButton = event.target.closest('.live-heat-bar button[data-time-ms]');
        if (!heatButton) return;
        const bucketStart = Number(heatButton.dataset.timeMs);
        const rows = [...document.querySelectorAll('.live-event-row[data-time-ms]')];
        const target = rows.find(row => Math.abs(Number(row.dataset.timeMs) - bucketStart) < 30000);
        target?.scrollIntoView({ behavior: 'smooth', block: 'center' });
    });

    // 追踪当前是否停留在直播 Tab：切走时停止互动轮询，切回时立即刷新
    document.querySelectorAll('.nav-tab').forEach(tab => {
        tab.addEventListener('click', () => {
            liveState.liveTabActive = tab.dataset.tab === 'live';
            if (liveState.liveTabActive) {
                refreshDashboard(true);
                pollEvents();
            }
        });
    });

    document.addEventListener('visibilitychange', () => {
        if (document.visibilityState !== 'hidden' && liveState.liveTabActive) {
            refreshDashboard(true);
            pollEvents();
        }
    });

    refreshDashboard(true);
    window.setInterval(() => {
        if (liveState.liveTabActive) refreshDashboard(true);
    }, 30000);
    window.setInterval(pollEvents, 2000);
    window.setInterval(tickUi, 1000);
}

if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initLiveTab);
} else {
    initLiveTab();
}

window.refreshDashboard = refreshDashboard;
