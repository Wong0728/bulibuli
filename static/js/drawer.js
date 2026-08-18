import { _state, _NETWORK_ERR_MSG } from './state.js';
import { apiPost, apiGet } from './core.js';
import { loadHistoryBoard, renderHistoryBoard } from './history.js';
import { showToast } from './download-status.js';
import { renderDrawerContent } from './drawer-render.js';
import { renderDrawerContentForManualQuery } from './media-actions.js';

_state.drawerRequestId = 0;
_state.drawerController = null;

// 打开视频详情抽屉。
// 始终走 /api/history/list?bvid=... 拉取完整详情（含 files/burned/blogger）；看板缓存仅用于秒开标题与兜底。
export async function openVideoDrawer(bvid, historyId = undefined) {
    if (!bvid) {
        showToast('无效的视频BV号', 'error');
        return;
    }

    const drawer = document.getElementById('video-drawer');
    const overlay = document.getElementById('drawer-overlay');
    const titleEl = document.getElementById('drawer-video-title');
    if (!drawer || !overlay || !titleEl) return;

    _state.currentDrawerBvid = bvid;
    _state.currentDrawerHistoryId = historyId;
    const requestId = ++_state.drawerRequestId;
    _state.drawerController?.abort();
    const controller = new AbortController();
    _state.drawerController = controller;

    // 先显示抽屉骨架，给用户即时反馈
    titleEl.textContent = '加载中...';
    overlay.classList.add('active');
    drawer.classList.add('active');
    document.body.classList.add('modal-open');

    // 秒开：先用看板缓存显示标题；缓存不含 files，仅用于即时反馈。
    const cachedVideo = (_state.currentBoardVideos && _state.currentBoardVideos[`${bvid}:${historyId}`])
        || (_state.currentBoardVideos && _state.currentBoardVideos[bvid]) || null;
    if (cachedVideo && cachedVideo.title) {
        titleEl.textContent = cachedVideo.title;
    }

    // 始终从后端拉取单视频详情：只有它带完整的 files / burned / blogger。
    // 看板缓存不含 files，直接用会导致“已下载文件”只剩兜底 sidecar、且不渲染烧录按钮。
    let video = null;
    try {
        const historyQuery = historyId == null ? '' : `&history_id=${encodeURIComponent(historyId)}`;
        const result = await apiGet(`/api/history/list?bvid=${encodeURIComponent(bvid)}${historyQuery}`, {
            signal: controller.signal,
        });
        if (requestId !== _state.drawerRequestId || _state.currentDrawerBvid !== bvid) return;
        if (result.code === 0 && result.data?.video) {
            video = result.data.video;
        } else if (result.offline) {
            // 网络错误：优先退回缓存渲染；无缓存则关闭抽屉并提示
            if (cachedVideo) {
                video = cachedVideo;
            } else {
                closeVideoDrawer();
                showToast(_NETWORK_ERR_MSG, 'error');
                return;
            }
        }
    } catch (e) {
        if (e?.name === 'AbortError') return;
        // 非网络异常：落到下面的兜底
    }

    // 后端无记录时的兜底：看板缓存 → 高频下载状态缓存
    if (!video && cachedVideo) {
        video = cachedVideo;
    }
    if (!video && _state.currentDownloadStatuses) {
        // status 的 key 是 bvid 或 bvid_type，这里按精确匹配 + 前缀匹配
        const status = _state.currentDownloadStatuses[bvid]
            || _state.currentDownloadStatuses[`${bvid}_video`]
            || _state.currentDownloadStatuses[`${bvid}_audio`];
        if (status) {
            // download/status 字段较少，补齐为统一结构
            video = {
                bvid: status.bvid || bvid,
                title: status.title || '未知标题',
                state: status.status || 'completed',
                task: {
                    status: status.status,
                    progress_percent: status.progress_percent || 0,
                    speed: status.speed || 0,
                    downloaded_size: status.downloaded_size || 0,
                    total_size: status.total_size || 0,
                },
            };
        }
    }

    if (requestId !== _state.drawerRequestId || _state.currentDrawerBvid !== bvid) return;
    if (!video) {
        titleEl.textContent = '视频信息加载失败';
        const bodyEl = document.getElementById('drawer-body');
        if (bodyEl) {
            bodyEl.innerHTML = `
                <div class="empty-state drawer-empty-state">
                    <i class="fa-solid fa-exclamation-circle drawer-error-icon"></i>
                    <p class="drawer-empty-message">未找到该视频记录</p>
                    <p class="empty-hint">该视频可能尚未入库或已被删除</p>
                </div>`;
        }
        return;
    }

    titleEl.textContent = video.title || '未知标题';
    await renderDrawerContent(video, bvid);
    if (requestId === _state.drawerRequestId) _state.drawerController = null;
}

// 关闭视频详情抽屉。
export function closeVideoDrawer() {
    const drawer = document.getElementById('video-drawer');
    const overlay = document.getElementById('drawer-overlay');

    _state.drawerRequestId++;
    _state.drawerController?.abort();
    _state.drawerController = null;
    overlay?.classList.remove('active');
    drawer?.classList.remove('active');
    document.body.classList.remove('modal-open');
    _state.currentDrawerBvid = null;
    _state.currentDrawerHistoryId = null;
}

// 从手动查询界面打开视频详情抽屉。
export function openVideoDrawerFromManual(bvid) {
    // 从手动查询的全局变量中获取视频信息
    if (!_state.manualQueryVideos || !_state.manualQueryVideos[bvid]) {
        showToast('视频信息加载失败', 'error');
        return;
    }

    _state.drawerRequestId++;
    _state.drawerController?.abort();
    _state.drawerController = null;
    const video = _state.manualQueryVideos[bvid];
    const drawer = document.getElementById('video-drawer');
    const overlay = document.getElementById('drawer-overlay');
    const titleEl = document.getElementById('drawer-video-title');

    _state.currentDrawerBvid = bvid;

    // 设置标题
    titleEl.textContent = video.title || '未知标题';

    // 渲染抽屉内容（手动查询版本）
    renderDrawerContentForManualQuery(video, bvid);

    // 显示抽屉
    overlay.classList.add('active');
    drawer.classList.add('active');
    document.body.classList.add('modal-open');
}

// 渲染抽屉内容
// 所有可选画质（静态列表，会根据视频实际可用质量动态禁用），供 media-actions.js 复用。
// 注意：不含 126（杜比视界）/127（8K）——后端下载白名单最高 125，选了必被 400 拒绝；
// 这两档在设置页全局画质下拉中保留但标注"仅在线观看可用，下载暂不支持"。
export const _ALL_QUALITY_OPTIONS = [
    { qn: 125, label: 'HDR', tag: 'HDR' },
    { qn: 120, label: '4K 超清', tag: '4K' },
    { qn: 116, label: '1080P60', tag: '60帧' },
    { qn: 112, label: '1080P+ 高码率', tag: '高码' },
    { qn: 80, label: '1080P 高清', tag: '1080P' },
    { qn: 74, label: '720P60', tag: '60帧' },
    { qn: 64, label: '720P 高清', tag: '720P' },
    { qn: 32, label: '480P 清晰', tag: '480P' },
    { qn: 16, label: '360P 流畅', tag: '360P' }
];

// 根据视频实际可用质量，更新 quality pills 的可用/禁用状态。
export async function refreshQualityPills(bvid) {
    const container = document.getElementById('quality-pills-container');
    if (!container) return;

    try {
        const result = await apiPost('/api/video/get-video-urls', { bvid });
        const data = result.data || {};
        if (result.code !== 0 || !data.qualities) return;

        // 收集实际可用的质量 ID（视频真正拥有的流）
        const availableQualities = new Set(data.qualities.map(q => q.quality));
        // 收集用户权限允许的质量 ID
        const acceptQuality = new Set((data.accept_quality || []).map(q => q));

        // 查找最高可用质量（兜底：用户默认画质不可用/无权限时退回最高可用档）
        let bestAvailable = 80;
        for (const q of _ALL_QUALITY_OPTIONS) {
            if (availableQualities.has(q.qn) && acceptQuality.has(q.qn) && q.qn > bestAvailable) {
                bestAvailable = q.qn;
            }
        }

        // 默认选中：服务端 default_quality（= 设置 query.video_quality，默认 80/1080P）优先，
        // 取不超过它的最高可用档；全部不可用时退回最高可用档（保持原行为）。
        const defaultQn = Number(data.default_quality) || 80;
        let selected = [..._ALL_QUALITY_OPTIONS]
            .sort((a, b) => b.qn - a.qn)
            .find(q => q.qn <= defaultQn && availableQualities.has(q.qn) && acceptQuality.has(q.qn))?.qn;
        if (!selected) selected = bestAvailable;

        const pills = container.querySelectorAll('.quality-pill');
        pills.forEach(pill => {
            const qn = parseInt(pill.dataset.qn);
            const isAvailable = availableQualities.has(qn) && acceptQuality.has(qn);
            if (isAvailable) {
                pill.classList.remove('disabled');
            } else {
                pill.classList.add('disabled');
                pill.classList.remove('active');
            }
        });

        // 设置默认选中（服务端默认画质优先，其次最高可用）
        _state.selectedQuality = selected;
        const defaultPill = container.querySelector(`.quality-pill[data-qn="${selected}"]`);
        if (defaultPill) {
            container.querySelectorAll('.quality-pill').forEach(p => p.classList.remove('active'));
            defaultPill.classList.add('active');
        }
    } catch (e) {
        // 获取失败时保持默认状态（全部可选），不影响用户操作
    }
}
