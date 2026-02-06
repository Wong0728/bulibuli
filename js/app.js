// B站视频监控助手 - 前端版本
// 连接后端API

// ==================== API基础配置 ====================
const API_BASE = '';

// ==================== WebSocket配置 ====================
let socket = null;
let isWebSocketConnected = false;

// 初始化WebSocket连接
function initWebSocket() {
    try {
        socket = io();

        socket.on('connect', function() {
            console.log('[WebSocket] 已连接到服务器');
            isWebSocketConnected = true;
            showMsg('实时连接已建立', 'success');

            // 订阅下载进度更新
            socket.emit('subscribe_download_progress');
        });

        socket.on('disconnect', function() {
            console.log('[WebSocket] 与服务器断开连接');
            isWebSocketConnected = false;
        });

        socket.on('log_update', function(data) {
            console.log('[WebSocket] 收到日志:', data);

            // 收到日志更新
            if (data.uid && bloggerStates) {
                // 找到对应的博主ID
                const blogger = bloggers.find(b => b.uid === data.uid);
                if (blogger && bloggerStates[blogger.id]) {
                    if (!bloggerStates[blogger.id].logs) {
                        bloggerStates[blogger.id].logs = [];
                    }
                    bloggerStates[blogger.id].logs.push({
                        time: data.time,
                        level: data.level,
                        msg: data.message
                    });

                    // 限制日志数量
                    if (bloggerStates[blogger.id].logs.length > 100) {
                        bloggerStates[blogger.id].logs.shift();
                    }

                    // 如果当前正在查看该博主，更新显示
                    if (selectedBloggerId === blogger.id) {
                        renderBloggerLogs(blogger.id);
                    }
                }
            }

            // 同时添加到全局日志
            addGlobalLog(data.message, data.level);
        });

        socket.on('download_progress', function(data) {
            // 收到下载进度更新
            const bvid = data.bvid;

            // 统一更新所有相关的 UI 组件（下载列表、手动下载按钮等）
            updateDownloadProgressInList(bvid, data);
        });

        socket.on('connected', function(data) {
            console.log('[WebSocket]', data.message);
        });

        socket.on('subscribed', function(data) {
            console.log('[WebSocket] 订阅成功:', data);
        });

        // 监听来自后端的通用通知
        socket.on('notification', function(data) {
            console.log('[WebSocket] 收到通知:', data);
            showMsg(data.message, data.type || 'info');
        });

    } catch (e) {
        console.error('[WebSocket] 初始化失败:', e);
    }
}

// 添加全局日志
function addGlobalLog(message, level = 'info') {
    const logContainer = document.getElementById('logContainer');
    if (!logContainer) return;

    const now = new Date();
    const time = now.toTimeString().split(' ')[0];

    const logEntry = document.createElement('div');
    logEntry.className = `log-entry log-level-${level}`;
    logEntry.innerHTML = `<span class="log-time">${time}</span><span>${message}</span>`;

    logContainer.appendChild(logEntry);
    logContainer.scrollTop = logContainer.scrollHeight;

    // 限制日志数量
    while (logContainer.children.length > 100) {
        logContainer.removeChild(logContainer.firstChild);
    }
}

// 用于取消请求的AbortController
let currentControllers = [];

function createAbortController() {
    const controller = new AbortController();
    currentControllers.push(controller);
    return controller;
}

function cleanupControllers() {
    currentControllers.forEach(ctrl => {
        try {
            ctrl.abort();
        } catch (e) {}
    });
    currentControllers = [];
}

async function apiRequest(url, options = {}) {
    const controller = createAbortController();
    try {
        const response = await fetch(url, {
            headers: {
                'Content-Type': 'application/json',
            },
            signal: controller.signal,
            ...options
        });
        
        // 从列表中移除已完成的控制器
        const index = currentControllers.indexOf(controller);
        if (index > -1) {
            currentControllers.splice(index, 1);
        }
        
        if (!response.ok) {
            throw new Error(`HTTP error! status: ${response.status}`);
        }
        return response.json();
    } catch (error) {
        // 从列表中移除控制器
        const index = currentControllers.indexOf(controller);
        if (index > -1) {
            currentControllers.splice(index, 1);
        }
        
        // 忽略取消请求的错误
        if (error.name === 'AbortError') {
            console.log('Request was aborted:', url);
            return { success: false, message: '请求被取消' };
        }
        throw error;
    }
}

async function apiPost(url, data) {
    return apiRequest(url, {
        method: 'POST',
        body: JSON.stringify(data)
    });
}

async function apiGet(url) {
    return apiRequest(url, {
        method: 'GET'
    });
}

// ==================== 全局状态 ====================
let manualDownloadProgress = {};  // 存储手动下载的进度 {bvid: {progress, status, speed, etc}}
let serverOffset = 0;
let nextCheckTimestamp = 0;
let bloggers = [];
let bloggerIdCounter = 0;
let isTaskRunning = false;
let progressUpdateInterval = null;
let urlExpiryTimers = {};
let videoTitles = {};
let cookieWarningShown = false;

// 当前选中的博主ID
let selectedBloggerId = null;
let selectedDownloadBloggerId = null;

// 每个博主的独立状态
let bloggerStates = {};

// ==================== 初始化 ====================
window.addEventListener('DOMContentLoaded', async () => {
    initTabSwitching();
    await loadConfig();
    
    // 显示状态恢复提示
    showMsg('正在恢复任务状态...', 'info');
    
    // 从服务器加载所有状态
    await loadBloggersFromServer();
    await loadDownloadStatus();
    await loadSettingsFromServer();
    
    // 初始化WebSocket连接
    initWebSocket();
    
    // 启动状态轮询
    startStatusPolling();
    startProgressUpdates();
    
    // 初始化移动端侧边栏
    initMobileSidebar();
    
    showMsg('任务状态已恢复', 'success');
});

function initTabSwitching() {
    document.querySelectorAll('.nav-tab').forEach(tab => {
        tab.addEventListener('click', () => switchTab(tab.dataset.tab));
    });
}

function switchTab(tabName) {
    document.querySelectorAll('.tab-panel, .nav-tab').forEach(el => el.classList.remove('active'));
    document.getElementById(`tab-${tabName}`).classList.add('active');
    document.querySelector(`.nav-tab[data-tab="${tabName}"]`).classList.add('active');
    if (tabName === 'history') {
        updateDownloadLists();
    } else if (tabName === 'settings') {
        loadSettingsFromServer();
    } else if (tabName === 'auto') {
        renderBloggerSidebar();
    }
}

// ==================== 配置管理 ====================
async function loadConfig() {
    try {
        const saved = localStorage.getItem('appConfig');
        const data = saved ? JSON.parse(saved) : { uid: '', cookies: '', history_uids: [] };
        document.getElementById('manualUid').value = data.uid || '';
        if (data.cookies) {
            document.getElementById('manualCookies').value = data.cookies;
        }
        const datalist = document.getElementById('uidHistory');
        datalist.innerHTML = (data.history_uids || []).map(uid => `<option value="${uid}"></option>`).join('');
    } catch (e) {
        showMsg('配置加载失败', 'error');
    }
}

async function syncConfig(value, type) {
    const uid = document.getElementById('manualUid').value;
    const cookies = document.getElementById('manualCookies').value;
    const saved = localStorage.getItem('appConfig');
    const data = saved ? JSON.parse(saved) : {};
    data.uid = uid;
    data.cookies = cookies;
    localStorage.setItem('appConfig', JSON.stringify(data));
}

// ==================== Cookies 管理 ====================
let qrcodePollInterval = null;

async function showQRCodeLogin() {
    const modal = document.getElementById('qrcodeModal');
    if (modal) {
        modal.classList.add('active');
        refreshQRCode();
    }
}

async function refreshQRCode() {
    const canvasContainer = document.getElementById('qrcodeCanvas');
    const statusEl = document.getElementById('qrcodeStatus');
    const hintEl = document.getElementById('qrcodeHint');
    const refreshBtn = document.getElementById('refreshQRCodeBtn');
    
    if (!canvasContainer) return;

    canvasContainer.innerHTML = '<div class="loading"></div>';
    statusEl.style.display = 'none';
    hintEl.textContent = '正在获取二维码...';
    refreshBtn.style.display = 'none';
    
    if (qrcodePollInterval) {
        clearInterval(qrcodePollInterval);
        qrcodePollInterval = null;
    }
    
    try {
        // 检查 QRCode 库是否加载
        if (typeof QRCode === 'undefined') {
            throw new Error('QRCode 库未加载，请检查网络连接或刷新页面');
        }

        const result = await apiGet('/api/cookies/qrcode/generate');
        console.log('[QRCode] 生成结果:', result);

        if (result.success && result.data) {
            const { url, qrcode_key } = result.data;
            
            // 清空容器并创建新的 canvas 元素
            canvasContainer.innerHTML = '';
            const qrcanvas = document.createElement('canvas');
            
            // 生成二维码
            await QRCode.toCanvas(qrcanvas, url, {
                width: 200,
                margin: 2,
                color: {
                    dark: '#000000',
                    light: '#ffffff'
                }
            });
            
            canvasContainer.appendChild(qrcanvas);
            hintEl.textContent = '请使用 Bilibili 手机客户端扫码';
            
            // 开始轮询
            startPollingQRCode(qrcode_key);
        } else {
            canvasContainer.innerHTML = '<i class="fas fa-exclamation-circle fa-3x" style="color:var(--error-color);"></i>';
            hintEl.textContent = result.message || '获取二维码失败';
            refreshBtn.style.display = 'inline-flex';
        }
    } catch (e) {
        console.error('[QRCode] 获取二维码失败:', e);
        canvasContainer.innerHTML = '<i class="fas fa-exclamation-circle fa-3x" style="color:var(--error-color);"></i>';
        hintEl.textContent = e.message || '网络请求失败';
        refreshBtn.style.display = 'inline-flex';
    }
}

function startPollingQRCode(qrcodeKey) {
    if (qrcodePollInterval) clearInterval(qrcodePollInterval);
    
    qrcodePollInterval = setInterval(async () => {
        try {
            const result = await apiGet(`/api/cookies/qrcode/poll?qrcode_key=${qrcodeKey}`);
            if (result.success) {
                const { code, message, cookies } = result;
                const statusEl = document.getElementById('qrcodeStatus');
                const hintEl = document.getElementById('qrcodeHint');
                const refreshBtn = document.getElementById('refreshQRCodeBtn');
                
                if (code === 0) {
                    // 登录成功
                    clearInterval(qrcodePollInterval);
                    qrcodePollInterval = null;
                    
                    statusEl.textContent = '登录成功！';
                    statusEl.style.display = 'block';
                    statusEl.style.color = 'var(--success-color)';
                    
                    document.getElementById('manualCookies').value = cookies;
                    syncConfig(cookies, 'cookies');
                    
                    showMsg('扫码登录成功', 'success');
                    
                    // 自动保存到文件
                    await apiPost('/api/cookies/save', { cookies });
                    
                    setTimeout(() => {
                        closeQRCodeModal();
                    }, 1500);
                } else if (code === 86101) {
                    // 未扫码
                    statusEl.style.display = 'none';
                } else if (code === 86090) {
                    // 已扫码未确认
                    statusEl.textContent = '已扫码，请在手机上确认';
                    statusEl.style.display = 'block';
                    statusEl.style.color = 'var(--primary-color)';
                } else if (code === 86038) {
                    // 二维码失效
                    clearInterval(qrcodePollInterval);
                    qrcodePollInterval = null;
                    
                    statusEl.textContent = '二维码已失效';
                    statusEl.style.display = 'block';
                    statusEl.style.color = 'var(--error-color)';
                    refreshBtn.style.display = 'inline-flex';
                }
            }
        } catch (e) {
            console.error('轮询状态失败:', e);
        }
    }, 2000);
}

function closeQRCodeModal() {
    const modal = document.getElementById('qrcodeModal');
    if (modal) {
        modal.classList.remove('active');
    }
    if (qrcodePollInterval) {
        clearInterval(qrcodePollInterval);
        qrcodePollInterval = null;
    }
}

async function loadCookiesFromFile() {
    try {
        const result = await apiGet('/api/cookies/load');
        if (result.success && result.cookies) {
            document.getElementById('manualCookies').value = result.cookies;
            showMsg('已加载保存的Cookies', 'success');
        } else {
            showMsg('没有找到保存的Cookies', 'info');
        }
    } catch (e) {
        showMsg('加载Cookies失败', 'error');
    }
}

async function saveCookiesToFile() {
    const cookies = document.getElementById('manualCookies').value;
    if (!cookies) {
        showMsg('请先输入Cookies内容', 'error');
        return;
    }
    try {
        const result = await apiPost('/api/cookies/save', { cookies });
        if (result.success) {
            showMsg('Cookies已保存', 'success');
        } else {
            showMsg(result.message || '保存失败', 'error');
        }
    } catch (e) {
        showMsg('保存Cookies失败', 'error');
    }
}

function clearCookies() {
    if (confirm('确定要清空Cookies吗？')) {
        document.getElementById('manualCookies').value = '';
        syncConfig('', 'cookies');
        showMsg('Cookies已清空', 'info');
    }
}

// ==================== 手动查询 ====================
async function doManualQuery() {
    const uid = document.getElementById('manualUid').value;
    const cookies = document.getElementById('manualCookies').value;
    const btn = document.querySelector('#tab-manual .btn-primary');
    const resultDiv = document.getElementById('manualResult');
    if (!uid) return showMsg('请输入UID', 'error');

    btn.disabled = true;
    btn.innerHTML = '<span class="loading"></span> 查询中';
    resultDiv.innerHTML = `<div class="card" style="text-align:center; padding:40px;"><i class="fas fa-spinner fa-spin fa-2x mb-md"></i><div>正在请求B站...</div></div>`;

    try {
        // 从设置输入框读取手动查询限制
        const manualQueryLimit = parseInt(document.getElementById('setting_manualQueryLimit')?.value) || 10;
        const result = await apiPost('/api/video/get_videos', {
            uid: uid,
            cookies: cookies,
            limit: manualQueryLimit
        });

        if (result.success) {
            if (result.videos.length === 0) {
                resultDiv.innerHTML = `<div class="card" style="text-align:center; padding:40px;"><i class="fas fa-info-circle fa-2x mb-md"></i><div>该UP主暂无视频</div></div>`;
            } else {
                resultDiv.innerHTML = result.videos.map(v => {
                    const escapeHtml = (str) => {
                        if (!str) return '';
                        return str.replace(/&/g, '&amp;')
                                  .replace(/</g, '&lt;')
                                  .replace(/>/g, '&gt;')
                                  .replace(/"/g, '&quot;')
                                  .replace(/'/g, '&#039;');
                    };
                    const safeTitle = escapeHtml(v.title);
                    const safeBvid = escapeHtml(v.bvid);
                    return `<div class="video-card">
                        <div class="video-info">
                            <div class="video-title">${safeTitle}</div>
                            <div class="video-meta">
                                <span><i class="far fa-calendar"></i> 发布: ${new Date(v.created * 1000).toLocaleString('zh-CN')}</span>
                                <span><i class="fas fa-play-circle"></i> 播放: ${(v.play || 0).toLocaleString()}</span>
                            </div>
                            <div class="btn-row" id="actions-${safeBvid}">
                                <button class="btn" onclick="getDownloadLinks('${safeBvid}', '${safeTitle.replace(/'/g, "\\'")}'); this.disabled=true; this.innerHTML='<span class=loading></span> 获取中'">
                                    <i class="fas fa-link"></i> 获取下载链接
                                </button>
                            </div>
                        </div>
                    </div>`;
                }).join('');
            }
        } else {
            resultDiv.innerHTML = `<div class="card" style="text-align:center; padding:40px;"><i class="fas fa-exclamation-triangle fa-2x mb-md" style="color:var(--error-color);"></i><p style="color:var(--error-color);">${result.message}</p></div>`;
        }
    } catch (e) {
        resultDiv.innerHTML = `<div class="card" style="text-align:center; padding:40px;"><i class="fas fa-exclamation-triangle fa-2x mb-md" style="color:var(--error-color);"></i><p style="color:var(--error-color);">请求错误: ${e.message}</p></div>`;
    } finally {
        btn.disabled = false;
        btn.innerHTML = '<i class="fas fa-play-circle"></i> 立即查询最新视频';
        await syncConfig(uid, 'uid');
    }
}

// ==================== 下载链接管理（支持多清晰度）====================
function startUrlExpiryTimer(bvid, title, expirySeconds = 300) {
    videoTitles[bvid] = title;

    if (urlExpiryTimers[bvid]) {
        clearInterval(urlExpiryTimers[bvid].interval);
    }

    const expiryTime = Date.now() + expirySeconds * 1000;
    const timerId = `url-expiry-${bvid}`;

    urlExpiryTimers[bvid] = {
        expiryTime: expiryTime,
        interval: setInterval(() => {
            const remaining = Math.max(0, Math.floor((expiryTime - Date.now()) / 1000));
            const timerElement = document.getElementById(timerId);

            if (timerElement) {
                if (remaining > 0) {
                    const minutes = Math.floor(remaining / 60);
                    const seconds = remaining % 60;
                    timerElement.textContent = `${minutes}:${seconds.toString().padStart(2, '0')}`;

                    if (remaining < 60) {
                        timerElement.style.color = 'var(--error-color)';
                        timerElement.style.fontWeight = '700';
                    }
                } else {
                    timerElement.textContent = '已过期';
                    timerElement.style.color = 'var(--error-color)';
                    clearInterval(urlExpiryTimers[bvid].interval);

                    const warningElement = document.getElementById(`url-warning-${bvid}`);
                    const storedTitle = videoTitles[bvid] || '';
                    if (warningElement) {
                        warningElement.innerHTML = `
                            <span style="color:var(--error-color);"><i class="fas fa-exclamation-triangle"></i> 链接已过期</span>
                            <button class="btn" onclick="getDownloadLinks('${bvid}', '${storedTitle.replace(/'/g, "\\'")}')" style="padding:4px 12px; font-size:12px;">
                                <i class="fas fa-sync-alt"></i> 重新获取
                            </button>
                        `;
                    }
                }
            }
        }, 1000)
    };
}

async function getDownloadLinks(bvid, title) {
    const cookies = document.getElementById('manualCookies').value;
    const actionsDiv = document.getElementById(`actions-${bvid}`);

    actionsDiv.innerHTML = '<span class="loading"></span> 获取中...';

    try {
        // 获取视频下载链接（多清晰度）
        const videoResult = await apiPost('/api/video/get_video_urls', {
            bvid: bvid,
            cookies: cookies
        });

        // 获取音频下载链接
        const audioResult = await apiPost('/api/video/get_audio_url', {
            bvid: bvid,
            cookies: cookies
        });

        if (videoResult.success || audioResult.success) {
            const safeTitle = title.replace(/[^\w\u4e00-\u9fff\-_. ]/g, '');
            
            let html = '<div style="display:flex; gap:8px; flex-wrap:wrap; align-items:center; margin-bottom:8px;">';
            html += '<span style="font-size:13px; color:var(--text-muted); font-weight:500;">视频下载:</span>';
            
            // 视频下载按钮（多清晰度）
            if (videoResult.success && videoResult.qualities && videoResult.qualities.length > 0) {
                html += '<select id="quality-select-' + bvid + '" style="padding:6px 12px; border-radius:6px; border:1px solid var(--border-light);">';
                videoResult.qualities.forEach((q, index) => {
                    html += `<option value="${index}">${q.quality_name} (${q.width}x${q.height})</option>`;
                });
                html += '</select>';
                
                html += `<button class="btn btn-primary" onclick="downloadVideoWithQuality('${bvid}', '${safeTitle.replace(/'/g, "\\'")}', 'browser')">
                    <i class="fas fa-desktop"></i> 浏览器下载
                </button>`;
                html += `<button class="btn btn-primary" id="manual-download-btn-${bvid}_video" style="background:var(--gradient-accent);" onclick="downloadVideoWithQuality('${bvid}', '${safeTitle.replace(/'/g, "\\'")}', 'server')">
                    <i class="fas fa-server"></i> 发送到下载器
                </button>`;
            }
            
            html += '</div>';
            
            // 音频下载按钮
            if (audioResult.success) {
                html += '<div style="display:flex; gap:8px; flex-wrap:wrap; align-items:center; margin-bottom:8px;">';
                html += '<span style="font-size:13px; color:var(--text-muted); font-weight:500;">音频下载:</span>';
                const audioFilename = `${safeTitle}_${bvid}.${audioResult.ext || 'm4s'}`;
                html += `<button class="btn" onclick="downloadToBrowser('${audioResult.audio_url}', '${audioFilename}')">
                    <i class="fas fa-music"></i> 音频(浏览器)
                </button>`;
                html += `<button class="btn" id="manual-download-btn-${bvid}_audio" style="background:var(--gradient-accent); color:white;" onclick="downloadToServer('${bvid}', '${audioFilename}', 'audio', null, true, '${audioResult.audio_url}')">
                    <i class="fas fa-music"></i> 音频(发送到下载器)
                </button>`;
                html += '</div>';
            }
            
            // 查看原视频
            html += `<div style="display:flex; gap:8px; flex-wrap:wrap; align-items:center;">`;
            html += `<a href="https://www.bilibili.com/video/${bvid}" target="_blank" class="btn">
                <i class="fas fa-external-link-alt"></i> 查看原视频
            </a>`;
            html += `</div>`;
            
            // 下载提示信息
            html += `<div id="manual-progress-container-${bvid}" style="margin-top:12px; padding:12px; background:var(--bg-color); border-radius:var(--radius-md); border:1px solid var(--border-light); display:none;">
                <div style="display:flex; align-items:center; gap:8px; color:var(--text-secondary);">
                    <i class="fas fa-info-circle" style="color:var(--primary-color);"></i>
                    <span style="font-size:13px;">下载已在 Aria2 中进行，请到软件内查看相关设置</span>
                </div>
            </div>`;
            
            // 有效期提示
            html += `<div id="url-warning-${bvid}" style="margin-top:8px; font-size:12px; display:flex; align-items:center; gap:8px;">
                <span style="color:var(--text-muted);"><i class="fas fa-clock"></i> 链接有效期:</span>
                <span id="url-expiry-${bvid}" style="font-family:'JetBrains Mono', monospace; font-weight:600; color:var(--success-color);">5:00</span>
                <span style="color:var(--text-muted);">(建议尽快下载)</span>
            </div>`;
            
            actionsDiv.innerHTML = html;
            
            // 存储视频链接数据供下载使用
            if (videoResult.success) {
                videoTitles[bvid + '_qualities'] = videoResult.qualities;
            }
            
            startUrlExpiryTimer(bvid, title, 300);
        } else {
            const errorMsg = videoResult.message || audioResult.message || '获取链接失败';
            actionsDiv.innerHTML = `<p style="color:var(--error-color);"><i class="fas fa-exclamation-circle"></i> ${errorMsg}</p>`;
        }
    } catch (e) {
        actionsDiv.innerHTML = `<p style="color:var(--error-color);"><i class="fas fa-exclamation-circle"></i> 获取链接失败: ${e.message}</p>`;
    }
}

function downloadVideoWithQuality(bvid, title, target) {
    const select = document.getElementById(`quality-select-${bvid}`);
    const qualityIndex = select ? select.value : 0;
    const qualities = videoTitles[bvid + '_qualities'];
    
    if (!qualities || !qualities[qualityIndex]) {
        showMsg('获取视频链接失败', 'error');
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

function downloadToBrowser(url, filename) {
    // 使用下载代理 - 使用 iframe 方式下载大文件，避免内存问题
    const cookies = document.getElementById('manualCookies').value;
    const proxyUrl = `/api/download/proxy?url=${encodeURIComponent(url)}&filename=${encodeURIComponent(filename)}&cookies=${encodeURIComponent(cookies)}`;

    showMsg(`开始下载: ${filename}`, 'info');

    // 创建一个隐藏的 iframe 来下载文件
    // 这种方式不会将文件加载到内存中
    let iframe = document.getElementById('downloadIframe');
    if (!iframe) {
        iframe = document.createElement('iframe');
        iframe.id = 'downloadIframe';
        iframe.style.display = 'none';
        document.body.appendChild(iframe);
    }

    // 设置 iframe 的 src 来触发下载
    iframe.src = proxyUrl;

    // 显示下载中消息
    showMsg(`正在下载: ${filename}，请查看浏览器下载栏`, 'success');
}

// 触发手动下载按钮的状态更新（主要用于错误显示）
function updateManualDownloadProgress(bvid, type) {
    const stateKey = `${bvid}_${type}`;
    const data = manualDownloadProgress[stateKey];
    if (data) {
        updateDownloadProgressInList(bvid, data);
    }
}

// 添加下载任务到服务器
async function downloadToServer(bvid, title, type = 'video', quality = null, isManual = false, directUrl = null) {
    const cookies = document.getElementById('manualCookies').value;
    
    // 如果没有传入清晰度，从设置读取
    if (quality === null) {
        quality = parseInt(document.getElementById('setting_videoQuality')?.value) || 80;
    }
    
    // 如果是手动触发，初始化进度跟踪并显示提示
    if (isManual) {
        const progressContainer = document.getElementById(`manual-progress-container-${bvid}`);
        if (progressContainer) {
            progressContainer.style.display = 'block';
        }
        
        const stateKey = `${bvid}_${type}`;
        manualDownloadProgress[stateKey] = {
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
            cookies: cookies,
            quality: quality,
            type: type
        };
        
        // 如果提供了直接 URL，则使用它
        if (directUrl) {
            payload.url = directUrl;
        }
        
        const result = await apiPost('/api/download/add', payload);
        
        if (result.success) {
            const msg = isManual ? `开始下载: ${title}` : `已添加到下载队列: ${title}`;
            showMsg(msg, 'success');
        } else {
            showMsg(result.message || '添加失败', 'error');
            const stateKey = `${bvid}_${type}`;
            if (isManual && manualDownloadProgress[stateKey]) {
                manualDownloadProgress[stateKey].status = 'failed';
                manualDownloadProgress[stateKey].error = result.message || '添加失败';
                updateManualDownloadProgress(bvid, type);
            }
        }
    } catch (e) {
        showMsg('添加下载任务失败', 'error');
        const stateKey = `${bvid}_${type}`;
        if (isManual && manualDownloadProgress[stateKey]) {
            manualDownloadProgress[stateKey].status = 'failed';
            manualDownloadProgress[stateKey].error = e.message;
            updateManualDownloadProgress(bvid, type);
        }
    }
}

// ==================== 模态框功能 ====================
function showAddBloggerModal() {
    const modal = document.getElementById('addBloggerModal');
    if (modal) {
        modal.classList.add('active');
        document.getElementById('modalBloggerUid').value = '';
        document.getElementById('modalBloggerName').value = '';
        document.getElementById('modalMinInterval').value = '60';
        document.getElementById('modalMaxInterval').value = '300';
        setTimeout(() => document.getElementById('modalBloggerUid').focus(), 100);
    }
}

function closeAddBloggerModal() {
    const modal = document.getElementById('addBloggerModal');
    if (modal) {
        modal.classList.remove('active');
    }
}

async function confirmAddBlogger() {
    const uid = document.getElementById('modalBloggerUid').value.trim();
    const name = document.getElementById('modalBloggerName').value.trim();
    const minInterval = parseInt(document.getElementById('modalMinInterval').value) || 60;
    const maxInterval = parseInt(document.getElementById('modalMaxInterval').value) || 300;
    
    if (!uid) {
        showMsg('请输入博主UID', 'error');
        return;
    }
    
    try {
        const result = await apiPost('/api/blogger/add', {
            uid: uid,
            name: name,
            min_interval: minInterval,
            max_interval: maxInterval
        });
        
        if (result.success) {
            await loadBloggersFromServer();
            closeAddBloggerModal();
            showMsg(`博主 ${uid} 已添加到监控列表`, 'success');
        } else {
            showMsg(result.message || '添加失败', 'error');
        }
    } catch (e) {
        showMsg('添加博主失败', 'error');
    }
}

// ==================== 右键菜单与编辑功能 ====================
let contextMenuBloggerId = null;

function showContextMenu(event, bloggerId) {
    event.preventDefault();
    contextMenuBloggerId = bloggerId;
    
    const menu = document.getElementById('contextMenu');
    menu.style.display = 'block';
    menu.style.left = `${event.clientX}px`;
    menu.style.top = `${event.clientY}px`;
    
    // 确保菜单不超出视口
    const menuRect = menu.getBoundingClientRect();
    if (menuRect.right > window.innerWidth) {
        menu.style.left = `${window.innerWidth - menuRect.width - 10}px`;
    }
    if (menuRect.bottom > window.innerHeight) {
        menu.style.top = `${window.innerHeight - menuRect.height - 10}px`;
    }
}

function hideContextMenu() {
    const menu = document.getElementById('contextMenu');
    if (menu) menu.style.display = 'none';
}

function handleContextMenuEdit() {
    hideContextMenu();
    if (contextMenuBloggerId) {
        showEditBloggerModal(contextMenuBloggerId);
    }
}

async function handleContextMenuDelete() {
    hideContextMenu();
    if (!contextMenuBloggerId) return;
    
    const blogger = bloggers.find(b => b.id === contextMenuBloggerId);
    if (!blogger) return;
    
    if (confirm(`确定要删除博主 ${blogger.uid} 吗？\n这将停止监控并删除配置，但不会删除已下载的视频。`)) {
        try {
            const result = await apiPost('/api/blogger/delete', { id: contextMenuBloggerId });
            if (result.success) {
                showMsg('博主已删除', 'success');
                if (selectedBloggerId === contextMenuBloggerId) {
                    selectedBloggerId = null;
                    showBloggerEmptyState();
                }
                await loadBloggersFromServer();
            } else {
                showMsg(result.message || '删除失败', 'error');
            }
        } catch (e) {
            showMsg('删除请求失败', 'error');
        }
    }
}

// 点击其他地方关闭右键菜单
document.addEventListener('click', (event) => {
    if (!event.target.closest('.context-menu')) {
        hideContextMenu();
    }
});

// 编辑模态框
function showEditBloggerModal(bloggerId) {
    const state = bloggerStates[bloggerId];
    if (!state) return;
    
    const modal = document.getElementById('editBloggerModal');
    if (modal) {
        document.getElementById('editBloggerId').value = bloggerId;
        document.getElementById('editBloggerUid').value = state.uid;
        document.getElementById('editBloggerName').value = state.name || '';
        document.getElementById('editMinInterval').value = state.minInterval || 60;
        document.getElementById('editMaxInterval').value = state.maxInterval || 300;
        
        modal.classList.add('active');
        setTimeout(() => document.getElementById('editBloggerName').focus(), 100);
    }
}

function closeEditBloggerModal() {
    const modal = document.getElementById('editBloggerModal');
    if (modal) {
        modal.classList.remove('active');
    }
}

async function confirmEditBlogger() {
    const id = parseInt(document.getElementById('editBloggerId').value);
    const name = document.getElementById('editBloggerName').value.trim();
    const minInterval = parseInt(document.getElementById('editMinInterval').value) || 60;
    const maxInterval = parseInt(document.getElementById('editMaxInterval').value) || 300;
    
    if (!id) return;
    
    try {
        const result = await apiPost('/api/blogger/update', {
            id: id,
            name: name,
            min_interval: minInterval,
            max_interval: maxInterval
        });
        
        if (result.success) {
            showMsg('博主配置已更新', 'success');
            closeEditBloggerModal();
            await loadBloggersFromServer();
            
            // 如果正在显示详情，刷新详情
            if (selectedBloggerId === id) {
                updateDetailPanel();
            }
        } else {
            showMsg(result.message || '更新失败', 'error');
        }
    } catch (e) {
        showMsg('更新请求失败', 'error');
    }
}

window.onclick = function(event) {
    // 点击遮罩层关闭模态框
    if (event.target.classList.contains('modal-overlay')) {
        event.target.classList.remove('active');
    }
}

// ==================== 博主管理（从服务器加载）====================
async function loadBloggersFromServer() {
    try {
        const result = await apiGet('/api/blogger/list');
        if (result.success) {
            bloggers = result.bloggers.map((b, index) => ({
                id: b.id,
                element: null,
                uid: b.uid
            }));
            
            // 初始化博主状态
            bloggerStates = {};
            result.bloggers.forEach(b => {
                bloggerStates[b.id] = {
                    id: b.id,
                    uid: b.uid,
                    name: b.name,
                    isRunning: b.is_running,
                    nextCheckTime: b.next_check,
                    logs: [],
                    minInterval: b.min_interval,
                    maxInterval: b.max_interval
                };
            });
            
            renderBloggerSidebar();
        }
    } catch (e) {
        // 忽略取消请求的错误，只显示其他错误
        if (e.name !== 'AbortError') {
            console.error('加载博主列表失败:', e);
            // 不显示错误消息，避免页面加载时的干扰
        }
    }
}

function renderBloggerSidebar() {
    const sidebar = document.getElementById('bloggerSidebarList');
    if (!sidebar) return;

    if (bloggers.length === 0) {
        sidebar.innerHTML = `
            <div style="text-align: center; padding: 30px 20px; color: var(--text-muted);">
                <i class="fas fa-users-slash" style="font-size: 32px; margin-bottom: 10px; opacity: 0.5;"></i>
                <div style="font-size: 14px;">暂无监控博主</div>
                <div style="font-size: 12px; margin-top: 5px;">点击下方按钮添加</div>
            </div>
        `;
        return;
    }

    sidebar.innerHTML = bloggers.map(b => {
        const state = bloggerStates[b.id] || {};
        const isRunning = state.isRunning || false;
        const uid = b.uid || '未设置UID';
        const name = state.name || '';
        const displayName = name ? `${name} (${uid})` : `博主 ${uid}`;
        const shortUid = uid.length > 8 ? uid.slice(0, 8) + '...' : uid;
        const isActive = selectedBloggerId === b.id;

        // 计算下次检查时间
        let nextCheckText = '';
        let nextCheckClass = '';

        if (isRunning) {
            if (state.nextCheckTime && state.nextCheckTime > 0) {
                const now = Math.floor(Date.now() / 1000);
                const diff = state.nextCheckTime - now;

                if (diff > 0) {
                    // 正常倒计时
                    const hours = Math.floor(diff / 3600);
                    const minutes = Math.floor((diff % 3600) / 60);
                    const seconds = diff % 60;
                    nextCheckText = `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;

                    // 如果剩余时间少于60秒，显示警告样式
                    if (diff < 60) {
                        nextCheckClass = 'warning';
                    }
                } else if (diff > -30) {
                    // 刚刚过期（30秒内），显示检查中
                    nextCheckText = '检查中...';
                    nextCheckClass = 'checking';
                } else {
                    // 已经过期很久，可能出错了
                    nextCheckText = '等待中';
                    nextCheckClass = 'waiting';
                }
            } else {
                // 正在运行但没有下次检查时间，可能是首次启动
                nextCheckText = '初始化...';
                nextCheckClass = 'initializing';
            }
        }

        return `
            <div class="blogger-list-item ${isActive ? 'active' : ''}" 
                 onclick="selectBlogger(${b.id})" 
                 oncontextmenu="showContextMenu(event, ${b.id})"
                 data-blogger-id="${b.id}">
                <div class="blogger-avatar">${(name || uid).slice(0, 2).toUpperCase()}</div>
                <div class="blogger-info">
                    <div class="blogger-name" title="${name ? name + ' (' + uid + ')' : uid}">${name || '博主 ' + b.id}</div>
                    <div class="blogger-uid">${uid}</div>
                    ${nextCheckText ? `<div class="blogger-next-check ${nextCheckClass}"><i class="fas fa-clock"></i> ${nextCheckText}</div>` : ''}
                </div>
                <div class="blogger-status ${isRunning ? 'running' : 'stopped'}" title="${isRunning ? '运行中' : '已停止'}"></div>
            </div>
        `;
    }).join('');
}

async function selectBlogger(id) {
    selectedBloggerId = id;
    renderBloggerSidebar();
    showBloggerDetail();
    await loadBloggerLogs(id);  // 从服务器加载日志
    updateDetailPanel();

    // 订阅该博主的日志更新
    const blogger = bloggers.find(b => b.id === id);
    if (blogger && blogger.uid && socket) {
        socket.emit('subscribe_blogger_logs', { uid: blogger.uid });
    }
}

function showBloggerEmptyState() {
    document.getElementById('bloggerEmptyState').style.display = 'block';
    document.getElementById('bloggerDetailContent').style.display = 'none';
}

function showBloggerDetail() {
    document.getElementById('bloggerEmptyState').style.display = 'none';
    document.getElementById('bloggerDetailContent').style.display = 'block';
}

function updateDetailPanel() {
    if (selectedBloggerId === null) return;
    
    const state = bloggerStates[selectedBloggerId];
    if (!state) return;
    
    const blogger = bloggers.find(b => b.id === selectedBloggerId);
    const uid = blogger ? blogger.uid : '未设置';
    const name = state.name || '';
    
    document.getElementById('detailBloggerName').textContent = name ? `${name} (${uid})` : (uid ? `博主 UID: ${uid}` : '未设置UID');
    
    const startBtn = document.getElementById('detailStartBtn');
    const stopBtn = document.getElementById('detailStopBtn');
    
    if (state.isRunning) {
        startBtn.style.display = 'none';
        stopBtn.style.display = 'inline-flex';
    } else {
        startBtn.style.display = 'inline-flex';
        stopBtn.style.display = 'none';
    }
    
    // 更新运行状态显示
    const runningStatusEl = document.getElementById('detailRunningStatus');
    if (runningStatusEl) {
        if (state.isRunning) {
            runningStatusEl.textContent = '运行中';
            runningStatusEl.className = 'status-value running';
        } else {
            runningStatusEl.textContent = '已停止';
            runningStatusEl.className = 'status-value stopped';
        }
    }
    
    // 更新倒计时显示
    updateBloggerCountdown(selectedBloggerId);
    
    renderBloggerLogs(selectedBloggerId);
}

// 更新博主倒计时
function updateBloggerCountdown(bloggerId) {
    const state = bloggerStates[bloggerId];
    if (!state) return;

    const countdownEl = document.getElementById('detailCountdown');
    if (!countdownEl) return;

    if (state.isRunning) {
        if (state.nextCheckTime && state.nextCheckTime > 0) {
            const now = Math.floor(Date.now() / 1000);
            const diff = state.nextCheckTime - now;

            if (diff > 0) {
                // 正常倒计时
                const hours = Math.floor(diff / 3600);
                const minutes = Math.floor((diff % 3600) / 60);
                const seconds = diff % 60;
                countdownEl.textContent = `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
                countdownEl.className = 'status-value running';
            } else if (diff > -30) {
                // 刚刚过期（30秒内），显示检查中
                countdownEl.textContent = '检查中...';
                countdownEl.className = 'status-value checking';
            } else {
                // 已经过期很久，可能出错了
                countdownEl.textContent = '等待中';
                countdownEl.className = 'status-value waiting';
            }
        } else {
            // 正在运行但没有下次检查时间，可能是首次启动
            countdownEl.textContent = '初始化...';
            countdownEl.className = 'status-value initializing';
        }
    } else {
        countdownEl.textContent = '--:--:--';
        countdownEl.className = 'status-value stopped';
    }
}

async function loadBloggerLogs(bloggerId) {
    // 从服务器加载博主日志
    const blogger = bloggers.find(b => b.id === bloggerId);
    if (!blogger || !blogger.uid) return;

    try {
        const result = await apiGet(`/api/logs/blogger?uid=${encodeURIComponent(blogger.uid)}&limit=100`);
        if (result.success && result.logs) {
            if (!bloggerStates[bloggerId]) {
                bloggerStates[bloggerId] = {};
            }
            // 直接使用服务器返回的日志格式
            bloggerStates[bloggerId].logs = result.logs;
            console.log(`[loadBloggerLogs] 加载了 ${result.logs.length} 条日志`, result.logs);
        }
    } catch (e) {
        console.error('加载日志失败:', e);
    }
}

function renderBloggerLogs(bloggerId) {
    const logsContainer = document.getElementById('detailBloggerLogs');
    if (!logsContainer) return;

    const state = bloggerStates[bloggerId];
    if (!state || !state.logs || state.logs.length === 0) {
        logsContainer.innerHTML = '<div style="text-align: center; padding: 20px; color: var(--text-muted);"><i class="fas fa-info-circle"></i> 暂无日志</div>';
        return;
    }

    // 按时间排序（最新的在最后）
    // 使用 Date 对象比较，确保跨天排序正确
    const sortedLogs = [...state.logs].sort((a, b) => {
        const timeA = a.time || '';
        const timeB = b.time || '';

        // 尝试解析为日期对象（假设是今天的日志）
        const now = new Date();
        const dateStr = now.toDateString();

        try {
            const dateA = new Date(`${dateStr} ${timeA}`);
            const dateB = new Date(`${dateStr} ${timeB}`);

            // 如果解析成功，使用日期比较
            if (!isNaN(dateA.getTime()) && !isNaN(dateB.getTime())) {
                return dateA - dateB;
            }
        } catch (e) {
            // 解析失败，回退到字符串比较
        }

        // 回退到字符串比较
        return timeA.localeCompare(timeB);
    });

    logsContainer.innerHTML = sortedLogs.map(l => {
        const level = l.level || 'info';
        const time = l.time || '--:--:--';
        const msg = l.msg || l.message || '';
        return `<div class="log-entry log-level-${level}"><span class="log-time">${time}</span><span>${msg}</span></div>`;
    }).join('');
    logsContainer.scrollTop = logsContainer.scrollHeight;
}

// 日志自动刷新定时器
let logRefreshInterval = null;

function startLogRefresh() {
    // 每3秒刷新一次当前选中博主的日志
    if (logRefreshInterval) clearInterval(logRefreshInterval);
    logRefreshInterval = setInterval(async () => {
        if (selectedBloggerId !== null) {
            await loadBloggerLogs(selectedBloggerId);
            renderBloggerLogs(selectedBloggerId);
        }
    }, 3000);
}

function stopLogRefresh() {
    if (logRefreshInterval) {
        clearInterval(logRefreshInterval);
        logRefreshInterval = null;
    }
}

async function startSelectedBlogger() {
    if (selectedBloggerId === null) return;
    
    const state = bloggerStates[selectedBloggerId];
    const blogger = bloggers.find(b => b.id === selectedBloggerId);
    
    if (!blogger || !blogger.uid) {
        showMsg('请先设置博主UID', 'error');
        return;
    }
    
    const cookies = document.getElementById('manualCookies').value;
    if (!cookies) {
        showMsg('请先在系统设置中配置Cookies', 'error');
        switchTab('settings');
        return;
    }
    
    try {
        const result = await apiPost('/api/task/start', {
            uid: blogger.uid,
            cookies: cookies
        });
        
        if (result.success) {
            state.isRunning = true;
            state.nextCheckTime = result.next_check;
            showMsg(`博主 ${blogger.uid} 监控已启动`, 'success');
            updateDetailPanel();
            renderBloggerSidebar();
        } else {
            showMsg(result.message || '启动失败', 'error');
        }
    } catch (e) {
        showMsg('启动监控失败', 'error');
    }
}

async function stopSelectedBlogger() {
    if (selectedBloggerId === null) return;
    
    const state = bloggerStates[selectedBloggerId];
    const blogger = bloggers.find(b => b.id === selectedBloggerId);
    
    if (!blogger) return;
    
    try {
        const result = await apiPost('/api/task/stop', {
            uid: blogger.uid
        });
        
        if (result.success) {
            state.isRunning = false;
            state.nextCheckTime = 0;
            showMsg('监控已停止', 'info');
            updateDetailPanel();
            renderBloggerSidebar();
        } else {
            showMsg(result.message || '停止失败', 'error');
        }
    } catch (e) {
        showMsg('停止监控失败', 'error');
    }
}

async function saveBloggers() {
    try {
        const result = await apiPost('/api/blogger/save_config', {});
        if (result.success) {
            showMsg(`已保存 ${bloggers.length} 个博主配置`, 'success');
        } else {
            showMsg(result.message || '保存失败', 'error');
        }
    } catch (e) {
        showMsg('保存配置失败', 'error');
    }
}

async function startMultiTask() {
    if (bloggers.length === 0) return showMsg('请至少添加一个博主UID', 'error');

    const cookies = document.getElementById('manualCookies').value;
    if (!cookies) {
        showMsg('请先在系统设置中配置Cookies', 'error');
        switchTab('settings');
        return;
    }

    const btn = document.getElementById('btnStart');
    btn.disabled = true;
    btn.innerHTML = '<span class="loading"></span> 启动中';

    try {
        const result = await apiPost('/api/task/start_all', { cookies });
        
        if (result.success) {
            showMsg('全部任务已启动', 'success');
            isTaskRunning = true;
            
            bloggers.forEach(b => {
                if (bloggerStates[b.id]) {
                    bloggerStates[b.id].isRunning = true;
                }
            });
            
            renderBloggerSidebar();
            if (selectedBloggerId !== null) {
                updateDetailPanel();
            }
        } else {
            showMsg(result.message || '启动失败', 'error');
        }
    } catch (e) {
        showMsg('启动任务失败', 'error');
    } finally {
        btn.disabled = false;
        btn.innerHTML = '<i class="fas fa-play"></i> 启动全部';
    }
}

async function stopTask() {
    const btn = document.getElementById('btnStop');
    btn.disabled = true;
    btn.innerHTML = '<span class="loading"></span> 停止中';

    try {
        const result = await apiPost('/api/task/stop_all', {});
        
        if (result.success) {
            showMsg('正在停止所有任务...', 'info');
            isTaskRunning = false;
            
            bloggers.forEach(b => {
                if (bloggerStates[b.id]) {
                    bloggerStates[b.id].isRunning = false;
                    bloggerStates[b.id].nextCheckTime = 0;
                }
            });
            
            renderBloggerSidebar();
            if (selectedBloggerId !== null) {
                updateDetailPanel();
            }
        } else {
            showMsg(result.message || '停止失败', 'error');
        }
    } catch (e) {
        showMsg('停止任务失败', 'error');
    } finally {
        btn.disabled = false;
        btn.innerHTML = '<i class="fas fa-stop"></i> 停止全部';
    }
}

// ==================== 状态轮询 ====================
let countdownInterval = null;

function startStatusPolling() {
    // 启动倒计时更新
    startCountdownUpdates();

    // 定期从服务器获取最新状态
    setInterval(async () => {
        try {
            // 获取各博主的下次检查时间
            const nextCheckResult = await apiGet('/api/task/next_check');
            if (nextCheckResult.success && nextCheckResult.bloggers) {
                for (const uid in nextCheckResult.bloggers) {
                    const blogger = bloggers.find(b => b.uid === uid);
                    if (blogger && bloggerStates[blogger.id]) {
                        const info = nextCheckResult.bloggers[uid];
                        bloggerStates[blogger.id].isRunning = info.is_running;
                        bloggerStates[blogger.id].nextCheckTime = info.next_check;
                    }
                }
                // 更新显示
                renderBloggerSidebar();
                if (selectedBloggerId !== null) {
                    updateDetailPanel();
                }
            }
        } catch (e) {
            // 静默处理
            console.error('状态轮询出错:', e);
        }
    }, 3000); // 每3秒更新一次
}

// 启动倒计时更新（每秒更新一次）
function startCountdownUpdates() {
    if (countdownInterval) {
        clearInterval(countdownInterval);
    }
    
    countdownInterval = setInterval(() => {
        // 更新侧边栏中的倒计时
        renderBloggerSidebar();
        
        // 更新详情面板中的倒计时
        if (selectedBloggerId !== null) {
            updateBloggerCountdown(selectedBloggerId);
        }
    }, 1000);
}

// ==================== 下载管理页面功能 ====================
let currentDownloadTab = 'queue';

async function switchDownloadTab(tab) {
    currentDownloadTab = tab;
    
    // 更新导航标签状态
    document.querySelectorAll('.download-nav-tab').forEach(el => {
        el.classList.remove('active');
        if (el.dataset.tab === tab) {
            el.classList.add('active');
        }
    });
    
    // 更新面板显示
    document.querySelectorAll('.download-tab-panel').forEach(el => {
        el.classList.remove('active');
    });
    const panel = document.getElementById(`download-tab-${tab}`);
    if (panel) {
        panel.classList.add('active');
    }
    
    // 刷新数据
    if (tab === 'completed') {
        await loadHistoryList();
    }
    await updateDownloadLists();
}

async function updateDownloadLists() {
    try {
        const result = await apiGet('/api/download/status');
        if (result.success) {
            const stats = result.stats;
            const statuses = result.statuses;
            
            // 更新 Aria2 状态指示点
            const dot = document.getElementById('aria2-status-dot');
            if (dot) {
                if (result.aria2_connected) {
                    dot.classList.remove('disconnected');
                    dot.classList.add('connected');
                    dot.title = 'Aria2 RPC 服务已连接 (点击查看详情)';
                    dot.onclick = (e) => {
                        e.stopPropagation();
                        showMsg('Aria2 RPC 服务运行正常，已建立连接', 'success');
                    };
                } else {
                    dot.classList.remove('connected');
                    dot.classList.add('disconnected');
                    dot.title = 'Aria2 RPC 服务未连接 (点击查看详情)';
                    dot.onclick = (e) => {
                        e.stopPropagation();
                        showMsg('无法连接到 Aria2 RPC 服务，请检查设置或确保服务已启动', 'error');
                    };
                }
            }

            // 更新导航徽章
            const navQueueBadge = document.getElementById('navQueueBadge');
            const navCompletedBadge = document.getElementById('navCompletedBadge');
            const navFailedBadge = document.getElementById('navFailedBadge');
            
            if (navQueueBadge) navQueueBadge.textContent = (stats.pending || 0) + (stats.downloading || 0);
            if (navCompletedBadge) navCompletedBadge.textContent = stats.completed || 0;
            if (navFailedBadge) navFailedBadge.textContent = stats.failed || 0;
            
            // 更新统计栏
            const statPending = document.getElementById('statPending');
            const statDownloading = document.getElementById('statDownloading');
            
            if (statPending) statPending.textContent = stats.pending || 0;
            if (statDownloading) statDownloading.textContent = stats.downloading || 0;
            
            // 渲染各列表
            renderQueueList(statuses);
            renderCompletedList(statuses);
            renderFailedList(statuses);
        }
    } catch (e) {
        console.error('更新下载列表失败:', e);
    }
}

function renderQueueList(statuses) {
    const container = document.getElementById('queueList');
    if (!container) return;

    const queueStatuses = Object.values(statuses).filter(s => s.status === 'pending' || s.status === 'downloading');

    if (queueStatuses.length === 0) {
        container.innerHTML = `
            <div style="text-align:center; padding:40px; color:var(--text-muted);">
                <i class="fas fa-inbox" style="font-size:48px; margin-bottom:16px; opacity:0.5;"></i>
                <div>暂无下载任务</div>
            </div>
        `;
        return;
    }

    container.innerHTML = queueStatuses.map(s => {
        const isDownloading = s.status === 'downloading';
        const isCompleted = s.status === 'completed';
        const isFailed = s.status === 'failed';
        const progress = s.progress_percent || 0;
        const speed = s.speed ? formatSpeed(s.speed) : '';
        const downloaded = s.downloaded_size ? formatFileSize(s.downloaded_size) : '0 B';
        const total = s.total_size ? formatFileSize(s.total_size) : '未知';

        // 确定状态文本
        let statusText = '等待中';
        let statusIcon = '<i class="fas fa-clock" style="color:var(--text-muted);"></i>';
        if (isDownloading) {
            statusText = '下载中';
            statusIcon = '<i class="fas fa-spinner fa-spin" style="color:var(--primary-color);"></i>';
        } else if (isCompleted) {
            statusText = '已完成';
            statusIcon = '<i class="fas fa-check-circle" style="color:var(--success-color);"></i>';
        } else if (isFailed) {
            statusText = '失败';
            statusIcon = '<i class="fas fa-exclamation-circle" style="color:var(--error-color);"></i>';
        }

        const taskType = s.type || 'video';
        const uniqueId = `${s.bvid}_${taskType}`;
        return `
            <div class="download-item" data-bvid="${s.bvid}" data-type="${taskType}" data-id="${uniqueId}">
                <div class="download-item-info">
                    <div class="download-item-title" title="${s.title}">
                        <a href="https://www.bilibili.com/video/${s.bvid}" target="_blank" style="color:var(--text-main); text-decoration:none;">
                            ${s.title}
                        </a>
                    </div>
                    <div class="download-item-meta">
                        ${statusIcon} ${statusText} ${taskType === 'audio' ? '· 音频' : ''}
                        ${speed && isDownloading ? ` · ${speed}` : ''}
                    </div>
                </div>
                <div class="download-item-progress">
                    <div class="download-item-progress-bar">
                        <div class="download-item-progress-fill ${isCompleted ? 'completed' : (isFailed ? 'failed' : '')}" style="width: ${progress}%"></div>
                    </div>
                    <div class="download-item-progress-text">
                        ${isCompleted ? '100% · 已完成' : (isFailed ? '失败' : (isDownloading ? `${progress}% · ${downloaded} / ${total}` : '等待中'))}
                    </div>
                </div>
                <div class="download-item-actions">
                    ${isDownloading ? '' : `<button class="action-btn download" data-action="retry" data-bvid="${s.bvid}" data-type="${taskType}" title="开始下载"><i class="fas fa-play"></i></button>`}
                </div>
            </div>
        `;
    }).join('');

    // 绑定事件委托
    bindDownloadItemEvents(container);
}

function renderCompletedList(statuses) {
    const container = document.getElementById('completedList');
    if (!container) return;

    const completedStatuses = Object.values(statuses).filter(s => s.status === 'completed');

    if (completedStatuses.length === 0) {
        container.innerHTML = `
            <div style="text-align:center; padding:40px; color:var(--text-muted);">
                <i class="fas fa-check-circle" style="font-size:48px; margin-bottom:16px; opacity:0.5;"></i>
                <div>暂无已完成下载</div>
            </div>
        `;
        return;
    }

    container.innerHTML = completedStatuses.map(s => {
        const total = s.total_size ? formatFileSize(s.total_size) : '未知';
        const time = s.updated_at ? s.updated_at.split(' ')[0] : '-';
        const taskType = s.type || 'video';
        const uniqueId = `${s.bvid}_${taskType}`;

        return `
            <div class="download-item" data-bvid="${s.bvid}" data-type="${taskType}" data-id="${uniqueId}">
                <div class="download-item-info">
                    <div class="download-item-title" title="${s.title}">
                        <a href="https://www.bilibili.com/video/${s.bvid}" target="_blank" style="color:var(--text-main); text-decoration:none;">
                            ${s.title}
                        </a>
                    </div>
                    <div class="download-item-meta">
                        <i class="fas fa-check-circle" style="color:var(--success-color);"></i> 已完成 ${taskType === 'audio' ? '· 音频' : ''} · ${total} · ${time}
                    </div>
                </div>
                <div class="download-item-progress">
                    <div class="download-item-progress-bar">
                        <div class="download-item-progress-fill completed" style="width: 100%"></div>
                    </div>
                    <div class="download-item-progress-text" style="color:var(--success-color);">
                        100% · ${total}
                    </div>
                </div>
            </div>
        `;
    }).join('');

    // 绑定事件委托
    bindDownloadItemEvents(container);
}

function renderFailedList(statuses) {
    const container = document.getElementById('failedList');
    if (!container) return;

    const failedStatuses = Object.values(statuses).filter(s => s.status === 'failed');

    if (failedStatuses.length === 0) {
        container.innerHTML = `
            <div style="text-align:center; padding:40px; color:var(--text-muted);">
                <i class="fas fa-exclamation-circle" style="font-size:48px; margin-bottom:16px; opacity:0.5;"></i>
                <div>暂无失败任务</div>
            </div>
        `;
        return;
    }

    container.innerHTML = failedStatuses.map(s => {
        const taskType = s.type || 'video';
        const uniqueId = `${s.bvid}_${taskType}`;
        return `
        <div class="download-item" data-bvid="${s.bvid}" data-type="${taskType}" data-id="${uniqueId}">
            <div class="download-item-info">
                <div class="download-item-title" title="${s.title}">
                    <a href="https://www.bilibili.com/video/${s.bvid}" target="_blank" style="color:var(--text-main); text-decoration:none;">
                        ${s.title}
                    </a>
                </div>
                <div class="download-item-meta">
                    <i class="fas fa-exclamation-triangle" style="color:var(--error-color);"></i>
                    ${s.error || '下载失败'} ${taskType === 'audio' ? '· 音频' : ''}
                </div>
            </div>
            <div class="download-item-progress">
                <div class="download-item-progress-bar">
                    <div class="download-item-progress-fill failed" style="width: 100%"></div>
                </div>
                <div class="download-item-progress-text" style="color:var(--error-color);">
                    失败
                </div>
            </div>
            <div class="download-item-actions">
                <button class="action-btn retry" data-action="retry" data-bvid="${s.bvid}" data-type="${taskType}" title="重试">
                    <i class="fas fa-redo"></i>
                </button>
            </div>
        </div>
    `}).join('');

    // 绑定事件委托
    bindDownloadItemEvents(container);
}

// 绑定下载项事件（事件委托）
function bindDownloadItemEvents(container) {
    if (!container) return;
    
    // 移除旧的事件监听器（如果有）
    container.removeEventListener('click', handleDownloadItemClick);
    
    // 添加新的事件监听器
    container.addEventListener('click', handleDownloadItemClick);
}

// 处理下载项点击事件
function handleDownloadItemClick(e) {
    // 查找最近的按钮元素
    const button = e.target.closest('[data-action]');
    if (!button) return;

    const action = button.dataset.action;
    const bvid = button.dataset.bvid;
    const taskType = button.dataset.type || 'video';

    if (!action || !bvid) return;

    e.preventDefault();
    e.stopPropagation();

    switch (action) {
        case 'retry':
        case 'download':
            retryDownload(bvid, taskType);
            break;
        case 'remove':
            removeDownload(bvid, taskType);
            break;
    }
}

// 移除下载任务
async function removeDownload(bvid, taskType = 'video') {
    if (!confirm('确定要移除这个下载任务吗？')) return;

    try {
        const result = await apiPost('/api/download/remove', { bvid, type: taskType });
        if (result.success) {
            showMsg('已移除下载任务', 'success');
            updateDownloadLists();
        } else {
            showMsg(result.message || '移除失败', 'error');
        }
    } catch (e) {
        showMsg('移除下载任务失败', 'error');
    }
}

async function loadHistoryList() {
    const tbody = document.querySelector('#historyTable tbody');
    if (!tbody) return;
    
    try {
        const result = await apiGet('/api/history/list');
        if (result.success && result.history) {
            if (result.history.length === 0) {
                tbody.innerHTML = '<tr><td colspan="4" style="text-align:center; padding:20px; color:var(--text-muted);"><i class="fas fa-inbox" style="margin-right:8px;"></i>暂无下载记录</td></tr>';
                return;
            }
            
            tbody.innerHTML = result.history.map(h => `
                <tr>
                    <td>
                        <div style="font-size:13px; max-width:300px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;" title="${h.title}">
                            <a href="https://www.bilibili.com/video/${h.bvid}" target="_blank" style="color:var(--text-main); text-decoration:none;">
                                ${h.title || '未知标题'}
                            </a>
                        </div>
                    </td>
                    <td style="font-size:12px; color:var(--text-muted);">${h.uid || '-'}</td>
                    <td style="font-size:12px; color:var(--text-muted);">${h.pub_date || '-'}</td>
                    <td style="font-size:12px; color:var(--text-muted);">${h.download_time || '-'}</td>
                </tr>
            `).join('');
        }
    } catch (e) {
        console.error('加载历史记录失败:', e);
    }
}

async function retryDownload(bvid, taskType = 'video') {
    try {
        const result = await apiPost('/api/download/retry', { bvid, type: taskType });
        if (result.success) {
            showMsg('开始重新下载...', 'info');
            setTimeout(updateDownloadLists, 1000);
        } else {
            showMsg(result.message || '重试失败', 'error');
        }
    } catch (e) {
        showMsg('重试下载失败', 'error');
    }
}

// 实时更新下载管理页面中的进度条以及手动下载按钮
function updateDownloadProgressInList(bvid, data) {
    const taskType = data.type || 'video';
    const stateKey = `${bvid}_${taskType}`;

    // 1. 更新手动下载的状态跟踪
    if (manualDownloadProgress[stateKey]) {
        manualDownloadProgress[stateKey] = {
            ...manualDownloadProgress[stateKey],
            ...data
        };
        
        // 更新手动查询页面的按钮状态
        const downloadBtn = document.getElementById(`manual-download-btn-${bvid}_video`);
        const audioBtn = document.getElementById(`manual-download-btn-${bvid}_audio`);
        const status = data.status;
        
        const updateBtn = (btn) => {
            if (!btn) return;
            if (status === 'downloading') {
                btn.disabled = true;
                btn.innerHTML = '<span class="loading"></span> 下载中';
            } else if (status === 'completed' || status === 'merged') {
                btn.disabled = false;
                btn.innerHTML = '<i class="fas fa-check"></i> 已完成';
                btn.style.background = 'var(--success-color)';
                btn.style.color = 'white';
            } else if (status === 'failed') {
                btn.disabled = false;
                btn.innerHTML = '<i class="fas fa-redo"></i> 重试';
                btn.style.background = 'var(--error-color)';
                btn.style.color = 'white';
            }
        };
        
        if (taskType === 'audio') {
            updateBtn(audioBtn);
        } else {
            updateBtn(downloadBtn);
        }
    }

    // 2. 查找下载队列中对应的任务元素并更新
    const uniqueId = `${bvid}_${taskType}`;
    
    const downloadItem = document.querySelector(`.download-item[data-id="${uniqueId}"]`);
    if (downloadItem) {
        return updateDownloadItemProgress(downloadItem, data);
    }
    
    // 兼容旧数据匹配
    const items = document.querySelectorAll(`.download-item[data-bvid="${bvid}"]`);
    for (const item of items) {
        if ((item.dataset.type || 'video') === taskType) {
            return updateDownloadItemProgress(item, data);
        }
    }
}

// 更新单个下载项的进度
function updateDownloadItemProgress(downloadItem, data) {
    const progressFill = downloadItem.querySelector('.download-item-progress-fill');
    const progressText = downloadItem.querySelector('.download-item-progress-text');
    const metaInfo = downloadItem.querySelector('.download-item-meta');

    if (!progressFill || !progressText) return;

    const progress = data.progress_percent || 0;
    const status = data.status;
    const downloaded = data.downloaded_size ? formatFileSize(data.downloaded_size) : '0 B';
    const total = data.total_size ? formatFileSize(data.total_size) : '未知';
    const speed = data.speed ? formatSpeed(data.speed) : '';
    const taskType = data.type || 'video';

    // 更新进度条宽度
    progressFill.style.width = `${progress}%`;

    // 根据状态更新样式
    progressFill.classList.remove('completed', 'failed');
    if (status === 'completed') {
        progressFill.classList.add('completed');
    } else if (status === 'failed') {
        progressFill.classList.add('failed');
    }

    // 更新进度文本
    if (status === 'completed') {
        progressText.textContent = '100% · 已完成';
        progressText.style.color = 'var(--success-color)';
    } else if (status === 'failed') {
        progressText.textContent = '失败';
        progressText.style.color = 'var(--error-color)';
    } else if (status === 'downloading') {
        progressText.textContent = `${progress}% · ${downloaded} / ${total}`;
        progressText.style.color = '';
    } else {
        progressText.textContent = '等待中';
        progressText.style.color = '';
    }

    // 更新状态图标和速度信息
    if (metaInfo) {
        let statusText = '等待中';
        let statusIcon = '<i class="fas fa-clock" style="color:var(--text-muted);"></i>';

        if (status === 'downloading') {
            statusText = '下载中';
            statusIcon = '<i class="fas fa-spinner fa-spin" style="color:var(--primary-color);"></i>';
        } else if (status === 'completed') {
            statusText = '已完成';
            statusIcon = '<i class="fas fa-check-circle" style="color:var(--success-color);"></i>';
        } else if (status === 'failed') {
            statusText = '失败';
            statusIcon = '<i class="fas fa-exclamation-circle" style="color:var(--error-color);"></i>';
        }

        metaInfo.innerHTML = `${statusIcon} ${statusText}${speed && status === 'downloading' ? ` · ${speed}` : ''} ${taskType === 'audio' ? '· 音频' : ''}`;
    }
}

// ==================== 下载状态管理 ====================
async function loadDownloadStatus() {
    try {
        const result = await apiGet('/api/download/status');
        if (result.success) {
            const stats = result.stats;
            const statuses = result.statuses;
            
            // 更新 Aria2 状态指示点
            const dot = document.getElementById('aria2-status-dot');
            if (dot) {
                if (result.aria2_connected) {
                    dot.classList.remove('disconnected');
                    dot.classList.add('connected');
                    dot.title = 'Aria2 RPC 服务已连接 (点击查看详情)';
                    dot.onclick = (e) => {
                        e.stopPropagation();
                        showMsg('Aria2 RPC 服务运行正常，已建立连接', 'success');
                    };
                } else {
                    dot.classList.remove('connected');
                    dot.classList.add('disconnected');
                    dot.title = 'Aria2 RPC 服务未连接 (点击查看详情)';
                    dot.onclick = (e) => {
                        e.stopPropagation();
                        showMsg('无法连接到 Aria2 RPC 服务，请检查设置或确保服务已启动', 'error');
                    };
                }
            }

            // 更新统计数字
            const statPending = document.getElementById('statPending');
            const statDownloading = document.getElementById('statDownloading');
            const navQueueBadge = document.getElementById('navQueueBadge');
            const navCompletedBadge = document.getElementById('navCompletedBadge');
            const navFailedBadge = document.getElementById('navFailedBadge');
            
            if (statPending) statPending.textContent = stats.pending;
            if (statDownloading) statDownloading.textContent = stats.downloading;
            if (navQueueBadge) navQueueBadge.textContent = stats.pending + stats.downloading;
            if (navCompletedBadge) navCompletedBadge.textContent = stats.completed;
            if (navFailedBadge) navFailedBadge.textContent = stats.failed;
            
            // 恢复下载进度到内存
            for (const key in statuses) {
                const s = statuses[key];
                manualDownloadProgress[key] = {
                    bvid: s.bvid,
                    type: s.type,
                    status: s.status,
                    progress_percent: s.progress_percent,
                    downloaded_size: s.downloaded_size,
                    total_size: s.total_size,
                    speed: s.speed,
                    filename: s.title || s.filename
                };
                
                // 刷新手动下载按钮状态
                updateManualDownloadProgress(s.bvid, s.type);
            }
            
            // 如果当前在下载管理页面，更新显示
            if (document.getElementById('tab-history').classList.contains('active')) {
                renderQueueList(statuses);
                renderCompletedList(statuses);
                renderFailedList(statuses);
            }
        }
    } catch (e) {
        console.error('加载下载状态失败:', e);
    }
}

function startProgressUpdates() {
    if (progressUpdateInterval) clearInterval(progressUpdateInterval);
    progressUpdateInterval = setInterval(() => {
        updateDownloadLists();
    }, 2000);
}

// ==================== 工具函数 ====================
function formatFileSize(bytes) {
    if (bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
}

function formatSpeed(bytesPerSecond) {
    return formatFileSize(bytesPerSecond) + '/s';
}

// ==================== 消息提示 ====================
function showMsg(msg, type) {
    const box = document.getElementById('msgBox');
    const div = document.createElement('div');
    div.className = 'msg-toast';
    const iconClass = type === 'success' ? 'fa-check-circle' : (type === 'error' ? 'fa-exclamation-circle' : 'fa-info-circle');
    div.style.borderColor = `var(--${type}-color)`;
    div.innerHTML = `<i class="fas ${iconClass}" style="color:var(--${type}-color);"></i><span style="color:var(--text-secondary);">${msg}</span>`;
    box.appendChild(div);
    setTimeout(() => {
        div.style.animation = 'slideOutRight 0.3s ease forwards';
        setTimeout(() => div.remove(), 300);
    }, 2700);
}

// ==================== 设置面板 ====================
async function loadSettingsFromServer() {
    try {
        const result = await apiGet('/api/settings/get');
        if (result.success && result.settings) {
            const s = result.settings;
            
            if (s.query) {
                document.getElementById('setting_manualQueryLimit').value = s.query.manual_query_limit || 10;
                document.getElementById('setting_autoQueryLimit').value = s.query.auto_query_limit || 10;
                document.getElementById('setting_videoQuality').value = s.query.video_quality || 112;
                document.getElementById('setting_videoFormat').value = s.query.video_format || 4048;
            }
            
            if (s.parallel_download) {
                document.getElementById('setting_maxParallel').value = s.parallel_download.max_parallel || 3;
                document.getElementById('setting_waitSlotTimeout').value = s.parallel_download.wait_slot_timeout || 300;
            }
            
            if (s.aria2_rpc) {
                document.getElementById('setting_aria2UseRpc').value = s.aria2_rpc.use_rpc ? 'true' : 'false';
                document.getElementById('setting_aria2Host').value = s.aria2_rpc.host || 'localhost';
                document.getElementById('setting_aria2Port').value = s.aria2_rpc.port || 6800;
                document.getElementById('setting_aria2Secret').value = s.aria2_rpc.secret || '';
            }
            
            if (s.aria2c_basic) {
                document.getElementById('setting_maxConnPerServer').value = s.aria2c_basic.max_connection_per_server || 16;
                document.getElementById('setting_split').value = s.aria2c_basic.split || 16;
                document.getElementById('setting_minSplitSize').value = s.aria2c_basic.min_split_size || '10M';
                document.getElementById('setting_maxTries').value = s.aria2c_basic.max_tries || 5;
                document.getElementById('setting_retryWait').value = s.aria2c_basic.retry_wait || 5;
                document.getElementById('setting_maxConcurrentDownloads').value = s.aria2c_basic.max_concurrent_downloads || 3;
            }
            
            if (s.storage) {
                document.getElementById('setting_historyLimit').value = s.storage.history_limit || 1000;
                document.getElementById('setting_uidHistoryLimit').value = s.storage.uid_history_limit || 10;
                document.getElementById('setting_logLimit').value = s.storage.log_limit || 100;
            }

            if (s.download_path) {
                document.getElementById('setting_autoOrganize').checked = s.download_path.auto_organize !== false;
                updatePathPreview();
            }
        }
    } catch (e) {
        console.error('加载设置失败:', e);
    }
}

async function saveSettings() {
    try {
        const settings = {
            query: {
                manual_query_limit: parseInt(document.getElementById('setting_manualQueryLimit').value),
                auto_query_limit: parseInt(document.getElementById('setting_autoQueryLimit').value),
                video_quality: parseInt(document.getElementById('setting_videoQuality').value),
                video_format: parseInt(document.getElementById('setting_videoFormat').value),
            },
            parallel_download: {
                max_parallel: parseInt(document.getElementById('setting_maxParallel').value),
                wait_slot_timeout: parseInt(document.getElementById('setting_waitSlotTimeout').value),
            },
            aria2_rpc: {
                use_rpc: document.getElementById('setting_aria2UseRpc').value === 'true',
                host: document.getElementById('setting_aria2Host').value,
                port: parseInt(document.getElementById('setting_aria2Port').value),
                secret: document.getElementById('setting_aria2Secret').value,
            },
            aria2c_basic: {
                max_connection_per_server: parseInt(document.getElementById('setting_maxConnPerServer').value),
                split: parseInt(document.getElementById('setting_split').value),
                min_split_size: document.getElementById('setting_minSplitSize').value,
                max_tries: parseInt(document.getElementById('setting_maxTries').value),
                retry_wait: parseInt(document.getElementById('setting_retryWait').value),
                max_concurrent_downloads: parseInt(document.getElementById('setting_maxConcurrentDownloads').value),
            },
            storage: {
                history_limit: parseInt(document.getElementById('setting_historyLimit').value),
                uid_history_limit: parseInt(document.getElementById('setting_uidHistoryLimit').value),
                log_limit: parseInt(document.getElementById('setting_logLimit').value),
            },
            download_path: {
                base_path: '',
                auto_organize: document.getElementById('setting_autoOrganize').checked,
                path_template: '{blogger_uid}/{title}'
            },
        };

        const result = await apiPost('/api/settings/save', settings);
        if (result.success) {
            showMsg('设置已保存', 'success');
        } else {
            showMsg(result.message || '保存失败', 'error');
        }
    } catch (e) {
        showMsg('保存设置失败', 'error');
    }
}

async function resetSettings() {
    if (!confirm('确定要恢复默认设置吗？此操作不可恢复。')) return;

    try {
        const result = await apiPost('/api/settings/reset', {});
        if (result.success) {
            showMsg('已恢复默认设置', 'success');
            await loadSettingsFromServer();
        } else {
            showMsg(result.message || '重置失败', 'error');
        }
    } catch (e) {
        showMsg('重置设置失败', 'error');
    }
}

function updatePathPreview() {
    const autoOrganize = document.getElementById('setting_autoOrganize').checked;
    const previewText = document.getElementById('path_preview_text');
    
    if (autoOrganize) {
        previewText.innerText = './data/downloads/{博主UID}/{视频标题}.mp4';
    } else {
        previewText.innerText = './data/downloads/{视频标题}.mp4';
    }
}

function loadSettings() {
    loadSettingsFromServer();
}

// ==================== 移动端适配 ====================
function initMobileSidebar() {
    const dashboard = document.querySelector('.blogger-dashboard');
    if (!dashboard) return;
    
    // 检查是否已存在按钮
    if (document.querySelector('.sidebar-toggle-btn')) return;
    
    // 创建切换按钮
    const toggleBtn = document.createElement('div');
    toggleBtn.className = 'sidebar-toggle-btn';
    toggleBtn.innerHTML = '<div style="display:flex;align-items:center;gap:8px;"><i class="fas fa-bars"></i> <span>博主列表</span></div><i class="fas fa-chevron-right"></i>';
    
    // 插入到 dashboard 前面
    dashboard.parentNode.insertBefore(toggleBtn, dashboard);
    
    // 创建遮罩层
    const overlay = document.createElement('div');
    overlay.className = 'sidebar-overlay';
    document.body.appendChild(overlay);
    
    // 获取侧边栏
    const sidebar = document.querySelector('.blogger-sidebar');
    
    // 事件绑定
    toggleBtn.addEventListener('click', () => {
        if (sidebar) sidebar.classList.add('active');
        overlay.classList.add('active');
    });
    
    overlay.addEventListener('click', () => {
        if (sidebar) sidebar.classList.remove('active');
        overlay.classList.remove('active');
    });
    
    // 点击列表项后自动关闭（仅在移动端）
    const sidebarList = document.getElementById('bloggerSidebarList');
    if (sidebarList) {
        sidebarList.addEventListener('click', (e) => {
            if (window.innerWidth <= 768 && e.target.closest('.blogger-list-item')) {
                if (sidebar) sidebar.classList.remove('active');
                overlay.classList.remove('active');
            }
        });
    }
}

// ==================== 测试功能 ====================
async function testDownload() {
    const bvid = prompt('请输入要测试的BVID（例如：BV1xx411c7mD）:');
    if (!bvid) return;

    showMsg('正在测试下载功能...', 'info');
    try {
        const result = await apiPost('/api/video/get_video_urls', {
            bvid: bvid,
            cookies: document.getElementById('manualCookies').value
        });
        
        if (result.success) {
            showMsg(`获取到 ${result.qualities.length} 个清晰度选项`, 'success');
        } else {
            showMsg(result.message || '测试失败', 'error');
        }
    } catch (e) {
        showMsg('测试失败', 'error');
    }
}
