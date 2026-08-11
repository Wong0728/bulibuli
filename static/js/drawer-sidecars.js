import { _NETWORK_ERR_MSG } from './state.js';
import { escapeHtml } from './utils.js';
import { apiGet } from './core.js';
import { showToast } from './download-status.js';

export async function loadDrawerComments(bvid, path = '') {
    const container = document.getElementById('drawer-sidecar-viewer') || document.getElementById('drawer-comments');
    if (!container) return;
    markSelectedSidecarPath(path);
    container.innerHTML = '<div class="drawer-comments-hint"><i class="fa-solid fa-spinner fa-spin"></i> 加载中...</div>';
    try {
        const pathQuery = path ? `&path=${encodeURIComponent(path)}` : '';
        const result = await apiGet(`/api/video/comments?bvid=${encodeURIComponent(bvid)}${pathQuery}`);
        if (result.code !== 0) {
            if (result.offline) {
                showToast(_NETWORK_ERR_MSG, 'error');
                container.innerHTML = '<div class="drawer-comments-hint">暂不可用</div>';
            } else {
                container.innerHTML = `<div class="drawer-comments-hint" data-js-style="1">${escapeHtml(result.message || '加载失败')}</div>`;
            }
            return;
        }
        const data = result.data || {};
        if (!data.exists) {
            container.innerHTML = '<div class="drawer-comments-hint"><i class="fa-solid fa-comment-slash"></i> 未下载评论（可先点下方“下载评论”，完成后再查看）</div>';
            return;
        }
        if (data.format === 'json' && Array.isArray(data.comments)) {
            if (data.comments.length === 0) {
                container.innerHTML = '<div class="drawer-comments-hint"><i class="fa-solid fa-comment-slash"></i> 暂无评论内容</div>';
                return;
            }
            container.innerHTML = `<div class="drawer-comments-list">${data.comments.map(renderDrawerCommentCard).join('')}</div>`;
            return;
        }
        if (data.content) {
            container.innerHTML = '<div class="drawer-comments-hint"><i class="fa-solid fa-file-lines"></i>该评论版本为原始文本，前端不提供直接查看</div>';
            return;
        }
        container.innerHTML = '<div class="drawer-comments-hint"><i class="fa-solid fa-comment-slash"></i> 暂无评论内容</div>';
    } catch (e) {
        container.innerHTML = '<div class="drawer-comments-hint">暂不可用</div>';
    }
}

// 加载并展示指定弹幕文件。结构化 JSON 以时间轴列表显示，XML/TXT 不展开原始文本。
export async function loadDrawerDanmaku(bvid, path) {
    const container = document.getElementById('drawer-sidecar-viewer');
    if (!container || !path) return;
    markSelectedSidecarPath(path);
    container.innerHTML = '<div class="drawer-comments-hint"><i class="fa-solid fa-spinner fa-spin"></i> 加载弹幕中...</div>';
    try {
        const result = await apiGet(`/api/video/danmaku?bvid=${encodeURIComponent(bvid)}&path=${encodeURIComponent(path)}`);
        if (result.code !== 0) {
            container.innerHTML = `<div class="drawer-comments-hint">${escapeHtml(result.message || '加载弹幕失败')}</div>`;
            return;
        }
        const data = result.data || {};
        if (data.format === 'json' && Array.isArray(data.danmaku)) {
            const total = data.danmaku.length;
            const rows = data.danmaku.map(renderDanmakuRow).join('');
            container.innerHTML = `
                <div class="drawer-sidecar-result-title">
                    <span><i class="fa-solid fa-comment-dots"></i> ${total} 条弹幕</span>
                    <span>${escapeHtml(path)}</span>
                </div>
                <div class="drawer-danmaku-list">${rows || '<div class="drawer-comments-hint">暂无弹幕内容</div>'}</div>
            `;
            return;
        }
        container.innerHTML = '<div class="drawer-comments-hint"><i class="fa-solid fa-file-lines"></i>该弹幕版本为原始文本，前端不提供直接查看</div>';
    } catch (e) {
        container.innerHTML = '<div class="drawer-comments-hint">弹幕暂不可用</div>';
    }
}

function markSelectedSidecarPath(path) {
    document.querySelectorAll('.sidecar-version-btn').forEach(button => {
        button.classList.toggle('active', Boolean(path) && button.dataset.path === path);
    });
}

function renderDanmakuRow(item) {
    const progress = Number(item?.progress ?? item?.time ?? item?.ctime ?? 0);
    const seconds = progress > 10000 ? progress / 1000 : progress;
    const timestamp = `${Math.max(0, Math.floor(seconds / 60))}:${String(Math.max(0, Math.floor(seconds % 60))).padStart(2, '0')}`;
    const content = item?.content ?? item?.text ?? item?.message ?? '';
    const user = item?.mid_hash ?? item?.user_hash ?? '';
    return `
        <div class="drawer-danmaku-row">
            <span class="drawer-danmaku-time">${timestamp}</span>
            <span class="drawer-danmaku-text">${escapeHtml(String(content))}</span>
            ${user ? `<span class="drawer-danmaku-user">${escapeHtml(String(user))}</span>` : ''}
        </div>
    `;
}

// 渲染单条评论卡片（主评论 + 缩进回复）
export function renderDrawerCommentCard(c) {
    const fmtTime = ts => {
        if (!ts) return '';
        const d = new Date(ts * 1000);
        return Number.isNaN(d.getTime()) ? '' : d.toLocaleString();
    };
    const uname = escapeHtml(c.uname || '');
    const vip = Number(c.vip_status || 0) > 0 ? `<span class="cmt-vip">${escapeHtml(c.vip_label || '大会员')}</span>` : '';
    const userStyle = /^#[0-9a-f]{6}$/i.test(c.name_color || '') ? ` style="color:${escapeHtml(c.name_color)}"` : '';
    const replies = Array.isArray(c.replies) ? c.replies : [];
    const repliesHtml = replies.length
        ? `<div class="cmt-replies"><div class="cmt-replies-title">回复 · 显示 ${replies.length}/${c.total_replies || 0} 条</div>${replies.map(reply => {
            const replyStyle = /^#[0-9a-f]{6}$/i.test(reply.name_color || '') ? ` style="color:${escapeHtml(reply.name_color)}"` : '';
            const replyVip = Number(reply.vip_status || 0) > 0 ? `<span class="cmt-vip">${escapeHtml(reply.vip_label || '大会员')}</span>` : '';
            return `<div class="cmt-reply"><div class="cmt-line"><span class="cmt-user"${replyStyle}>${escapeHtml(reply.uname || '')}</span>${replyVip}<span class="cmt-lv">Lv${reply.level || 0}</span><span class="cmt-meta"><i class="fa-solid fa-thumbs-up"></i> ${reply.like || 0} · ${fmtTime(reply.ctime)}</span></div><div class="cmt-text">${escapeHtml(reply.message || '')}</div></div>`;
        }).join('')}</div>`
        : '';
    return `<div class="cmt-card"><div class="cmt-line"><span class="cmt-user"${userStyle}>${uname}</span>${vip}<span class="cmt-lv">Lv${c.level || 0}</span><span class="cmt-meta"><i class="fa-solid fa-thumbs-up"></i> ${c.like || 0} · <i class="fa-solid fa-comment"></i> ${c.total_replies || 0} · ${fmtTime(c.ctime)}</span></div><div class="cmt-text">${escapeHtml(c.message || '')}</div>${repliesHtml}</div>`;
}
