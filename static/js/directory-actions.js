import { apiPost } from './core.js';
import { showToast } from './download-status.js';

export async function openHistoryDirectory(bvid, path = '', historyId = undefined) {
    if (!bvid) return;
    try {
        await apiPost('/api/history/open-directory', { bvid, history_id: historyId ?? null, path: path || null });
        showToast('已打开文件所在目录', 'success');
    } catch (error) {
        showToast(`打开目录失败：${error.message}`, 'error');
    }
}
