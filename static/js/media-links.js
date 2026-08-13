import { _state } from './state.js';
import { escapeHtml } from './utils.js';
import { setTone, checkNetworkBeforeAction, apiPost } from './core.js';
import { updateDownloadProgressInList } from './history.js';
import { showToast } from './download-status.js';

// --- 下载链接管理（支持多清晰度） ---
export function startUrlExpiryTimer(bvid, title, expirySeconds = 1800) {
    _state.videoTitles[bvid] = title;
    const expiryTime = Date.now() + expirySeconds * 1000;
    _state.urlExpiryTimers[bvid] = { expiryTime };
    ensureUrlExpiryTicker();
}

function ensureUrlExpiryTicker() {
    if (_state.urlExpiryInterval) return;
    _state.urlExpiryInterval = setInterval(() => {
        const entries = Object.entries(_state.urlExpiryTimers || {});
        entries.forEach(([bvid, timer]) => {
            const remaining = Math.max(0, Math.floor((timer.expiryTime - Date.now()) / 1000));
            const timerElement = document.querySelector(
                `[data-role="url-expiry"][data-bvid="${CSS.escape(bvid)}"]`,
            );

            if (timerElement) {
                if (remaining > 0) {
                    const minutes = Math.floor(remaining / 60);
                    const seconds = remaining % 60;
                    timerElement.textContent = `${minutes}:${seconds.toString().padStart(2, '0')}`;

                    if (remaining < 60) {
                        setTone(timerElement, 'error');
                        timerElement.classList.add('text-strong');
                    }
                } else {
                    timerElement.textContent = '已过期';
                    setTone(timerElement, 'error');
                    delete _state.urlExpiryTimers[bvid];

                    const expiryContainer = timerElement.closest('.download-expiry');
                    const storedTitle = (_state.videoTitles[bvid] || '').replace(/[^\w\u4e00-\u9fff\-_. ]/g, '');
                    if (expiryContainer) {
                        expiryContainer.innerHTML = `
                            <span class="status-error"><i class="fa-solid fa-exclamation-triangle"></i> 已过期</span>
                            <button class="btn btn-sm btn-ghost" data-action="get-download-links" data-bvid="${bvid}" data-title="${escapeHtml(storedTitle)}">
                                <i class="fa-solid fa-sync-alt"></i> 重新获取
                            </button>
                        `;
                    }
                }
            }
        });
        if (Object.keys(_state.urlExpiryTimers || {}).length === 0) {
            clearInterval(_state.urlExpiryInterval);
            _state.urlExpiryInterval = null;
        }
    }, 1000);
}

export async function getDownloadLinks(bvid, title) {
    const actionsDiv = document.getElementById(`actions-${bvid}`);

    actionsDiv.innerHTML = '<span class="loading"></span> 获取中...';

    try {
        const [videoRequest, audioRequest] = await Promise.allSettled([
            apiPost('/api/video/get-video-urls', { bvid }),
            apiPost('/api/video/get-audio-url', { bvid }),
        ]);
        const videoResult = videoRequest.status === 'fulfilled'
            ? videoRequest.value
            : { code: -1, message: videoRequest.reason?.message || '视频链接获取失败', data: {} };
        const audioResult = audioRequest.status === 'fulfilled'
            ? audioRequest.value
            : { code: -1, message: audioRequest.reason?.message || '音频链接获取失败', data: {} };
        const videoData = videoResult.data || {};
        const audioData = audioResult.data || {};

        if (videoResult.code === 0 || audioResult.code === 0) {
            const safeTitle = title.replace(/[^\w\u4e00-\u9fff\-_. ]/g, '');
            let html = '<div class="download-panel">';

            // 视频下载行
            if (videoResult.code === 0 && videoData.qualities && videoData.qualities.length > 0) {
                const acceptQuality = videoData.accept_quality || [];
                let defaultQualityIndex = -1;
                let defaultQualityOriginalIndex = -1;

                // 保存原始的视频流列表
                _state.videoTitles[bvid + '_qualities'] = videoData.qualities;

                // 构建下拉菜单，记录每个可用流的原始索引
                let qualityOptions = [];
                videoData.qualities.forEach((q, originalIndex) => {
                    const isAvailable = acceptQuality.includes(q.quality);
                    
                    if (isAvailable) {
                        // 记录第一个可用的视频流作为默认选项
                        if (defaultQualityOriginalIndex === -1) {
                            defaultQualityOriginalIndex = originalIndex;
                        }
                        
                        qualityOptions.push({
                            originalIndex: originalIndex,
                            quality: q,
                            isAvailable: true
                        });
                    } else {
                        qualityOptions.push({
                            originalIndex: originalIndex,
                            quality: q,
                            isAvailable: false
                        });
                    }
                });

                html += '<div class="download-row">';
                html += '<span class="download-row-label">视频</span>';
                html += `<select data-role="video-quality" data-bvid="${bvid}" class="form-control download-quality-select">`;
                
                qualityOptions.forEach((opt, index) => {
                    if (opt.isAvailable) {
                        const selected = opt.originalIndex === defaultQualityOriginalIndex ? ' selected' : '';
                        html += `<option value="${opt.originalIndex}"${selected}>${escapeHtml(opt.quality.quality_name)} ${opt.quality.width}x${opt.quality.height}</option>`;
                    } else {
                        // 灰色标记，禁用选项
                        html += `<option value="${opt.originalIndex}" disabled class="premium-required">${escapeHtml(opt.quality.quality_name)} ${opt.quality.width}x${opt.quality.height} (需要大会员)</option>`;
                    }
                });
                
                html += '</select>';
                
                // 只有有可用视频流时才显示下载按钮
                if (defaultQualityOriginalIndex >= 0) {
                    html += `<button class="btn btn-sm btn-primary manual-download-btn" title="适合小文件，大文件建议使用下载器" data-action="download-video" data-bvid="${bvid}" data-title="${safeTitle}" data-mode="browser"><i class="fa-solid fa-desktop"></i> 浏览器下载</button>`;
                    html += `<button class="btn btn-sm btn-primary manual-download-btn" data-role="manual-video-download" data-action="download-video" data-bvid="${bvid}" data-title="${safeTitle}" data-mode="server"><i class="fa-solid fa-server"></i> 下载器下载</button>`;
                } else {
                    html += `<span class="premium-required-inline">无可用视频流（需要登录或大会员）</span>`;
                }
                
                html += '</div>';
            }

            // 音频下载行（多音质）
            if (audioResult.code === 0) {
                const audioQualities = audioData.qualities || [];
                html += '<div class="download-row">';
                html += '<span class="download-row-label">音频</span>';

                if (audioQualities.length > 1) {
                    html += `<select data-role="audio-quality" data-bvid="${bvid}" class="form-control download-quality-select">`;
                    audioQualities.forEach((aq, index) => {
                        const kbps = Math.round((aq.bandwidth || 0) / 1000);
                        const label = formatAudioQuality(aq.id, kbps);
                        html += `<option value="${index}"${index === 0 ? ' selected' : ''}>${label}</option>`;
                    });
                    html += '</select>';
                } else {
                    html += '<span class="download-quality-single">默认音质</span>';
                }

                html += `<button class="btn btn-sm manual-download-btn" data-action="download-audio" data-bvid="${bvid}" data-title="${safeTitle}" data-mode="browser"><i class="fa-solid fa-music"></i> 浏览器下载</button>`;
                html += `<button class="btn btn-sm btn-primary manual-download-btn" data-role="manual-audio-download" data-action="download-audio" data-bvid="${bvid}" data-title="${safeTitle}" data-mode="server"><i class="fa-solid fa-music"></i> 下载器下载</button>`;
                html += '</div>';
            }

            // 分割线 + 其他操作
            html += '<div class="download-other-row">';
            html += `<a href="https://www.bilibili.com/video/${bvid}" target="_blank" class="btn btn-sm btn-ghost"><i class="fa-solid fa-external-link-alt"></i> 原视频</a>`;
            html += `<button class="btn btn-sm btn-ghost" data-role="manual-danmaku-download" data-action="download-danmaku" data-source="manual" data-bvid="${bvid}"><i class="fa-solid fa-comment-dots"></i> 弹幕</button>`;
            html += `<button class="btn btn-sm btn-ghost" data-role="manual-comments-download" data-action="download-comments" data-source="manual" data-bvid="${bvid}"><i class="fa-solid fa-comments"></i> 评论</button>`;
            html += `<span class="download-expiry"><i class="fa-solid fa-clock"></i> <span data-role="url-expiry" data-bvid="${bvid}">30:00</span></span>`;
            html += '</div>';

            html += '</div>'; // .download-panel
            actionsDiv.innerHTML = html;

            // 存储链接数据
            if (videoResult.code === 0) {
                _state.videoTitles[bvid + '_qualities'] = videoData.qualities || [];
            }
            if (audioResult.code === 0) {
                _state.videoTitles[bvid + '_audio_qualities'] = audioData.qualities || [];
                _state.videoTitles[bvid + '_audio_ext'] = audioData.ext || 'm4a';
            }

            startUrlExpiryTimer(bvid, title, 1800);
        } else {
            const errorMsg = videoResult.message || audioResult.message || '获取链接失败';
            actionsDiv.innerHTML = `<p class="status-error"><i class="fa-solid fa-exclamation-circle"></i> ${escapeHtml(errorMsg)}</p>`;
        }
    } catch (e) {
        actionsDiv.innerHTML = `<p class="status-error"><i class="fa-solid fa-exclamation-circle"></i> 获取链接失败: ${escapeHtml(e.message)}</p>`;
    }
}

// 格式化音频音质显示名称
export function formatAudioQuality(id, kbps) {
    const names = {
        30251: 'Hi-Res 无损',
        30250: '杜比全景声',
        30280: '192K',
        30232: '132K',
        30216: '64K',
    };
    if (names[id]) return names[id];
    return kbps > 0 ? `${kbps}K` : '未知音质';
}

// 音频多音质下载
export function downloadAudioWithQuality(bvid, title, target) {
    if (!checkNetworkBeforeAction()) return;
    const audioQualities = _state.videoTitles[bvid + '_audio_qualities'] || [];
    const ext = _state.videoTitles[bvid + '_audio_ext'] || 'm4a';
    const select = document.querySelector(`[data-role="audio-quality"][data-bvid="${CSS.escape(bvid)}"]`);
    const qualityIndex = select ? parseInt(select.value) : 0;
    const aq = audioQualities[qualityIndex];

    if (!aq || !aq.url) {
        showToast('获取音频链接失败', 'error');
        return;
    }

    const kbps = Math.round((aq.bandwidth || 0) / 1000);
    const qualityTag = formatAudioQuality(aq.id, kbps).replace(/\s+/g, '_');
    const filename = `${title}_${bvid}_${qualityTag}.${ext}`;

    if (target === 'browser') {
        downloadToBrowser(aq.url, filename);
    } else {
        downloadToServer(bvid, filename, 'audio', null, true, aq.url);
    }
}

export function downloadVideoWithQuality(bvid, title, target) {
    if (!checkNetworkBeforeAction()) return;
    const select = document.querySelector(`[data-role="video-quality"][data-bvid="${CSS.escape(bvid)}"]`);
    const qualityIndex = select ? select.value : 0;
    const qualities = _state.videoTitles[bvid + '_qualities'];
    
    if (!qualities || !qualities[qualityIndex]) {
        showToast('获取视频链接失败', 'error');
        return;
    }
    
    const quality = qualities[qualityIndex];
    const safeTitle = title.replace(/[^\w\u4e00-\u9fff\-_. ]/g, '');
    const filename = `${safeTitle}_${bvid}_${quality.quality_name.replace(/\s+/g, '_')}.${quality.format || 'mp4'}`;
    
    if (target === 'browser') {
        downloadToBrowser(quality.url, filename);
    } else {
        downloadToServer(bvid, filename, 'video', quality.quality, true, quality.url);
    }
}

export function downloadToBrowser(url, filename) {
    // 使用下载代理 - 每次新建 iframe 触发下载，避免复用单 iframe 时浏览器忽略连续请求
    // 安全：Cookie 由后端从 DB 读取，前端不再拼接到 URL。
    const proxyUrl = `/api/download/proxy?url=${encodeURIComponent(url)}&filename=${encodeURIComponent(filename)}`;

    const iframe = document.createElement('iframe');
    iframe.hidden = true;
    iframe.src = proxyUrl;
    document.body.appendChild(iframe);

    // 60 秒后清理 iframe，避免 DOM 堆积
    setTimeout(() => { iframe.remove(); }, 60000);

    // 显示下载中消息
    showToast(`正在下载: ${filename}，请查看浏览器下载栏`, 'success');
}

// 下载弹幕
export async function downloadDanmaku(bvid, source = '', historyId = undefined, page = undefined) {
    if (!bvid) return;
    const btn = document.querySelector(`[data-role="manual-danmaku-download"][data-bvid="${CSS.escape(bvid)}"]`);
    
    if (btn) {
        btn.disabled = true;
        btn.innerHTML = '<span class="loading"></span> 下载中';
    }
    
    try {
        const result = await apiPost('/api/video/download-danmaku', {
            bvid: bvid,
            source: source || undefined,
            history_id: historyId ?? undefined,
            page: page ?? undefined,
        });
        
        if (result.code === 0) {
            const data = result.data || {};
            if (data.count > 0) {
                showToast(`弹幕下载完成: ${data.count} 条`, 'success');
                if (btn) {
                    btn.innerHTML = '<i class="fa-solid fa-check"></i> 弹幕已下载';
                    setTone(btn, 'success');
                    
                }
            } else {
                showToast('该视频暂无弹幕', 'info');
                if (btn) {
                    btn.innerHTML = '<i class="fa-solid fa-comment-slash"></i> 暂无弹幕';
                }
            }
        } else {
            showToast(result.message || '弹幕下载失败', 'error');
            if (btn) {
                btn.disabled = false;
                btn.innerHTML = '<i class="fa-solid fa-redo"></i> 重试弹幕';
            }
        }
    } catch (e) {
        showToast('弹幕下载失败', 'error');
        if (btn) {
            btn.disabled = false;
            btn.innerHTML = '<i class="fa-solid fa-comment-dots"></i> 下载弹幕';
        }
    }
}

// 下载评论
export async function downloadComments(bvid, source = '', historyId = undefined) {
    if (!bvid) return;
    const btn = document.querySelector(`[data-role="manual-comments-download"][data-bvid="${CSS.escape(bvid)}"]`);
    
    if (btn) {
        btn.disabled = true;
        btn.innerHTML = '<span class="loading"></span> 下载中';
    }
    
    try {
        const result = await apiPost('/api/video/download-comments', {
            bvid: bvid,
            source: source || undefined,
            history_id: historyId ?? undefined,
        });
        
        if (result.code === 0) {
            const data = result.data || {};
            if (data.count > 0) {
                showToast(`评论下载完成: ${data.count} 条主评论`, 'success');
                if (btn) {
                    btn.innerHTML = '<i class="fa-solid fa-check"></i> 评论已下载';
                    setTone(btn, 'success');
                    
                }
            } else {
                showToast('该视频暂无评论', 'info');
                if (btn) {
                    btn.innerHTML = '<i class="fa-solid fa-comment-slash"></i> 暂无评论';
                }
            }
        } else {
            showToast(result.message || '评论下载失败', 'error');
            if (btn) {
                btn.disabled = false;
                btn.innerHTML = '<i class="fa-solid fa-redo"></i> 重试评论';
            }
        }
    } catch (e) {
        showToast('评论下载失败', 'error');
        if (btn) {
            btn.disabled = false;
            btn.innerHTML = '<i class="fa-solid fa-comments"></i> 下载评论';
        }
    }
}

// 触发手动下载按钮的状态更新（主要用于错误显示）
export function updateManualDownloadProgress(bvid, type) {
    const stateKey = `${bvid}_${type}`;
    const data = _state.manualDownloadProgress[stateKey];
    if (data) {
        updateDownloadProgressInList(bvid, data);
    }
}

// 添加下载任务到服务器
export async function downloadToServer(bvid, title, type = 'video', quality = null, isManual = false, directUrl = null) {
    // 安全：Cookie 由后端从 DB 读取，前端不再传递。
    // 如果没有传入清晰度，从设置读取
    if (quality === null) {
        quality = parseInt(document.getElementById('setting-video-quality')?.value) || 80;
    }
    
    // 如果是手动触发，初始化进度跟踪
    if (isManual) {
        const stateKey = `${bvid}_${type}`;
        _state.manualDownloadProgress[stateKey] = {
            bvid: bvid,
            type: type,
            status: 'pending',
            progress_percent: 0,
            downloaded_size: 0,
            total_size: 0,
            speed: 0,
            filename: title
        };
    }
    
    try {
        const payload = {
            bvid: bvid,
            title: title.replace(/\.[^.]+$/, ''), // 移除扩展名作为标题
            quality: quality,
            type: type
        };
        
        // 如果提供了直接 URL，则使用它
        if (directUrl) {
            payload.url = directUrl;
        }
        
        const result = await apiPost('/api/download/add', payload);
        
        if (result.code === 0) {
            const msg = isManual ? `开始下载: ${title}` : `已添加到下载队列: ${title}`;
            showToast(msg, 'success');
        } else {
            showToast(result.message || '添加失败', 'error');
            const stateKey = `${bvid}_${type}`;
            if (isManual && _state.manualDownloadProgress[stateKey]) {
                _state.manualDownloadProgress[stateKey].status = 'failed';
                _state.manualDownloadProgress[stateKey].error = result.message || '添加失败';
                updateManualDownloadProgress(bvid, type);
            }
        }
    } catch (e) {
        showToast('添加下载任务失败', 'error');
        const stateKey = `${bvid}_${type}`;
        if (isManual && _state.manualDownloadProgress[stateKey]) {
            _state.manualDownloadProgress[stateKey].status = 'failed';
            _state.manualDownloadProgress[stateKey].error = e.message;
            updateManualDownloadProgress(bvid, type);
        }
    }
}
