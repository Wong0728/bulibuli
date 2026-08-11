import { _state, _NETWORK_ERR_MSG } from './state.js';
import { escapeHtml } from './utils.js';
import { checkNetworkBeforeAction, apiPost, apiGet } from './core.js';
import { showToast } from './download-status.js';
import { renderSeasonResolveResult } from './media-actions.js';

// --- 手动查询 ---
_state.manualQueryMode = 'submission'; // 'submission' | 'series' | 'link'
_state.manualQueryOffset = 0;
_state.manualQueryLimit = 20;
_state.manualQueryHasMore = false;
_state.manualQueryUid = null;
_state.manualQuerySeriesId = null;
_state.manualQueryCollectionType = 'series'; // 'season' | 'series'
_state.manualQueryLoading = false;

export function setManualQueryMode(mode) {
    if (mode !== 'submission' && mode !== 'series' && mode !== 'link') return;
    _state.manualQueryMode = mode;
    document.querySelectorAll('.manual-mode-switch .mode-btn').forEach(btn => {
        btn.classList.toggle('active', btn.dataset.mode === mode);
    });
    const seriesGroup = document.getElementById('manual-series-select-group');
    const queryBtn = document.getElementById('manual-query-btn');
    const seriesSelect = document.getElementById('manual-series-select');
    const uidGroup = document.querySelector('#tab-manual .uid-input-group:not(#manual-link-input-group)');
    const linkGroup = document.getElementById('manual-link-input-group');

    if (mode === 'link') {
        // 链接解析模式：隐藏投稿/合集输入，显示链接输入
        if (uidGroup) uidGroup.hidden = true;
        if (linkGroup) linkGroup.hidden = false;
        document.getElementById('manual-load-more').hidden = true;
        return;
    }

    // 投稿/合集模式：恢复显示 UID 输入组，隐藏链接输入组
    if (uidGroup) uidGroup.hidden = false;
    if (linkGroup) linkGroup.hidden = true;

    if (mode === 'series') {
        if (seriesGroup) seriesGroup.hidden = false;
        if (queryBtn) queryBtn.innerHTML = '<i class="fa-solid fa-layer-group"></i> 加载合集视频';
        // 切换到合集模式时，若已选择博主则自动加载合集
        const uid = document.getElementById('uid-history-select')?.value;
        if (uid) {
            loadManualSeriesList(uid);
        } else {
            if (seriesSelect) seriesSelect.innerHTML = '<option value="">-- 请先选择博主 --</option>';
        }
    } else {
        if (seriesGroup) seriesGroup.hidden = true;
        if (queryBtn) queryBtn.innerHTML = '<i class="fa-solid fa-play-circle"></i> 查询最新视频';
        if (seriesSelect) seriesSelect.innerHTML = '<option value="">-- 请先选择博主 --</option>';
    }
}

export async function loadManualSeriesList(uid) {
    const seriesSelect = document.getElementById('manual-series-select');
    if (!seriesSelect) return;
    seriesSelect.innerHTML = '<option value="">—— 正在加载合集 ——</option>';
    try {
        const result = await apiGet(`/api/blogger/series?uid=${encodeURIComponent(uid)}`);
        if (result.code !== 0) {
            seriesSelect.innerHTML = `<option value="">${escapeHtml(result.message || '无法加载合集')}</option>`;
            return;
        }
        const series = result.data?.series || [];
        if (series.length === 0) {
            seriesSelect.innerHTML = '<option value="">该博主暂无合集</option>';
            return;
        }
        seriesSelect.innerHTML = '<option value="">-- 选择合集 --</option>' +
            series.map(s => {
                const sid = String(s.id || s.series_id || '');
                const ctype = s.type || 'series';
                const label = (ctype === 'season' ? '[合集] ' : '[系列] ') + escapeHtml(s.name || s.title || '未命名');
                return `<option value="${escapeHtml(sid)}" data-type="${ctype}">${label} (${s.count || s.total || 0})</option>`;
            }).join('');
    } catch (e) {
        seriesSelect.innerHTML = '<option value="">合集加载失败</option>';
    }
}

export async function doManualQuery() {
    if (!checkNetworkBeforeAction()) return;
    const uid = document.getElementById('uid-history-select').value;
    const btn = document.getElementById('manual-query-btn') || document.querySelector('#tab-manual .btn-primary');
    const resultDiv = document.getElementById('manual-result');
    if (!uid) return showToast('请先选择博主', 'error');

    // 按合集模式校验
    let seriesId = null;
    let collectionType = 'series';
    if (_state.manualQueryMode === 'series') {
        const seriesSelect = document.getElementById('manual-series-select');
        seriesId = seriesSelect?.value || '';
        if (!seriesId) return showToast('请先选择合集', 'error');
        const selectedOption = seriesSelect?.selectedOptions?.[0];
        collectionType = selectedOption?.dataset?.type || 'series';
    }

    _state.manualQueryUid = uid;
    _state.manualQuerySeriesId = seriesId;
    _state.manualQueryCollectionType = collectionType;
    _state.manualQueryOffset = 0;
    _state.manualQueryLoading = true;

    btn.disabled = true;
    btn.innerHTML = '<span class="loading"></span> 查询中';
    resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-spinner fa-spin fa-2x mb-md"></i><p>正在请求B站...</p></div>`;
    document.getElementById('manual-load-more').hidden = true;

    try {
        const limit = Math.min(parseInt(document.getElementById('setting-manual-query-limit')?.value) || 20, 50);
        _state.manualQueryLimit = limit;
        let result;
        if (_state.manualQueryMode === 'series') {
            result = await apiGet(`/api/blogger/series-videos?uid=${encodeURIComponent(uid)}&series_id=${encodeURIComponent(seriesId)}&collection_type=${encodeURIComponent(collectionType)}&offset=0&limit=${limit}`);
        } else {
            result = await apiPost('/api/video/get-videos', { uid, limit, offset: 0 });
        }

        if (result.code === 0) {
            const data = result.data || {};
            _state.manualQueryVideos = {};
            const videos = data.videos || [];
            _state.manualQueryOffset = Number.isFinite(data.offset) ? data.offset : 0;
            _state.manualQueryHasMore = data.has_more === true || (data.has_more !== false && videos.length >= limit);
            renderManualQueryResults(videos, true);
        } else if (result.offline) {
            showToast(_NETWORK_ERR_MSG, 'error');
            resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-inbox fa-2x mb-md"></i><p>暂无结果，请稍后重试</p></div>`;
        } else {
            resultDiv.innerHTML = `<div class="card empty-state" data-js-style="1"><i class="fa-solid fa-exclamation-triangle fa-2x mb-md"></i><p>${escapeHtml(result.message || '查询失败')}</p></div>`;
        }
    } catch (e) {
        resultDiv.innerHTML = `<div class="card empty-state" data-js-style="1"><i class="fa-solid fa-exclamation-triangle fa-2x mb-md"></i><p>请求错误: ${escapeHtml(e.message)}</p></div>`;
    } finally {
        _state.manualQueryLoading = false;
        btn.disabled = false;
        const btnText = _state.manualQueryMode === 'series' ? '<i class="fa-solid fa-layer-group"></i> 加载该合集视频' : '<i class="fa-solid fa-play-circle"></i> 查询最新视频';
        btn.innerHTML = btnText;
    }
}

export async function loadMoreManualQuery() {
    if (_state.manualQueryLoading || !_state.manualQueryHasMore) return;
    if (!checkNetworkBeforeAction()) return;
    _state.manualQueryLoading = true;
    const loadMoreDiv = document.getElementById('manual-load-more');
    loadMoreDiv.innerHTML = '<button class="btn btn-ghost" disabled><span class="loading"></span> 加载中</button>';

    try {
        const nextOffset = _state.manualQueryOffset + _state.manualQueryLimit;
        let result;
        if (_state.manualQueryMode === 'series') {
            result = await apiGet(`/api/blogger/series-videos?uid=${encodeURIComponent(_state.manualQueryUid)}&series_id=${encodeURIComponent(_state.manualQuerySeriesId)}&collection_type=${encodeURIComponent(_state.manualQueryCollectionType)}&offset=${nextOffset}&limit=${_state.manualQueryLimit}`);
        } else {
            result = await apiPost('/api/video/get-videos', { uid: _state.manualQueryUid, limit: _state.manualQueryLimit, offset: nextOffset });
        }

        if (result.code === 0) {
            const data = result.data || {};
            const videos = data.videos || [];
            _state.manualQueryOffset = Number.isFinite(data.offset) ? data.offset : nextOffset;
            _state.manualQueryHasMore = data.has_more === true
                || (data.has_more !== false && videos.length >= _state.manualQueryLimit);
            renderManualQueryResults(videos, false);
        } else {
            showToast(result.message || '加载更多失败', 'error');
            renderManualLoadMore();
        }
    } catch (e) {
        showToast('加载更多请求失败', 'error');
        renderManualLoadMore();
    } finally {
        _state.manualQueryLoading = false;
    }
}

export function renderManualQueryResults(videos, reset) {
    const resultDiv = document.getElementById('manual-result');
    if (!resultDiv) return;

    if (reset && videos.length === 0) {
        resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-info-circle fa-2x mb-md"></i><p>暂无视频</p></div>`;
        document.getElementById('manual-load-more').hidden = true;
        return;
    }

    // 保存到全局变量供抽屉使用
    videos.forEach(v => {
        const rawPic = v.pic ? (v.pic.startsWith('//') ? 'https:' + v.pic : v.pic) : '';
        _state.manualQueryVideos[v.bvid] = {
            bvid: v.bvid,
            title: v.title,
            pic: rawPic,
            poster: rawPic,
            created: v.created,
            play: v.play,
            duration: v.duration,
            status: 'not_downloaded',
            pubdate: v.created ? new Date(v.created * 1000).toLocaleDateString('zh-CN') : (v.pubdate || '--'),
            view: (v.play || 0).toLocaleString()
        };
    });

    const cardsHtml = videos.map(v => renderManualVideoCard(v)).join('');

    if (reset) {
        resultDiv.innerHTML = `<div class="video-grid" id="manual-video-grid">${cardsHtml}</div>`;
    } else {
        const grid = document.getElementById('manual-video-grid');
        if (grid) {
            grid.insertAdjacentHTML('beforeend', cardsHtml);
        } else {
            resultDiv.innerHTML = `<div class="video-grid" id="manual-video-grid">${cardsHtml}</div>`;
        }
    }

    renderManualLoadMore();
}

export function renderManualVideoCard(v) {
    const safeBvid = escapeHtml(v.bvid);
    const rawPic = v.pic ? (v.pic.startsWith('//') ? 'https:' + v.pic : v.pic) : '';
    const thumbUrl = rawPic ? `/api/video/proxy-image?url=${encodeURIComponent(rawPic)}` : '';
    const pubdate = v.created ? new Date(v.created * 1000).toLocaleDateString('zh-CN') : (v.pubdate || '--');
    const payBadge = (v.rights && (v.rights.ugc_pay === 1 || v.rights.pay === 1))
        ? '<span class="video-card-badge pay" title="充电/付费专属"><i class="fa-solid fa-coins"></i></span>'
        : '<span class="video-card-badge not-downloaded">未下载</span>';
        
    return `
        <div class="video-card-grid not-downloaded" data-action="open-manual-video" data-bvid="${safeBvid}">
            <div class="video-card-thumb">
                ${thumbUrl ? `<img src="${thumbUrl}" alt="" loading="lazy" data-image-error="remove">` : ''}
                ${payBadge}
                ${v.duration ? `<span class="video-card-duration">${escapeHtml(v.duration)}</span>` : ''}
            </div>
            <div class="video-card-body">
                <div class="video-card-title">${escapeHtml(v.title || '未知标题')}</div>
                <div class="video-card-meta">
                    <div class="video-card-meta-left">
                        <span class="video-card-meta-item">
                            <i class="fa-solid fa-calendar-alt"></i>
                            ${pubdate}
                        </span>
                    </div>
                </div>
            </div>
        </div>
    `;
}

export function renderManualLoadMore() {
    const loadMoreDiv = document.getElementById('manual-load-more');
    if (!loadMoreDiv) return;
    if (_state.manualQueryHasMore) {
        loadMoreDiv.hidden = false;
        loadMoreDiv.innerHTML = `<button class="btn btn-ghost" data-action="load-more-manual"><i class="fa-solid fa-chevron-down"></i> 加载更多</button>`;
    } else if (Object.keys(_state.manualQueryVideos || {}).length > 0) {
        loadMoreDiv.hidden = false;
        loadMoreDiv.innerHTML = `<span class="manual-no-more">没有更多了</span>`;
    } else {
        loadMoreDiv.hidden = true;
    }
}

// --- 链接解析（番剧 / 课程 / 普通视频） ---

// 解析用户输入的链接：调用 /api/video/resolve，并按媒体类型分发渲染。
// - 番剧/课程：渲染季信息和分集列表（勾选下载），见 media-actions.js。
// - 普通视频（BV/AV）：拉取视频信息后复用既有单视频卡片流程。
// - pay_blocked：显示可读的权限提示，不阻断页面。
export async function doManualResolve() {
    if (!checkNetworkBeforeAction()) return;
    const inputEl = document.getElementById('manual-link-input');
    const btn = document.getElementById('manual-resolve-btn');
    const resultDiv = document.getElementById('manual-result');
    if (!inputEl || !resultDiv) return;

    const input = inputEl.value.trim();
    if (!input) {
        showToast('请输入番剧 / 课程链接', 'error');
        return;
    }

    const originalHtml = btn?.innerHTML;
    if (btn) { btn.disabled = true; btn.innerHTML = '<span class="loading"></span> 解析中'; }
    resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-spinner fa-spin fa-2x mb-md"></i><p>正在解析链接...</p></div>`;
    document.getElementById('manual-load-more').hidden = true;

    try {
        const result = await apiPost('/api/video/resolve', { input });
        if (result.offline) {
            showToast(_NETWORK_ERR_MSG, 'error');
            resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-inbox fa-2x mb-md"></i><p>暂无结果，请稍后重试</p></div>`;
            return;
        }
        if (result.code !== 0) {
            resultDiv.innerHTML = `<div class="card empty-state" data-js-style="1"><i class="fa-solid fa-exclamation-triangle fa-2x mb-md"></i><p>${escapeHtml(result.message || '解析失败')}</p></div>`;
            return;
        }
        await renderResolveResult(result.data || {});
    } catch (e) {
        resultDiv.innerHTML = `<div class="card empty-state" data-js-style="1"><i class="fa-solid fa-exclamation-triangle fa-2x mb-md"></i><p>请求错误: ${escapeHtml(e.message)}</p></div>`;
    } finally {
        if (btn) { btn.disabled = false; btn.innerHTML = originalHtml; }
    }
}

// 按 resolve 返回的媒体类型分发渲染。
async function renderResolveResult(result) {
    const resultDiv = document.getElementById('manual-result');
    const media = result.media || {};
    const mediaType = result.media_type || '';

    // 番剧 / 课程：渲染季信息 + 分集选集 UI
    if (mediaType === 'pgc' || mediaType === 'cheese') {
        renderSeasonResolveResult(result, mediaType);
        return;
    }

    // 普通视频（BV）：拉取视频信息后复用单视频卡片
    const bvid = media.type === 'video_bv' ? media.id : '';
    if (bvid) {
        await renderResolvedNormalVideo(bvid, resultDiv);
        return;
    }

    // AV 链接：后端 get-video-info 仅支持 bvid，提示用户改用 BV 链接
    if (media.type === 'video_av') {
        resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-info-circle fa-2x mb-md"></i><p>暂不支持 AV 链接，请使用 BV 链接（可在视频页地址栏复制）</p></div>`;
        return;
    }

    // 未知类型 / pay_blocked 兜底
    if (result.pay_blocked) {
        resultDiv.innerHTML = `<div class="card empty-state" data-js-style="1"><i class="fa-solid fa-lock fa-2x mb-md"></i><p>${escapeHtml(result.message || '当前账号无权限访问该内容')}</p></div>`;
        return;
    }
    resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-info-circle fa-2x mb-md"></i><p>未能识别的链接类型</p></div>`;
}

// 普通视频链接解析：拉取视频信息后渲染单张卡片，复用既有手动查询数据流。
async function renderResolvedNormalVideo(bvid, resultDiv) {
    resultDiv.innerHTML = `<div class="card empty-state"><i class="fa-solid fa-spinner fa-spin fa-2x mb-md"></i><p>正在获取视频信息...</p></div>`;
    try {
        const info = await apiGet(`/api/video/info?bvid=${encodeURIComponent(bvid)}`);
        const data = info.data || {};
        if (info.code !== 0 || !data.bvid) {
            resultDiv.innerHTML = `<div class="card empty-state" data-js-style="1"><i class="fa-solid fa-exclamation-triangle fa-2x mb-md"></i><p>${escapeHtml(info.message || '获取视频信息失败')}</p></div>`;
            return;
        }
        _state.manualQueryVideos = _state.manualQueryVideos || {};
        const rawPic = data.pic ? (data.pic.startsWith('//') ? 'https:' + data.pic : data.pic) : '';
        _state.manualQueryVideos[data.bvid] = {
            bvid: data.bvid,
            title: data.title,
            pic: rawPic,
            poster: rawPic,
            created: data.pub_timestamp,
            play: data.stat?.view || 0,
            duration: data.duration,
            status: 'not_downloaded',
            pubdate: data.pub_timestamp ? new Date(data.pub_timestamp * 1000).toLocaleDateString('zh-CN') : '--',
            view: (data.stat?.view || 0).toLocaleString()
        };
        resultDiv.innerHTML = `<div class="video-grid" id="manual-video-grid">${renderManualVideoCard(_state.manualQueryVideos[data.bvid])}</div>`;
    } catch (e) {
        resultDiv.innerHTML = `<div class="card empty-state" data-js-style="1"><i class="fa-solid fa-exclamation-triangle fa-2x mb-md"></i><p>获取视频信息失败: ${escapeHtml(e.message)}</p></div>`;
    }
}
