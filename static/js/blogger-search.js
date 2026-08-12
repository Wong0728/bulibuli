import { _state, _NETWORK_ERR_MSG } from './state.js';
import { escapeHtml } from './utils.js';
import { apiPost, apiGet } from './core.js';
import { loadManualSeriesList } from './manual.js';
import { showToast, confirmDialog } from './download-status.js';

// --- 博主搜索与已添加博主管理 ---
// “已添加博主”与“自动任务”是两个独立集合，前端不做本地持久化。

// 从后端加载已知博主列表
export async function loadKnownBloggers() {
    try {
        const result = await apiGet('/api/blogger/saved/list');
        if (result.code === 0 && Array.isArray(result.data?.bloggers)) {
            return result.data.bloggers;
        }
        return [];
    } catch (e) {
        showToast('加载已添加博主失败，请检查网络', 'error');
        return [];
    }
}

// 生成博主富内容 option HTML（头像 + 名称 + UID/等级/粉丝），用于 base-select 下拉
// 不支持 base-select 的浏览器会忽略 HTML 子元素，只显示 textContent 作为回退
export function buildBloggerOption(b) {
    const name = escapeHtml(b.name || '未知博主');
    const uid = escapeHtml(b.uid);
    const face = b.face ? `/api/video/proxy-image?url=${encodeURIComponent(b.face)}` : '';
    const meta = `UID: ${uid} · Lv${escapeHtml(b.level || 0)} · 粉丝 ${formatFans(b.fans)}`;
    const avatarHtml = face ? `<img class="opt-avatar" src="${face}" alt="">` : '';
    return `<option value="${uid}" data-name="${name}">${avatarHtml}<span class="opt-info"><span class="opt-name">${name}</span><span class="opt-meta">${meta}</span></span></option>`;
}

// 渲染手动查询页的博主快捷选择下拉 - 异步版本
export async function renderUidHistorySelect() {
    const select = document.getElementById('uid-history-select');
    if (!select) return;
    const list = await loadKnownBloggers();
    select.innerHTML = '<option value="">-- 从已添加博主中选择 --</option>' +
        list.map(b => buildBloggerOption(b)).join('');
    select.value = '';
}

// 手动查询页下拉选中后填入 UID 输入框（合集模式下自动加载合集列表）
export async function onUidHistorySelectChange() {
    const select = document.getElementById('uid-history-select');
    if (!select) return;
    const uid = select.value;
    // 按合集模式下，选择博主后自动拉取合集列表
    if (_state.manualQueryMode === 'series' && uid) {
        await loadManualSeriesList(uid);
    }
}

export async function addBloggerToKnown(blogger) {
    const uid = String(blogger.uid);
    try {
        const list = await loadKnownBloggers();
        const existing = list.find(item => String(item.uid) === uid);
        if (existing) {
            showToast(`博主 ${existing.name || uid} 已在列表中`, 'info');
            return existing;
        }

        // 这里只收藏搜索结果，不创建自动任务。
        const result = await apiPost('/api/blogger/saved/add', {
            uid,
            name: blogger.name || '',
            face: blogger.face || '',
            sign: blogger.sign || '',
            level: blogger.level || 0,
            fans: blogger.fans || 0
        });

        await Promise.all([
            renderKnownBloggers(),
            renderUidHistorySelect()
        ]);
        showToast(`已添加博主 ${blogger.name || uid}`, 'success');
        return result.data?.blogger;
    } catch (error) {
        if (!error.offline) {
            showToast(error.message || '添加博主失败', 'error');
        }
        return null;
    }
}

export async function removeKnownBlogger(uid) {
    const blogger = (await loadKnownBloggers()).find(item => String(item.uid) === String(uid));
    if (!blogger) return;
    await apiPost('/api/blogger/saved/delete', { id: blogger.id });
    await renderKnownBloggers();
    await renderUidHistorySelect();
}

// 清空已添加博主；不会修改任何自动任务。
export async function clearKnownBloggers() {
    if (!(await confirmDialog('确定要清空已添加博主列表吗？已有自动任务不会受到影响。', { title: '清空列表', okText: '清空', danger: true }))) return;
    try {
        const list = await loadKnownBloggers();
        for (const blogger of list) {
            await apiPost('/api/blogger/saved/delete', { id: blogger.id });
        }
        await renderKnownBloggers();
        await renderUidHistorySelect();
        showToast('已清空已添加博主', 'success');
    } catch (e) {
        showToast('清空失败，请检查网络', 'error');
    }
}

export async function renderKnownBloggers() {
    const container = document.getElementById('known-blogger-list');
    if (!container) return;
    const list = await loadKnownBloggers();
    if (list.length === 0) {
        container.innerHTML = `
            <div class="empty-state empty-state-padded">
                <p>暂无已添加博主</p>
                <p class="empty-hint">使用上方搜索添加博主</p>
            </div>
        `;
        return;
    }
    container.innerHTML = list.map(b => `
        <div class="known-blogger-card">
            <img src="/api/video/proxy-image?url=${encodeURIComponent(b.face)}" class="blogger-avatar" alt="" data-image-error="hide">
            <div class="blogger-info">
                <div class="blogger-name">${escapeHtml(b.name)}</div>
                <div class="blogger-meta">UID: ${escapeHtml(b.uid)} · Lv${escapeHtml(b.level)} · 粉丝 ${formatFans(b.fans)}</div>
            </div>
            <button class="btn btn-sm btn-danger" data-action="remove-known-blogger" data-uid="${escapeHtml(b.uid)}">
                <i class="fa-solid fa-times"></i>
            </button>
        </div>
    `).join('');
}

export function formatFans(n) {
    n = Number(n) || 0;
    if (n >= 10000) return (n/10000).toFixed(1) + '万';
    return n.toString();
}

// --- 博主资料变更通知（黄点） ---

// 检查所有博主的资料变更通知状态，更新博主搜索页右上角黄点。
// 黄点表示后端存在 blogger.last_seen_at 非空的未确认资料变更。
export async function checkBloggerProfileNotices() {
    try {
        const result = await apiGet('/api/blogger/saved/list');
        const data = result.data || {};
        if (result.code !== 0 || !Array.isArray(data.bloggers)) {
            hideBloggerNoticeDot();
            return;
        }
        // 筛出有变更通知的博主
        const changed = data.bloggers.filter(b => b.notice_visible);
        const btn = document.getElementById('blogger-notice-dot');
        const badge = document.getElementById('blogger-notice-count');
        if (!btn || !badge) return;
        if (changed.length === 0) {
            btn.hidden = true;
            return;
        }
        btn.hidden = false;
        badge.textContent = changed.length;
        // 缓存给 modal 使用。
        _state.bloggerProfileChanges = changed;
    } catch (e) {
        // 静默失败
        hideBloggerNoticeDot();
    }
}

export function hideBloggerNoticeDot() {
    const btn = document.getElementById('blogger-notice-dot');
    if (btn) btn.hidden = true;
}

// 显示博主资料变更模态框。
export function showBloggerNoticeModal() {
    const list = _state.bloggerProfileChanges || [];
    const container = document.getElementById('blogger-notice-list');
    if (!container) return;
    if (list.length === 0) {
        container.innerHTML = `<div class="empty-state notice-empty"><p>暂无资料变更通知</p></div>`;
    } else {
        container.innerHTML = list.map(b => {
            const time = b.last_seen_at ? new Date(b.last_seen_at).toLocaleString() : '';
            const faceOld = b.last_seen_face ? `/api/video/proxy-image?url=${encodeURIComponent(b.last_seen_face)}` : '';
            const faceNew = b.face ? `/api/video/proxy-image?url=${encodeURIComponent(b.face)}` : '';
            const nameChanged = b.last_seen_name && b.last_seen_name !== b.name;
            const faceChanged = b.last_seen_face && b.last_seen_face !== b.face;
            const changes = [];
            if (nameChanged) changes.push('改名');
            if (faceChanged) changes.push('改头像');
            const changeLabel = changes.length > 0 ? changes.join('、') : '资料变更';
            return `
                <div class="blogger-notice-item">
                    <div class="blogger-notice-header">
                        <span class="blogger-notice-name">${escapeHtml(b.name || b.uid)}</span>
                        <span class="blogger-notice-tag">${changeLabel}</span>
                        <span class="blogger-notice-time">${time}</span>
                    </div>
                    <div class="blogger-notice-compare">
                        <div class="blogger-notice-col">
                            <div class="blogger-notice-label">旧</div>
                            ${faceOld ? `<img src="${faceOld}" class="blogger-avatar-sm" data-image-error="hide">` : '<div class="blogger-avatar-sm blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>'}
                            <div class="blogger-notice-name-old">${escapeHtml(b.last_seen_name || '--')}</div>
                        </div>
                        <div class="blogger-notice-arrow"><i class="fa-solid fa-arrow-right"></i></div>
                        <div class="blogger-notice-col">
                            <div class="blogger-notice-label">新</div>
                            ${faceNew ? `<img src="${faceNew}" class="blogger-avatar-sm" data-image-error="hide">` : '<div class="blogger-avatar-sm blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>'}
                            <div class="blogger-notice-name-new">${escapeHtml(b.name || '--')}</div>
                        </div>
                    </div>
                    <button class="btn btn-sm btn-ghost" data-action="acknowledge-blogger-change" data-uid="${escapeHtml(b.uid)}">
                        <i class="fa-solid fa-check"></i> 知道了
                    </button>
                </div>
            `;
        }).join('');
    }
    const modal = document.getElementById('blogger-notice-modal');
    if (modal) modal.classList.add('active');
}

export function closeBloggerNoticeModal() {
    const modal = document.getElementById('blogger-notice-modal');
    if (modal) modal.classList.remove('active');
}

// 单个博主“知道了”：调用后端 acknowledge，然后刷新黄点和模态框列表。
export async function acknowledgeBloggerChange(uid) {
    try {
        const result = await apiPost('/api/blogger/acknowledge', { uid });
        if (result.code !== 0) {
            showToast(result.message || '确认失败', 'error');
            return;
        }
        // 从缓存移除。
        if (_state.bloggerProfileChanges) {
            _state.bloggerProfileChanges = _state.bloggerProfileChanges.filter(b => b.uid !== uid);
        }
        await checkBloggerProfileNotices();
        // 若已无变更通知则关闭 modal，否则刷新列表。
        const remaining = (_state.bloggerProfileChanges || []).length;
        if (remaining === 0) {
            closeBloggerNoticeModal();
        } else {
            showBloggerNoticeModal();
        }
        showToast('已确认', 'success');
    } catch (e) {
        showToast('确认失败', 'error');
    }
}

// 全部“知道了”：批量调用后端 acknowledge。
export async function acknowledgeAllBloggerChanges() {
    const list = _state.bloggerProfileChanges || [];
    if (list.length === 0) {
        closeBloggerNoticeModal();
        return;
    }
    try {
        const result = await apiPost('/api/blogger/acknowledge-batch', {
            uids: list.map(blogger => blogger.uid),
        });
        _state.bloggerProfileChanges = [];
        await checkBloggerProfileNotices();
        closeBloggerNoticeModal();
        showToast(`已确认 ${result.data?.acknowledged || 0} 条`, 'success');
    } catch (error) {
        showToast(`批量确认失败: ${error.message}`, 'error', 5000);
    }
}

_state.searchBloggersLock = false;
export async function searchBloggers() {
    if (_state.searchBloggersLock) return; // 防抖：请求期间不可重复发起
    const input = document.getElementById('blogger-search-input');
    const q = input.value.trim();
    const resultsEl = document.getElementById('blogger-search-results');
    const searchBtn = input.parentElement?.querySelector('.btn-primary');
    if (!q) {
        showToast('请输入搜索关键字', 'error');
        return;
    }
    _state.searchBloggersLock = true;
    if (searchBtn) { searchBtn.disabled = true; searchBtn.innerHTML = '<span class="loading"></span> 搜索中'; }
    resultsEl.innerHTML = '<div class="loading search-loading"></div>';

    const isAllDigits = /^\d+$/.test(q);
    let uidCardHtml = '';

    // 纯数字输入：优先按 UID 精确查找
    if (isAllDigits) {
        try {
            const uidResult = await apiGet(`/api/blogger/validate-uid?uid=${q}`);
            const uidData = uidResult.data || {};
            if (uidResult.code === 0 && uidData.exists) {
                const u = uidData;
                const faceUrl = u.face ? `/api/video/proxy-image?url=${encodeURIComponent(u.face)}` : '';
                uidCardHtml = `
                <div class="blogger-search-card uid-exact-match">
                    ${faceUrl ? `<img src="${faceUrl}" class="blogger-avatar" alt="" data-image-error="hide">` : '<div class="blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>'}
                    <div class="blogger-info">
                        <div class="blogger-name">${escapeHtml(u.name)} <span class="uid-match-badge">UID 精确匹配</span></div>
                        <div class="blogger-meta">UID: ${escapeHtml(String(u.uid))} · Lv${Number(u.level) || 0} · 粉丝 ${formatFans(Number(u.fans) || 0)}</div>
                        <div class="blogger-sign">${escapeHtml(u.sign || '')}</div>
                    </div>
                    <div class="blogger-search-actions">
                        <button class="btn btn-sm btn-primary" data-uid-add="${escapeHtml(String(u.uid))}" data-name="${escapeHtml(u.name)}" data-face="${escapeHtml(u.face || '')}" data-level="${Number(u.level) || 0}" data-fans="${Number(u.fans) || 0}">
                            <i class="fa-solid fa-plus"></i> 添加
                        </button>
                    </div>
                </div>`;
            }
        } catch (e) {
            // UID 查找失败不影响后续名称搜索，但记录警告
            console.warn('UID 精确查找失败:', e);
        }
    }

    // 名称搜索（对纯数字也搜索包含该数字的名称）
    try {
        const result = await apiGet(`/api/blogger/search?q=${encodeURIComponent(q)}`);

        // 区分请求失败和无结果
        if (result.code !== 0) {
            if (result.offline) {
                // 网络错误：统一右上角 toast + 顶栏横幅，不在结果区内联渲染网络错误
                showToast(_NETWORK_ERR_MSG, 'error');
                resultsEl.innerHTML = uidCardHtml || `<div class="empty-state empty-state-padded"><i class="fa-solid fa-magnifying-glass"></i><p>暂无结果，请稍后重试</p></div>`;
            } else if (!uidCardHtml) {
                resultsEl.innerHTML = `<div class="empty-state empty-state-padded"><p class="status-error"><i class="fa-solid fa-exclamation-circle"></i> ${escapeHtml(result.message || '搜索失败')}</p></div>`;
            } else {
                resultsEl.innerHTML = uidCardHtml;
                showToast(result.message || '名称搜索失败', 'warning');
            }
            return;
        }

        const users = result.data?.users || [];

        if (!uidCardHtml && users.length === 0) {
            resultsEl.innerHTML = '<div class="empty-state empty-state-padded"><p>未找到匹配的博主</p></div>';
            return;
        }

        let html = uidCardHtml;
        if (users.length > 0) {
            html += users.map((u, idx) => {
                return `
                <div class="blogger-search-card">
                    <img src="/api/video/proxy-image?url=${encodeURIComponent(u.upic)}" class="blogger-avatar" alt="" data-image-error="hide">
                    <div class="blogger-info">
                        <div class="blogger-name">${escapeHtml(u.uname)}</div>
                        <div class="blogger-meta">UID: ${escapeHtml(String(u.mid))} · Lv${Number(u.level) || 0} · 粉丝 ${formatFans(Number(u.fans) || 0)} · 视频 ${Number(u.videos) || 0}</div>
                        <div class="blogger-sign">${escapeHtml(u.sign || '')}</div>
                    </div>
                    <div class="blogger-search-actions">
                        <button class="btn btn-sm btn-primary" data-add-blogger="${idx}">
                            <i class="fa-solid fa-plus"></i> 添加
                        </button>
                    </div>
                </div>`;
            }).join('');
        }
        resultsEl.innerHTML = html;

        // 绑定 UID 精确匹配卡片的添加按钮
        resultsEl.querySelectorAll('[data-uid-add]').forEach(btn => {
            btn.addEventListener('click', () => {
                addBloggerToKnown({
                    uid: btn.dataset.uidAdd,
                    name: btn.dataset.name,
                    face: btn.dataset.face,
                    level: parseInt(btn.dataset.level) || 0,
                    fans: parseInt(btn.dataset.fans) || 0
                });
            });
        });

        // 绑定名称搜索结果的添加按钮
        resultsEl.querySelectorAll('[data-add-blogger]').forEach(btn => {
            const idx = parseInt(btn.dataset.addBlogger);
            const u = users[idx];
            if (u) {
                btn.addEventListener('click', () => {
                    addBloggerToKnown({uid: String(u.mid), name: u.uname, face: u.upic, level: u.level, fans: u.fans});
                });
            }
        });
    } catch (e) {
        if (uidCardHtml) {
            resultsEl.innerHTML = uidCardHtml;
        } else {
            resultsEl.innerHTML = '<div class="empty-state empty-state-padded"><p>搜索请求失败</p></div>';
        }
    } finally {
        _state.searchBloggersLock = false;
        if (searchBtn) { searchBtn.disabled = false; searchBtn.innerHTML = '<i class="fa-solid fa-search"></i> 搜索'; }
    }
}
