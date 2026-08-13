import { _state, _NETWORK_ERR_MSG } from './state.js';
import { escapeHtml } from './utils.js';
import { checkNetworkBeforeAction, apiPost, apiGet } from './core.js';
import { downloadDanmaku, downloadComments } from './media-links.js';
import { updateDownloadLists } from './download-queue.js';
import { loadHistoryBoard, formatViewCount } from './history.js';
import { showToast, confirmDialog } from './download-status.js';
import { openVideoDrawer, closeVideoDrawer, refreshQualityPills, _ALL_QUALITY_OPTIONS } from './drawer.js';

// 通用烧录入口（弹幕、字幕或两者）。
export async function burnMedia(bvid, source, button, historyId = undefined) {
    if (!bvid || !button) return;
    if (!checkNetworkBeforeAction()) return;
    if (_state.burningTasks.has(bvid)) {
        showToast('该视频正在烧录中，请耐心等待', 'info');
        return;
    }

    const originalHtml = button.innerHTML;
    button.disabled = true;
    button.innerHTML = '<span class="loading"></span>';
    button.title = '正在烧录...';
    showToast('正在烧录，请等待...', 'info');

    try {
        const result = await apiPost('/api/download/burn', { bvid, source, history_id: historyId ?? null });
        if (result && result.offline) {
            showToast(_NETWORK_ERR_MSG, 'error');
            _state.burningTasks.delete(bvid);
            button.disabled = false;
            button.innerHTML = originalHtml;
            button.title = '';
            return;
        }
        if (result.code !== 0) {
            showToast(result.message || '启动烧录失败', 'error');
            button.disabled = false;
            button.innerHTML = originalHtml;
            button.title = '';
            _state.burningTasks.delete(bvid);
            return;
        }

        const taskId = result.data?.task_id;
        _state.burningTasks.set(bvid, { taskId, button, originalHtml });
        await pollBurnStatus(bvid, taskId, button, originalHtml, source);
    } catch (e) {
        showToast('烧录请求失败', 'error');
        button.disabled = false;
        button.innerHTML = originalHtml;
        button.title = '';
        _state.burningTasks.delete(bvid);
    }
}

// 轮询烧录状态。
export async function pollBurnStatus(bvid, taskId, button, originalHtml, source) {
    let completed = false;
    let attempts = 0;
    const maxAttempts = 600;
    while (!completed && attempts < maxAttempts) {
        await new Promise(r => setTimeout(r, 1000));
        attempts++;
        try {
            const statusRes = await apiGet(`/api/download/burn/status/${taskId}`);
            if (statusRes.code !== 0) continue;
            const status = statusRes.data?.status;
            if (status === 'completed') {
                completed = true;
                showToast('烧录完成！', 'success');
                button.innerHTML = '<i class="fa-solid fa-check"></i>';
                button.classList.add('btn-success');
                button.title = '已烧录';
                _state.burningTasks.delete(bvid);
                // 刷新抽屉以更新 burned 状态
                if (_state.currentDrawerBvid === bvid) {
                    openVideoDrawer(bvid);
                }
            } else if (status === 'failed') {
                completed = true;
                showToast(statusRes.message || statusRes.data?.message || '烧录失败', 'error');
                button.disabled = false;
                button.innerHTML = originalHtml;
                button.title = '';
                _state.burningTasks.delete(bvid);
            }
        } catch (e) {
            console.error('[pollBurnStatus] 轮询异常:', e);
        }
    }
    if (!completed) {
        showToast('烧录超时，请稍后刷新查看', 'warning');
        button.disabled = false;
        button.innerHTML = originalHtml;
        button.title = '';
        _state.burningTasks.delete(bvid);
    }
}

// 删除视频记录（文件和 DB）。
export async function deleteVideoRecord(bvid, historyId = undefined) {
    if (!bvid) return;
    if (!(await confirmDialog(`确认删除视频 ${bvid} 的记录？\n\n将同时删除：\n- 本地视频文件\n- 本地封面文件\n- 弹幕 / 字幕侧车文件\n- download_task 记录\n- history 记录\n\n此操作不可撤销。`, { title: '删除记录', okText: '删除', danger: true }))) {
        return;
    }
    try {
        const result = await apiPost('/api/history/delete', { bvid, history_id: historyId ?? null, delete_files: true });
        if (result.code === 0) {
            showToast(`已删除记录（${(result.data?.removed_files || []).length} 个文件）`, 'success');
            closeVideoDrawer();
            // 刷新看板
            await loadHistoryBoard(_state.currentBoardTab);
        } else {
            showToast(result.message || '删除失败', 'error');
        }
    } catch (e) {
        showToast('删除失败', 'error');
    }
}

// 从 B 站拉取最新视频数据并渲染到“实时数据”区。
export async function refreshVideoInfo(bvid) {
    const container = document.getElementById('drawer-live-stats');
    if (!container) return;
    container.innerHTML = `<div class="drawer-live-stats-hint"><i class="fa-solid fa-spinner fa-spin"></i> 正在从 B 站拉取...</div>`;
    try {
        const result = await apiGet(`/api/video/info?bvid=${encodeURIComponent(bvid)}`);
        if (result.code !== 0) {
            if (result.offline) {
                // 网络错误：右上角 toast + 顶栏横幅；子界面仅显示中性占位，不显示网络错误
                showToast(_NETWORK_ERR_MSG, 'error');
                container.innerHTML = `<div class="drawer-live-stats-hint">实时数据暂不可用</div>`;
            } else {
                container.innerHTML = `<div class="drawer-live-stats-hint status-error"><i class="fa-solid fa-exclamation-circle"></i> ${escapeHtml(result.message || '获取失败')}</div>`;
            }
            return;
        }

        const data = result.data || {};
        const stat = data.stat || {};
        const owner = data.owner || {};

        // 把最新数据写回看板缓存，保证抽屉刷新后看板同步。
        if (_state.currentBoardVideos && _state.currentBoardVideos[bvid]) {
            const cached = _state.currentBoardVideos[bvid];
            cached.view = stat.view ?? cached.view;
            cached.duration = data.duration ?? cached.duration;
            cached.pub_timestamp = data.pubdate ?? cached.pub_timestamp;
            cached.pub_date = data.pub_date ?? cached.pub_date;
            cached.title = data.title ?? cached.title;
            cached.pic = data.pic ?? cached.pic;
            if (data.owner) {
                cached.blogger = cached.blogger || {};
                cached.blogger.name = data.owner.name ?? cached.blogger.name;
                cached.blogger.uid = String(data.owner.mid ?? cached.blogger.uid ?? '');
            }
        }
        const cells = [
            { label: '播放', value: stat.view },
            { label: '弹幕', value: stat.danmaku },
            { label: '评论', value: stat.reply },
            { label: '收藏', value: stat.favorite },
            { label: '投币', value: stat.coin },
            { label: '分享', value: stat.share },
            { label: '点赞', value: stat.like },
        ];
        container.innerHTML = `
            <div class="drawer-live-stats-grid">
                ${cells.map(c => `
                    <div class="drawer-stat-cell">
                        <span class="drawer-stat-label">${c.label}</span>
                        <span class="drawer-stat-value">${formatViewCount(Number(c.value || 0))}</span>
                    </div>
                `).join('')}
            </div>
            <div class="drawer-live-owner">
                <span>UP 主：${escapeHtml(owner.name || '--')}</span>
                <span class="drawer-info-sub">MID: ${escapeHtml(String(owner.mid || '--'))}</span>
            </div>
        `;
    } catch (e) {
        container.innerHTML = `<div class="drawer-live-stats-hint">实时数据暂不可用</div>`;
    }
}

// 加载该 bvid 的日志到抽屉“日志”区。
export async function loadBvidLogs(bvid) {
    const container = document.getElementById('drawer-logs');
    if (!container) return;
    container.innerHTML = `<div class="drawer-logs-hint"><i class="fa-solid fa-spinner fa-spin"></i> 加载中...</div>`;
    try {
        const result = await apiGet(`/api/logs/bvid?bvid=${encodeURIComponent(bvid)}&limit=100`);
        if (result.code !== 0) {
            if (result.offline) {
                showToast(_NETWORK_ERR_MSG, 'error');
                container.innerHTML = `<div class="drawer-logs-hint">暂不可用</div>`;
            } else {
                container.innerHTML = `<div class="drawer-logs-hint status-error">${escapeHtml(result.message || '加载失败')}</div>`;
            }
            return;
        }
        const logs = result.data?.logs || [];
        if (logs.length === 0) {
            container.innerHTML = `<div class="drawer-logs-hint">暂无该视频的日志</div>`;
            return;
        }
        container.innerHTML = logs.map(l => {
            let time = l.time || '';
            if (!time && l.timestamp) time = new Date(l.timestamp * 1000).toLocaleString();
            if (!time && l.created_at) time = new Date(l.created_at).toLocaleString();
            const level = (l.level || '').toLowerCase();
            const levelClass = ['error', 'warn', 'warning'].includes(level) ? 'log-error' : 'log-info';
            const msg = l.msg || l.message || '';
            return `
                <div class="drawer-log-item ${levelClass}">
                    <span class="drawer-log-time">${escapeHtml(time)}</span>
                    <span class="drawer-log-level">${escapeHtml(l.level || 'INFO')}</span>
                    <span class="drawer-log-msg">${escapeHtml(msg)}</span>
                </div>
            `;
        }).join('');
    } catch (e) {
        container.innerHTML = `<div class="drawer-logs-hint">暂不可用</div>`;
    }
}

// 加载并展示已下载的评论（抽屉“评论”区）。优先渲染结构化卡片，兼容旧 TXT；只读本地文件。
// 渲染抽屉内容（手动查询版本）
export function renderDrawerContentForManualQuery(video, bvid) {
    const bodyEl = document.getElementById('drawer-body');
    if (!bodyEl) return;

    // 使用代理URL显示图片
    const rawPic = video.poster || video.pic || '';
    const thumbUrl = rawPic ? `/api/video/proxy-image?url=${encodeURIComponent(rawPic)}` : '';

    const qualityPills = _ALL_QUALITY_OPTIONS.map((q, idx) => `
        <button class="quality-pill ${q.qn === 80 ? 'active' : ''}" data-action="select-quality" data-qn="${q.qn}">
            ${q.label}
            ${q.tag ? `<span class="quality-pill-tag">${q.tag}</span>` : ''}
        </button>
    `).join('');

    // 设置默认质量为1080P
    _state.selectedQuality = 80;

    bodyEl.innerHTML = `
        <div class="drawer-preview">
            ${thumbUrl ? `<img src="${thumbUrl}" alt="" data-image-error="remove">` : ''}
            ${video.duration ? `<span class="drawer-preview-badge">${escapeHtml(video.duration)}</span>` : ''}
        </div>

        <div class="drawer-info-row">
            <div class="drawer-info-item">
                <span class="drawer-info-label">发布时间</span>
                <span class="drawer-info-value">${video.pubdate || '--'}</span>
            </div>
            <div class="drawer-info-item">
                <span class="drawer-info-label">播放量</span>
                <span class="drawer-info-value">${video.view || '--'}</span>
            </div>
            <div class="drawer-info-item">
                <span class="drawer-info-label">状态</span>
                <span class="drawer-info-value drawer-info-muted">未下载</span>
            </div>
        </div>

        <div class="drawer-section">
            <div class="drawer-section-title">视频下载</div>
            <div class="quality-pills" id="quality-pills-container">
                ${qualityPills}
            </div>
            <div class="drawer-pages" id="drawer-pages-section" hidden></div>
            <div class="drawer-actions">
                <button class="drawer-btn drawer-btn-primary" data-action="start-manual-video" data-bvid="${bvid}">
                    <i class="fa-solid fa-download"></i>
                    开始下载
                </button>
            </div>
        </div>

        <div class="drawer-section">
            <div class="drawer-section-title">更多选项</div>
            <div class="drawer-extras">
                <button class="drawer-extra-btn" data-action="download-cover" data-bvid="${bvid}">
                    <i class="fa-solid fa-image"></i>
                    下载封面
                </button>
                <button class="drawer-extra-btn" data-action="open-video-page" data-bvid="${bvid}">
                    <i class="fa-solid fa-external-link-alt"></i>
                    原视频链接
                </button>
                <button class="drawer-extra-btn" data-action="download-danmaku" data-source="manual" data-bvid="${bvid}">
                    <i class="fa-solid fa-comment-dots"></i>
                    下载弹幕
                </button>
                <button class="drawer-extra-btn" data-action="download-comments" data-source="manual" data-bvid="${bvid}">
                    <i class="fa-solid fa-comments"></i>
                    下载评论
                </button>
            </div>
        </div>
    `;

    // 异步获取实际可用质量并禁用不可用选项
    refreshQualityPills(bvid);
    // 异步获取分P列表：多P时渲染多选列表
    loadManualPages(bvid);
}

// 拉取视频分P信息，多P（pages>1）时渲染分P多选列表；单P保持现状不展示。
async function loadManualPages(bvid) {
    const section = document.getElementById('drawer-pages-section');
    if (!section) return;
    try {
        const result = await apiGet(`/api/video/info?bvid=${encodeURIComponent(bvid)}`);
        // 抽屉可能已被切换到其他视频，避免错位渲染
        if (_state.currentDrawerBvid !== bvid) return;
        const data = result.data || {};
        const pages = (result && result.code === 0 && Array.isArray(data.pages)) ? data.pages : [];
        if (pages.length <= 1) {
            section.hidden = true;
            return;
        }
        // 缓存到手动查询视频信息，供下载时读取 cid/part。
        if (_state.manualQueryVideos && _state.manualQueryVideos[bvid]) {
            _state.manualQueryVideos[bvid].pages = pages;
        }
        const items = pages.map(p => {
            const cid = p.cid;
            const page = p.page;
            const part = p.part || `P${page}`;
            return `
                <label class="drawer-page-item">
                    <input type="checkbox" class="drawer-page-check" checked
                        data-cid="${cid}" data-page="${page}" data-part="${escapeHtml(part)}">
                    <span class="drawer-page-index">P${page}</span>
                    <span class="drawer-page-title">${escapeHtml(part)}</span>
                </label>`;
        }).join('');
        section.innerHTML = `
            <div class="drawer-pages-header">
                <span class="drawer-pages-label">分P选择（共 ${pages.length} 个）</span>
                <button class="btn btn-ghost btn-sm" data-action="toggle-all-pages">全选/全不选</button>
            </div>
            <div class="drawer-pages-list">${items}</div>`;
        section.hidden = false;
    } catch (e) {
        // 获取失败时静默隐藏分 P 区，退回单 P 下载（默认 cid），不阻塞主流程。
        section.hidden = true;
    }
}

// 分P列表全选/全不选切换
export function toggleAllPages() {
    const checks = document.querySelectorAll('#drawer-pages-section .drawer-page-check');
    if (!checks.length) return;
    const allChecked = Array.from(checks).every(c => c.checked);
    checks.forEach(c => { c.checked = !allChecked; });
}

// --- 番剧 / 课程链接解析结果渲染与下载 ---

// 渲染番剧或课程的季信息和分集选择 UI（链接解析模式）。
// result 是 /api/video/resolve 返回的 payload，包含 season_title、cover、episodes 和 current_ep_id。
// mediaType 为 "pgc" 或 "cheese"，透传到下载请求。
// pay_blocked 时 result.pay_blocked=true，渲染可读权限提示而不是分集列表。
export function renderSeasonResolveResult(result, mediaType) {
    const resultDiv = document.getElementById('manual-result');
    if (!resultDiv) return;

    // 权限拦截：显示可读提示，不渲染分集列表
    if (result.pay_blocked) {
        const reason = result.pay_reason || '';
        const hint = mediaType === 'pgc'
            ? '该番剧可能需要大会员专享或存在区域限制'
            : '该课程可能需要购买后才能下载';
        resultDiv.innerHTML = `
            <div class="card empty-state status-error">
                <i class="fa-solid fa-lock fa-2x mb-md"></i>
                <p>${escapeHtml(result.message || '当前账号无权限访问该内容')}</p>
                <p class="empty-hint">${escapeHtml(hint)}</p>
            </div>`;
        return;
    }

    const episodes = Array.isArray(result.episodes) ? result.episodes : [];
    if (episodes.length === 0) {
        resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-inbox fa-2x mb-md"></i><p>未解析到分集数据</p></div>`;
        return;
    }

    const seasonTitle = result.season_title || '未知季';
    const cover = result.cover ? (result.cover.startsWith('//') ? 'https:' + result.cover : result.cover) : '';
    const thumbUrl = cover ? `/api/video/proxy-image?url=${encodeURIComponent(cover)}` : '';
    const currentEpId = result.current_ep_id || null;
    const isEpLink = currentEpId !== null;

    // 缓存到全局，供下载函数读取 ep_id / bvid / aid / cid
    _state.resolvedSeason = {
        mediaType,
        seasonTitle,
        episodes,
        currentEpId,
    };

    // 画质选项：复用抽屉的静态画质列表，默认 1080P
    _state.selectedQuality = 80;
    const qualityPills = _ALL_QUALITY_OPTIONS.map(q => `
        <button class="quality-pill ${q.qn === 80 ? 'active' : ''}" data-action="select-quality" data-qn="${q.qn}">
            ${q.label}
            ${q.tag ? `<span class="quality-pill-tag">${q.tag}</span>` : ''}
        </button>
    `).join('');

    // 分集列表：ep 链接默认仅勾选当前集；ss/fp 链接默认全选
    const episodeItems = episodes.map((ep, idx) => {
        const epId = ep.ep_id;
        const cid = ep.cid || 0;
        const bvid = ep.bvid || '';
        const aid = ep.aid || 0;
        const page = idx + 1;
        const displayTitle = ep.display_title || ep.title || `ep${epId}`;
        const badge = ep.badge ? `<span class="video-card-badge pay" title="${escapeHtml(ep.badge)}">${escapeHtml(ep.badge)}</span>` : '';
        const checked = isEpLink ? (Number(epId) === Number(currentEpId)) : true;
        const sectionTag = ep.section_title ? `<span class="drawer-page-index">${escapeHtml(ep.section_title)}</span>` : `<span class="drawer-page-index">P${page}</span>`;
        return `
            <label class="drawer-page-item">
                <input type="checkbox" class="drawer-page-check season-episode-check" ${checked ? 'checked' : ''}
                    data-ep-id="${epId}" data-cid="${cid}" data-bvid="${escapeHtml(bvid)}"
                    data-aid="${aid}" data-page="${page}" data-part="${escapeHtml(displayTitle)}">
                ${sectionTag}
                <span class="drawer-page-title">${escapeHtml(displayTitle)}</span>
                ${badge}
            </label>`;
    }).join('');

    const typeLabel = mediaType === 'pgc' ? '番剧' : '课程';
    const downloadBtnText = isEpLink ? '下载本集' : `下载选中分集（共 ${episodes.length} 集）`;

    resultDiv.innerHTML = `
        <div class="card">
            <div class="drawer-preview">
                ${thumbUrl ? `<img src="${thumbUrl}" alt="" data-image-error="remove">` : ''}
            </div>
            <div class="drawer-info-row drawer-info-row-spaced">
                <div class="drawer-info-item">
                    <span class="drawer-info-label">类型</span>
                    <span class="drawer-info-value">${typeLabel}</span>
                </div>
                <div class="drawer-info-item">
                    <span class="drawer-info-label">分集数</span>
                    <span class="drawer-info-value">${episodes.length}</span>
                </div>
                <div class="drawer-info-item">
                    <span class="drawer-info-label">季标题</span>
                    <span class="drawer-info-value">${escapeHtml(seasonTitle)}</span>
                </div>
            </div>

            <div class="drawer-section drawer-section-spaced">
                <div class="drawer-section-title">画质选择</div>
                <div class="quality-pills" id="quality-pills-container">
                    ${qualityPills}
                </div>
            </div>

            <div class="drawer-section">
                <div class="drawer-pages" id="season-episodes-section">
                    <div class="drawer-pages-header">
                        <span class="drawer-pages-label">分集选择（共 ${episodes.length} 集）</span>
                        <button class="btn btn-ghost btn-sm" data-action="toggle-all-episodes">全选/全不选</button>
                    </div>
                    <div class="drawer-pages-list">${episodeItems}</div>
                </div>
                <div class="drawer-actions">
                    <button class="drawer-btn drawer-btn-primary" data-action="start-season-download"
                        data-media-type="${mediaType}" data-season-title="${escapeHtml(seasonTitle)}">
                        <i class="fa-solid fa-download"></i>
                        ${downloadBtnText}
                    </button>
                </div>
            </div>
        </div>
    `;
}

// 番剧/课程分集列表全选/全不选切换
export function toggleAllEpisodes() {
    const checks = document.querySelectorAll('#season-episodes-section .season-episode-check');
    if (!checks.length) return;
    const allChecked = Array.from(checks).every(c => c.checked);
    checks.forEach(c => { c.checked = !allChecked; });
}

// 提交番剧或课程分集下载：收集勾选的分集，构造携带 ep_id/bvid/aid/cid 的 pages 数组，
// 调用 /api/download/start，并携带 media_type=pgc/cheese 和 season_title。
// 后端按 media_type 选择 pgc/cheese 取流路径，每集作为一个分 P 入队。
export async function startSeasonDownload(mediaType, seasonTitle) {
    if (!mediaType) return;
    if (!checkNetworkBeforeAction()) return;

    const checks = document.querySelectorAll('#season-episodes-section .season-episode-check');
    const pages = Array.from(checks)
        .filter(c => c.checked)
        .map(c => {
            const page = {
                cid: Number(c.dataset.cid),
                page: Number(c.dataset.page),
                part: c.dataset.part || '',
                ep_id: Number(c.dataset.epId),
                aid: Number(c.dataset.aid || 0),
            };
            const bvid = c.dataset.bvid || '';
            // 仅在 bvid 非空时携带；为空时后端会回退到顶层 bvid（PageSelector.bvid = None）
            if (bvid) page.bvid = bvid;
            return page;
        });

    if (pages.length === 0) {
        showToast('请至少选择一个分集', 'warning');
        return;
    }

    // 番剧需要每集携带 bvid；课程需要每集携带 aid
    for (const p of pages) {
        if (mediaType === 'pgc' && !p.bvid) {
            showToast(`第 ${p.page} 集缺少 bvid，无法下载`, 'error');
            return;
        }
        if (mediaType === 'cheese' && (!p.aid || p.aid === 0)) {
            showToast(`第 ${p.page} 集缺少 aid，无法下载`, 'error');
            return;
        }
    }

    // 番剧/课程用任意一集的 bvid 作为请求顶层 bvid；后端仅用于日志，实际取流按 pages 中的 ep_id 处理。
    const fallbackBvid = pages.find(p => p.bvid)?.bvid
        || _state.resolvedSeason?.episodes?.find(e => e.bvid)?.bvid
        || '';
    if (!fallbackBvid) {
        showToast('无法确定下载任务的 bvid', 'error');
        return;
    }

    const payload = {
        bvid: fallbackBvid,
        qn: _state.selectedQuality,
        media_type: mediaType,
        season_title: seasonTitle || '',
        pages,
    };

    try {
        const result = await apiPost('/api/download/start', payload);
        const data = result.data || {};
        if (result.code === 0) {
            const okCount = data.ok_count ?? pages.length;
            const total = data.total ?? pages.length;
            showToast(result.message || `已提交 ${okCount}/${total} 个分集下载`, 'success');
            updateDownloadLists();
        } else {
            showToast(result.message || '启动下载失败', 'error');
        }
    } catch (e) {
        showToast(e?.message || '下载请求失败', 'error');
    }
}

// 选择画质按钮
_state.selectedQuality = 80; // 默认 1080P
export function selectQualityPill(el, qn) {
    // 移除所有 active
    const pills = el.parentElement.querySelectorAll('.quality-pill');
    pills.forEach(p => p.classList.remove('active'));

    // 添加 active 到当前
    el.classList.add('active');

    // 保存选择
    _state.selectedQuality = qn;
}

// 下载前权限探测：拦截充电、付费或下架视频。
// 后端未实现时默认放行，避免阻塞现有功能。
export async function gateDownloadCheck(bvid) {
    try {
        const result = await apiPost('/api/video/gate-download', { bvid });
        if (result.offline) {
            return { blocked: true, message: _NETWORK_ERR_MSG };
        }

        // 预检失败不能阻断用户主动发起下载。
        if (result.code !== 0) {
            return { blocked: false };
        }

        // 接口返回成功，检查状态
        const data = result.data || {};
        if (data.allow === false) {
            // 后端明确不允许下载
            return { blocked: true, message: result.message || '该视频无法下载' };
        }

        // 根据状态和 pay_note 给出更友好的提示
        const state = data.state;
        const payNote = data.pay_note;

        // 下架视频：完全拦截
        if (state === 'removed') {
            return { blocked: true, message: '该视频已下架，无法下载' };
        }

        // 付费但无权限：拦截
        if (state === 'pay_blocked') {
            if (payNote && payNote.endsWith('_no_permission')) {
                const reason = payNote.includes('upower') ? '充电专属'
                             : payNote.includes('ugc_pay') ? 'UGC付费'
                             : '付费';
                return { blocked: true, message: `该视频为${reason}内容，当前账号无观看权限` };
            }

            // 已付费但被拦截（如设置为跳过充电视频）：允许手动重试
            if (payNote && payNote.endsWith('_paid')) {
                return { blocked: false };  // 允许下载
            }

            // 其他付费拦截：给出提示但允许重试
            return { blocked: false, message: result.message };
        }

        // 默认：根据 allow 字段判断
        return { blocked: !data.allow, message: result.message };
    } catch (e) {
        // 网络错误或其他异常：放行（避免阻塞）
        return { blocked: false };
    }
}

// 开始视频下载
export async function startVideoDownload(bvid) {
    if (!bvid) return;
    if (!checkNetworkBeforeAction()) return;

    const gate = await gateDownloadCheck(bvid);
    if (gate.blocked) {
        showToast(gate.message || '该视频无法下载，已跳过', 'warning');
        return;
    }

    try {
        const result = await apiPost('/api/download/start', {
            bvid: bvid,
            qn: _state.selectedQuality
        });

        if (result.code === 0) {
            showToast('开始下载', 'success');
            // 初始化进度跟踪，确保 WebSocket 更新能正确匹配
            const videoKey = `${bvid}_video`;
            const audioKey = `${bvid}_audio`;
            if (!_state.manualDownloadProgress[videoKey]) {
                _state.manualDownloadProgress[videoKey] = {
                    bvid: bvid, type: 'video', status: 'pending',
                    progress_percent: 0, downloaded_size: 0, total_size: 0, speed: 0
                };
            }
            if (!_state.manualDownloadProgress[audioKey]) {
                _state.manualDownloadProgress[audioKey] = {
                    bvid: bvid, type: 'audio', status: 'pending',
                    progress_percent: 0, downloaded_size: 0, total_size: 0, speed: 0
                };
            }
            closeVideoDrawer();
            // 刷新下载列表
            updateDownloadLists();
        } else {
            showToast(result.message || '启动下载失败', 'error');
        }
    } catch (e) {
        showToast(e?.message || '下载请求失败', 'error');
    }
}

// 重试失败的下载
export async function retryVideoDownload(bvid) {
    await startVideoDownload(bvid);
}

// 从手动查询界面开始视频下载
export async function startVideoDownloadFromManual(bvid) {
    if (!bvid) return;
    if (!checkNetworkBeforeAction()) return;

    const gate = await gateDownloadCheck(bvid);
    if (gate.blocked) {
        showToast(gate.message || '该视频无法下载，已跳过', 'warning');
        return;
    }

    // 从手动查询的视频信息中获取数据
    if (!_state.manualQueryVideos || !_state.manualQueryVideos[bvid]) {
        showToast('视频信息不存在', 'error');
        return;
    }

    try {
        // 多P：收集抽屉内勾选的分P；单P或未展示分P时不传 pages（保持现状）
        const checks = document.querySelectorAll('#drawer-pages-section .drawer-page-check');
        const pages = Array.from(checks)
            .filter(c => c.checked)
            .map(c => ({
                cid: Number(c.dataset.cid),
                page: Number(c.dataset.page),
                part: c.dataset.part || '',
            }));
        if (checks.length && pages.length === 0) {
            showToast('请至少选择一个分P', 'warning');
            return;
        }
        const payload = { bvid: bvid, qn: _state.selectedQuality };
        if (pages.length > 0) payload.pages = pages;
        const result = await apiPost('/api/download/start', payload);

        if (result.code === 0) {
            showToast(result.message || '开始下载', 'success');
            closeVideoDrawer();
            // 刷新下载列表
            updateDownloadLists();
        } else {
            showToast(result.message || '启动下载失败', 'error');
        }
    } catch (e) {
        showToast(e?.message || '下载请求失败', 'error');
    }
}

// 打开原视频页面
export function openVideoPage(bvid) {
    if (!bvid) return;
    window.open(`https://www.bilibili.com/video/${bvid}`, '_blank');
}

// 下载封面
export async function downloadCover(bvid) {
    if (!bvid) return;
    if (!checkNetworkBeforeAction()) return;

    try {
        const result = await apiPost('/api/video/download-cover', {
            bvid: bvid
        });
        if (result.code === 0) {
            showToast('封面下载已开始', 'success');
        } else {
            showToast(result.message || result.error || '封面下载失败', 'error');
        }
    } catch (e) {
        showToast('封面下载请求失败', 'error');
    }
}

// 复制文件路径
export function copyFilePath(path) {
    if (!path) return;

    if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(path).then(() => {
            showToast('路径已复制', 'success');
        }).catch(() => {
            fallbackCopy(path);
        });
    } else {
        fallbackCopy(path);
    }
}

// 事件委托：处理 data-copy-path 按钮的点击，避免 inline onclick 中的反斜杠被 JS 解释为转义字符。
document.addEventListener('click', function(e) {
    const btn = e.target.closest('[data-copy-path]');
    if (btn) {
        e.preventDefault();
        e.stopPropagation();
        copyFilePath(btn.dataset.copyPath);
    }
});

// 降级复制方案
export function fallbackCopy(text) {
    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.className = 'clipboard-helper';
    document.body.appendChild(textarea);
    textarea.select();
    try {
        document.execCommand('copy');
        showToast('路径已复制', 'success');
    } catch (e) {
        showToast('复制失败', 'error');
    }
    document.body.removeChild(textarea);
}
