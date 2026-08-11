import { clampPercent, escapeHtml, formatFileSize, formatSpeed } from './utils.js';
import { renderSidecarIcons, stateDotClass, stateLabel, formatDuration, formatTimestamp, formatViewCount } from './history.js';
import { loadBvidLogs } from './media-actions.js';

export function renderDrawerContent(video, bvid) {
    const bodyEl = document.getElementById('drawer-body');
    if (!bodyEl) return;
    // 属性位插值统一用转义后的 bvid，防止含引号时发生属性逃逸
    const safeBvid = escapeHtml(bvid || '');

    // 统一字段命名（兼容 history/list 与 download/status 两种来源）
    const state = video.state || video.status || 'completed';
    const task = video.task || {};
    const sidecar = video.sidecar || {};
    const blogger = video.blogger || (video.blogger_uid ? { uid: video.blogger_uid } : null) || null;
    const artifactSource = video.source === 'manual' ? 'manual' : 'auto';

    // 封面统一走 /api/cover/{bvid}（本地优先 + 兜底下载）
    const coverUrl = `/api/cover/${encodeURIComponent(bvid)}`;
    const durationStr = video.duration ? formatDuration(Number(video.duration)) : '';
    const pubDate = video.pub_date || (video.pub_timestamp ? formatTimestamp(Number(video.pub_timestamp)) : '')
        || (video.pubdate ? video.pubdate : '');
    const viewStr = (video.view !== undefined && video.view !== null) ? formatViewCount(Number(video.view)) : '--';

    // 状态点 class + 文案（复用看板逻辑）
    const stateDot = stateDotClass(state, video);
    const stateLabelStr = stateLabel(state, video);

    // 重投提示
    const reuploadOf = video.reupload_of || '';
    const reuploadHtml = reuploadOf ? `
        <div class="drawer-reupload-hint">
            <i class="fa-solid fa-exclamation-triangle"></i>
            可能是 <a href="#" data-action="open-video" data-bvid="${escapeHtml(reuploadOf)}">${escapeHtml(reuploadOf)}</a> 的重传
        </div>
    ` : '';

    // 路径展示由后端统一决定；内部相对路径只用于安全打开目录。
    const filePathHtml = video.file_path || video.relative_path ? `
        <div class="drawer-file-path" title="${escapeHtml(video.file_path || '路径已隐藏')}">
            <i class="fa-solid fa-file-video"></i>
            <span>${escapeHtml(video.file_path || '路径已隐藏')}</span>
            ${video.file_path ? `<button class="btn btn-sm btn-ghost copy-path-btn" data-copy-path="${escapeHtml(video.file_path)}" title="复制路径"><i class="fa-solid fa-copy"></i></button>` : ''}
            ${video.relative_path ? `<button class="btn btn-sm btn-ghost" data-action="open-history-directory" data-bvid="${safeBvid}" data-path="${escapeHtml(video.relative_path)}" title="打开文件所在目录"><i class="fa-solid fa-folder-open"></i></button>` : ''}
        </div>
    ` : '';

    // MD5 信息
    const md5Html = video.md5 ? `
        <div class="drawer-info-item">
            <span class="drawer-info-label">MD5</span>
            <span class="drawer-info-value drawer-md5" title="${escapeHtml(video.md5)}">${escapeHtml(video.md5.slice(0, 16))}...</span>
            ${video.md5_last_checked_at ? `<span class="drawer-info-sub">校验于 ${new Date(video.md5_last_checked_at).toLocaleString()}</span>` : ''}
        </div>
    ` : '';

    // 进度条（活跃状态显示：下载中 / 等待 / 已暂停）
    const isActive = ['downloading', 'pending', 'paused'].includes(task.status);
    const isPaused = task.status === 'paused';
    const progressHtml = isActive ? `
        <div class="drawer-progress">
                <progress class="drawer-progress-bar" max="100" value="${clampPercent(task.progress_percent)}"></progress>
        </div>
        <div class="drawer-progress-text">
            <span>${isPaused ? `已暂停 ${clampPercent(task.progress_percent)}%` : `${clampPercent(task.progress_percent)}%`}</span>
            ${!isPaused && task.speed ? `<span>${formatSpeed(task.speed)}</span>` : ''}
            ${task.downloaded_size && task.total_size ? `<span>${formatFileSize(task.downloaded_size)} / ${formatFileSize(task.total_size)}</span>` : ''}
        </div>
    ` : '';

    // 主操作按钮：按 task.status / state 决定
    // 重新下载仅在 state in (failed, removed, pay_blocked) 时显示
    const canRetry = ['failed', 'removed', 'pay_blocked'].includes(state);
    const taskId = task.task_id || '';
    const mainActionHtml = (task.status === 'downloading' || task.status === 'pending') ? `
        <button class="drawer-btn drawer-btn-primary" disabled>
            <i class="fa-solid fa-spinner fa-spin"></i> 下载中 ${clampPercent(task.progress_percent)}%
        </button>
    ` : isPaused ? `
        <button class="drawer-btn drawer-btn-primary" disabled>
            <i class="fa-solid fa-pause"></i> 已暂停 ${clampPercent(task.progress_percent)}%
        </button>
    ` : state === 'completed' || state === 'tampered' ? `
        <button class="drawer-btn drawer-btn-success" disabled>
            <i class="fa-solid fa-check-circle"></i> 已完成
        </button>
    ` : canRetry ? `
        <button class="drawer-btn drawer-btn-primary" data-action="retry-video" data-bvid="${safeBvid}">
            <i class="fa-solid fa-redo"></i> 重新下载
        </button>
    ` : `
        <button class="drawer-btn drawer-btn-primary" data-action="start-video" data-bvid="${safeBvid}">
            <i class="fa-solid fa-download"></i> 开始下载
        </button>
    `;

    // 暂停 / 恢复按钮：仅活跃任务且后端返回了 task_id 时显示
    const pauseResumeBtnHtml = taskId && (task.status === 'downloading' || task.status === 'pending' || task.status === 'paused') ? `
        <button class="drawer-btn drawer-btn-ghost" data-action="${isPaused ? 'resume-download' : 'pause-download'}" data-task-id="${escapeHtml(String(taskId))}">
            <i class="fa-solid ${isPaused ? 'fa-play' : 'fa-pause'}"></i> ${isPaused ? '恢复下载' : '暂停下载'}
        </button>
    ` : '';

    // 删除按钮（所有状态都可删，带二次确认）
    const deleteBtnHtml = `
        <button class="drawer-btn drawer-btn-danger" data-action="delete-video" data-bvid="${safeBvid}">
            <i class="fa-solid fa-trash"></i> 删除记录
        </button>
    `;

    // 充电提示
    const payNoteHtml = (state === 'pay_blocked' && video.pay_note) ? `
        <div class="drawer-pay-note">
            <i class="fa-solid fa-coins"></i>
            ${video.pay_note.endsWith('_paid') ? '充电专属（当前账号可下载）' : '充电专属（当前账号不可下载）'}
            <span class="drawer-info-sub">${escapeHtml(video.pay_note)}</span>
        </div>
    ` : '';

    // 博主信息
    const bloggerHtml = blogger ? `
        <div class="drawer-blogger">
            ${blogger.face ? `<img src="/api/video/proxy-image?url=${encodeURIComponent(blogger.face)}" class="drawer-blogger-avatar" data-image-error="hide">` : '<div class="drawer-blogger-avatar blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>'}
            <div class="drawer-blogger-info">
                <div class="drawer-blogger-name">${escapeHtml(blogger.name || blogger.uid || '--')}</div>
                <div class="drawer-blogger-uid">UID: ${escapeHtml(blogger.uid || '--')}</div>
            </div>
        </div>
    ` : '';

    bodyEl.innerHTML = `
        <div class="drawer-preview">
            <img src="${coverUrl}" alt="" data-image-error="thumb-fallback">
            ${durationStr ? `<span class="drawer-preview-badge">${durationStr}</span>` : ''}
            <span class="drawer-state-badge state-${stateDot}">${escapeHtml(stateLabelStr)}</span>
        </div>

        ${reuploadHtml}

        <div class="drawer-info-row">
            <div class="drawer-info-item">
                <span class="drawer-info-label">发布时间</span>
                <span class="drawer-info-value">${escapeHtml(pubDate) || '--'}</span>
            </div>
            <div class="drawer-info-item">
                <span class="drawer-info-label">播放量</span>
                <span class="drawer-info-value">${viewStr}</span>
            </div>
            <div class="drawer-info-item">
                <span class="drawer-info-label">下载时间</span>
                <span class="drawer-info-value">${escapeHtml(video.download_time || '') || '--'}</span>
            </div>
            ${md5Html}
        </div>

        ${bloggerHtml}
        ${payNoteHtml}
        ${filePathHtml}
        ${progressHtml}

        <!-- 全部产物：同时展示 manual / 自动目录 / 历史归档 -->
        <div class="drawer-section">
            <div class="drawer-section-title">全部产物</div>
            ${renderArtifactOverview(video.files)}
            <div class="drawer-file-list" id="drawer-file-list">
                ${renderDrawerFiles(video.files, bvid, video.burned, sidecar)}
            </div>
        </div>

        <!-- 历史弹幕与评论浏览器 -->
        <div class="drawer-section">
            <div class="drawer-section-title">弹幕与评论历史</div>
            ${renderSidecarBrowser(video.files, bvid)}
            <div id="drawer-sidecar-viewer" class="drawer-comments">
                <div class="drawer-comments-hint">选择上方任一弹幕或评论版本查看本地内容</div>
            </div>
        </div>

        <!-- 实时数据（按需加载，避免触发风控） -->
        <div class="drawer-section">
            <div class="drawer-section-title">
                实时数据
                <button class="btn btn-sm btn-ghost" data-action="refresh-video-info" data-bvid="${safeBvid}" title="从 B 站拉取最新数据（5 分钟缓存）">
                    <i class="fa-solid fa-sync-alt"></i> 刷新数据
                </button>
            </div>
            <div id="drawer-live-stats" class="drawer-live-stats">
                <div class="drawer-live-stats-hint">点击"刷新数据"从 B 站拉取最新统计（点赞 / 投币 / 收藏 / 评论等）</div>
            </div>
        </div>

        <!-- 主操作 -->
        <div class="drawer-section">
            <div class="drawer-section-title">操作</div>
            <div class="drawer-actions">
                ${mainActionHtml}
                ${pauseResumeBtnHtml}
                ${deleteBtnHtml}
            </div>
            <div class="drawer-extras">
                <button class="drawer-extra-btn" data-action="download-cover" data-bvid="${safeBvid}">
                    <i class="fa-solid fa-image"></i> 下载封面
                </button>
                <button class="drawer-extra-btn" data-action="open-video-page" data-bvid="${safeBvid}">
                    <i class="fa-solid fa-external-link-alt"></i> 原视频链接
                </button>
                <button class="drawer-extra-btn" data-action="download-danmaku" data-source="${artifactSource}" data-bvid="${safeBvid}">
                    <i class="fa-solid fa-comment-dots"></i> 下载弹幕
                </button>
                <button class="drawer-extra-btn" data-action="download-comments" data-source="${artifactSource}" data-bvid="${safeBvid}">
                    <i class="fa-solid fa-comments"></i> 下载评论
                </button>
            </div>
        </div>

        <!-- 日志区（按 bvid 过滤，时间倒序） -->
        <div class="drawer-section">
            <div class="drawer-section-title">
                日志
                <button class="btn btn-sm btn-ghost" data-action="load-bvid-logs" data-bvid="${safeBvid}" title="刷新日志">
                    <i class="fa-solid fa-sync-alt"></i> 刷新
                </button>
            </div>
            <div id="drawer-logs" class="drawer-logs">
                <div class="drawer-logs-hint">点击"刷新"加载该视频的日志</div>
            </div>
        </div>
    `;

    // 异步加载日志（首次打开自动拉一次）
    loadBvidLogs(bvid);
}

/// 渲染抽屉"已下载文件"列表。
/// files: 后端扫描返回的文件数组；burned: { danmaku, subtitle }；sidecar: 无文件明细时的侧车状态。
export function renderDrawerFiles(files, bvid, burned, sidecar) {
    burned = burned || {};
    // 优先使用后端返回的真实文件列表
    if (files && Array.isArray(files) && files.length > 0) {
        const groups = new Map();
        files.forEach(file => {
            const location = file.location || 'other';
            if (!groups.has(location)) groups.set(location, []);
            groups.get(location).push(file);
        });
        return [...groups.entries()].map(([location, entries]) => `
            <div class="drawer-file-group">
                <div class="drawer-file-group-title">
                    <span><i class="fa-solid fa-folder"></i> ${escapeHtml(locationLabel(location))}</span>
                    <span>${entries.length} 个文件</span>
                </div>
                ${entries.map(file => renderDrawerFileItem(file, bvid, burned)).join('')}
            </div>
        `).join('');
    }
    // 没有文件明细时，用聚合状态提供只读摘要。
    if (sidecar) {
        return `<div class="drawer-sidecar">${renderSidecarIcons(sidecar)}</div>`;
    }
    return `<div class="drawer-files-empty"><i class="fa-solid fa-inbox"></i> 暂无本地文件记录</div>`;
}

export function renderDrawerFileItem(f, bvid, burned) {
    // `file_type` 是当前接口字段，`type` 用于读取已有缓存数据。
    const type = f.file_type || f.type || 'other';
    const safeBvid = escapeHtml(bvid || '');
    const name = escapeHtml(f.name || '未知文件');
    const internalPath = escapeHtml(f.path || '');
    const path = escapeHtml(f.display_path || '');
    const size = f.size ? formatFileSize(Number(f.size)) : '';
    const iconMap = {
        video: 'fa-film',
        danmaku_video: 'fa-fire',
        audio: 'fa-music',
        cover: 'fa-image',
        danmaku: 'fa-comment-dots',
        subtitle: 'fa-closed-captioning',
        comment: 'fa-comments',
        other: 'fa-file'
    };
    const icon = iconMap[type] || 'fa-file';
    const typeLabelMap = {
        video: '视频', danmaku_video: '弹幕版视频', audio: '音频', cover: '封面',
        danmaku: '弹幕', subtitle: 'CC 字幕', comment: '评论', other: '文件'
    };
    const typeLabel = typeLabelMap[type] || '文件';
    const versionLabel = f.version ? formatArchiveVersion(f.version) : (f.is_current ? '当前' : '副本');
    const modified = f.modified_at ? new Date(Number(f.modified_at) * 1000).toLocaleString() : '';

    let actions = f.display_path
        ? `<button class="btn btn-sm btn-ghost" data-copy-path="${path}" title="复制文件路径"><i class="fa-solid fa-copy"></i> 路径</button>` : '';
    if (f.path) {
        actions += `<button class="btn btn-sm btn-ghost" data-action="open-history-directory" data-bvid="${safeBvid}" data-path="${internalPath}" title="打开所在目录"><i class="fa-solid fa-folder-open"></i> 打开</button>`;
    }
    if (type === 'comment') {
        actions += `<button class="btn btn-sm btn-primary" data-action="load-drawer-comments" data-bvid="${safeBvid}" data-path="${internalPath}"><i class="fa-solid fa-eye"></i> 查看</button>`;
    } else if (type === 'danmaku') {
        actions += `<button class="btn btn-sm btn-primary" data-action="load-drawer-danmaku" data-bvid="${safeBvid}" data-path="${internalPath}"><i class="fa-solid fa-eye"></i> 查看</button>`;
    }
    if (type === 'video') {
        if (burned.danmaku) {
            actions += `<span class="burn-badge burned" title="弹幕已烧录"><i class="fa-solid fa-check"></i> 弹幕已烧录</span>`;
        }
        if (burned.subtitle) {
            actions += `<span class="burn-badge burned" title="字幕已烧录"><i class="fa-solid fa-check"></i> 字幕已烧录</span>`;
        }
        // 烧录按钮：未烧录时才显示
        if (!burned.danmaku) {
            actions += `<button class="btn btn-sm btn-primary" data-action="burn-media" data-bvid="${safeBvid}" data-kind="danmaku" title="将弹幕烧录进视频"><i class="fa-solid fa-fire"></i> 烧录弹幕</button>`;
        }
        if (!burned.subtitle) {
            actions += `<button class="btn btn-sm btn-primary" data-action="burn-media" data-bvid="${safeBvid}" data-kind="subtitle" title="将 CC 字幕烧录进视频"><i class="fa-solid fa-closed-captioning"></i> 烧录字幕</button>`;
        }
    }

    return `
        <div class="drawer-file-item" data-file-type="${type}">
            <div class="drawer-file-icon"><i class="fa-solid ${icon}"></i></div>
            <div class="drawer-file-main">
                <div class="drawer-file-name" title="${path || name}">${name}</div>
                <div class="drawer-file-meta">
                    <span class="drawer-file-type">${typeLabel}</span>
                    <span class="drawer-file-version ${f.is_current ? 'current' : ''}">${escapeHtml(versionLabel)}</span>
                    ${size ? `<span class="drawer-file-size">${size}</span>` : ''}
                    ${f.format ? `<span class="drawer-file-format">${escapeHtml(f.format)}</span>` : ''}
                    ${modified ? `<span>${escapeHtml(modified)}</span>` : ''}
                </div>
                ${path ? `<div class="drawer-file-display-path" title="${path}">${path}</div>` : ''}
            </div>
            ${actions ? `<div class="drawer-file-actions">${actions}</div>` : ''}
        </div>
    `;
}

function locationLabel(location) {
    if (location === 'manual') return '手动下载区域';
    if (location.startsWith('auto:')) return `自动下载区域 · UID ${location.slice(5)}`;
    if (location.startsWith('other:')) return `自定义目录 · ${location.slice(6)}`;
    return '其他下载区域';
}

function formatArchiveVersion(version) {
    if (!version) return '最新';
    const match = /^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})-(\d{3})$/.exec(version);
    if (!match) return version;
    return `${match[1]}-${match[2]}-${match[3]} ${match[4]}:${match[5]}:${match[6]}.${match[7]}`;
}

function renderArtifactOverview(files) {
    if (!Array.isArray(files) || files.length === 0) return '';
    const locations = new Set(files.map(file => file.location || 'other'));
    const videos = files.filter(file => ['video', 'danmaku_video'].includes(file.file_type)).length;
    const archives = files.filter(file => Boolean(file.version)).length;
    return `
        <div class="drawer-artifact-overview">
            <span><strong>${files.length}</strong> 个文件</span>
            <span><strong>${locations.size}</strong> 个产物区域</span>
            <span><strong>${videos}</strong> 个视频产物</span>
            <span><strong>${archives}</strong> 个历史归档文件</span>
        </div>
    `;
}

function renderSidecarBrowser(files, bvid) {
    if (!Array.isArray(files)) files = [];
    const safeBvid = escapeHtml(bvid || '');
    const danmaku = preferredDanmakuVersions(files.filter(file => file.file_type === 'danmaku'));
    const comments = files.filter(file => file.file_type === 'comment');
    const renderButtons = (entries, action, icon) => entries.length > 0
        ? entries.map(file => {
            const location = locationLabel(file.location || 'other');
            const version = file.version ? formatArchiveVersion(file.version) : (file.is_current ? '当前最新版' : '最新副本');
            return `<button class="sidecar-version-btn" data-action="${action}" data-bvid="${safeBvid}" data-path="${escapeHtml(file.path || '')}" title="${escapeHtml(file.path || '')}">
                <i class="fa-solid ${icon}"></i>
                <span>${escapeHtml(version)}</span>
                <small>${escapeHtml(location)} · ${escapeHtml((file.format || '').toUpperCase())}</small>
            </button>`;
        }).join('')
        : '<div class="drawer-comments-hint">暂无本地版本</div>';
    return `
        <div class="drawer-sidecar-browser">
            <div class="sidecar-version-column">
                <div class="sidecar-version-title"><i class="fa-solid fa-comment-dots"></i> 弹幕版本（${danmaku.length}）</div>
                <div class="sidecar-version-list">${renderButtons(danmaku, 'load-drawer-danmaku', 'fa-comment-dots')}</div>
            </div>
            <div class="sidecar-version-column">
                <div class="sidecar-version-title"><i class="fa-solid fa-comments"></i> 评论版本（${comments.length}）</div>
                <div class="sidecar-version-list">${renderButtons(comments, 'load-drawer-comments', 'fa-comments')}</div>
            </div>
        </div>
    `;
}

function preferredDanmakuVersions(files) {
    const groups = new Map();
    const priority = { json: 0, txt: 1, xml: 2 };
    files.forEach(file => {
        const directory = String(file.path || '').split('/').slice(0, -1).join('/');
        const key = `${file.location || 'other'}|${directory}|${file.version || 'latest'}`;
        const previous = groups.get(key);
        if (!previous || (priority[file.format] ?? 9) < (priority[previous.format] ?? 9)) {
            groups.set(key, file);
        }
    });
    return [...groups.values()];
}

/// 在资源管理器中打开文件所在目录（后端配套接口）。
