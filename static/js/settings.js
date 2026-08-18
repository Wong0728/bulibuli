import { _state } from './state.js';
import { escapeHtml } from './utils.js';
import { setTone, apiPost, apiPut, apiGet } from './core.js';
import { showToast, confirmDialog } from './download-status.js';

let settingsFragmentPromise = null;

export function loadSettingsFragment() {
    if (settingsFragmentPromise) return settingsFragmentPromise;
    settingsFragmentPromise = (async () => {
        const mount = document.getElementById('settings-fragment-mount');
        if (!mount || mount.dataset.loaded === 'true') return;
        // 设置页片段走内部片段路由（/settings.html 直链已 302 回主界面）。
        // 纯静态文件服务（如前端冒烟测试）没有内部片段路由，回退到 /settings.html。
        let response = await fetch('/_fragments/settings.html', { credentials: 'same-origin', cache: 'no-store' });
        if (!response.ok) {
            response = await fetch('/settings.html', { credentials: 'same-origin', cache: 'no-store' });
        }
        if (!response.ok) throw new Error(`设置导航加载失败 (${response.status})`);
        mount.innerHTML = await response.text();
        mount.dataset.loaded = 'true';
        const groups = {
            basic: ['account', 'appearance', 'query', 'parallel', 'smart'],
            downloads: ['danmaku', 'aria2', 'ffmpeg', 'burn', 'subtitle', 'path', 'storage', 'retain', 'verify'],
            advanced: ['board', 'monitor', 'refresh', 'live-recording', 'update', 'logs'],
            security: ['local-config'],
        };
        const sections = () => [...document.querySelectorAll('#tab-settings .section-collapsible')];
        const selectGroup = group => {
            const visible = new Set(groups[group] || groups.basic);
            sections().forEach(section => {
                section.hidden = !visible.has(section.dataset.section);
            });
            mount.querySelectorAll('[data-settings-group]').forEach(button => {
                button.setAttribute('aria-pressed', String(button.dataset.settingsGroup === group));
            });
        };
        mount.querySelectorAll('[data-settings-group]').forEach(button => {
            button.addEventListener('click', () => selectGroup(button.dataset.settingsGroup));
        });
        selectGroup('basic');
        // 全局日志对所有已认证角色开放（与 /api/logs/get 的 RBAC 一致），
        // 片段加载后即启动轮询，不依赖 owner-only 的 loadSettingsFromServer。
        startGlobalLogsPolling();
    })();
    return settingsFragmentPromise;
}

// --- 设置面板 ---

// 解析时间点字符串为数组
export function parseTimePoints(value) {
    if (!value) return [1, 5, 24];
    return value.split(',')
        .map(v => parseInt(v.trim()))
        .filter(v => !isNaN(v) && v > 0)
        .sort((a, b) => a - b);
}

// 智能下载分时时间点：标签式管理，避免手输逗号带来的解析/去重问题
_state.smartTimePoints = [1, 5, 24];

export function renderTimePointChips() {
    const container = document.getElementById('time-point-chips');
    if (!container) return;
    if (!_state.smartTimePoints.length) {
        container.innerHTML = `<span class="time-point-empty">未添加时间点</span>`;
        return;
    }
    container.innerHTML = _state.smartTimePoints.map(h =>
        `<span class="time-point-chip">${h}h<button type="button" class="time-point-chip-del" title="移除" data-action="remove-time-point" data-hours="${h}"><i class="fa-solid fa-times"></i></button></span>`
    ).join('');
}

export function addTimePoint() {
    const input = document.getElementById('time-point-input');
    if (!input) return;
    const raw = input.value.trim();
    if (raw === '') return;
    const h = parseInt(raw);
    if (isNaN(h) || h < 0 || h > 72) { showToast('时间点需在 0-72 小时之间', 'error'); return; }
    if (_state.smartTimePoints.includes(h)) { showToast('该时间点已存在', 'error'); input.value = ''; return; }
    _state.smartTimePoints.push(h);
    _state.smartTimePoints.sort((a, b) => a - b);
    input.value = '';
    renderTimePointChips();
}

export function removeTimePoint(h) {
    _state.smartTimePoints = _state.smartTimePoints.filter(x => x !== h);
    renderTimePointChips();
}

// 评论“全部回复”模式的风控提示
export function onCommentsReplyModeChange() {
    const sel = document.getElementById('setting-comments-reply-mode');
    const note = document.getElementById('comments-reply-mode-note');
    if (!sel || !note) return;
    if (sel.value === 'all') {
        note.innerHTML = '<i class="fa-solid fa-exclamation-triangle"></i> 展开每条评论的全部子评论，会显著增加请求量，触发风控的概率更高。追求稳定请用“仅热门回复”。';
        setTone(note, 'error');
    } else {
        note.textContent = '仅取接口自带的约 3 条热门回复，无额外请求，最不易触发风控。';
        setTone(note);
    }
}

export function onSidecarArchiveModeChange() {
    const mode = document.getElementById('setting-sidecar-archive-mode');
    const limitGroup = document.getElementById('sidecar-archive-limit-group');
    if (!mode || !limitGroup) return;
    limitGroup.hidden = mode.value !== 'keep_latest_n';
}

// 切换智能下载设置的显示/隐藏
export function toggleSmartDownloadSettings(enabled) {
    const minHoursGroup = document.getElementById('smart-download-settings');
    const timePointsGroup = document.getElementById('time-points-settings');
    if (minHoursGroup) {
        minHoursGroup.classList.toggle('control-disabled', !enabled);
    }
    if (timePointsGroup) {
        timePointsGroup.classList.toggle('control-disabled', !enabled);
    }
}

// MD5 校验模式切换：periodic 时显示校验间隔 + 单次批量
export function onVerifyModeChange() {
    const modeEl = document.getElementById('setting-verify-mode');
    if (!modeEl) return;
    const mode = modeEl.value;
    const periodicGroup = document.getElementById('verify-periodic-group');
    const batchGroup = document.getElementById('verify-batch-group');
    const show = mode === 'periodic';
    if (periodicGroup) periodicGroup.hidden = !show;
    if (batchGroup) batchGroup.hidden = !show;
}

// 下载模式切换处理
export function onDownloadModeChange() {
    const mode = document.getElementById('setting-download-mode').value;
    const rpcPanel = document.getElementById('rpc-settings-panel');
    const modeNote = document.getElementById('download-mode-note');

    if (rpcPanel) {
        rpcPanel.hidden = mode !== 'rpc';
    }

    if (modeNote) {
        switch(mode) {
            case 'embedded':
                modeNote.textContent = '自动启动内置 aria2c 并通过 RPC 控制，无需手动配置';
                break;
            case 'rpc':
                modeNote.textContent = '连接到外部 Aria2 RPC 服务，需要手动启动 aria2c 并启用 RPC';
                break;
        }
    }
}

// FFmpeg 模式切换处理
export function onFFmpegModeChange() {
    const mode = document.getElementById('setting-ffmpeg-mode').value;
    const customPathGroup = document.getElementById('ffmpeg-custom-path-group');
    const modeNote = document.getElementById('ffmpeg-mode-note');
    const detectedPathGroup = document.getElementById('ffmpeg-detected-path-group');

    if (customPathGroup) {
        customPathGroup.hidden = mode !== 'custom';
    }

    if (modeNote) {
        switch(mode) {
            case 'auto':
                modeNote.textContent = '自动检测：自定义路径 > 内置 ffmpeg > 环境变量 > 系统 PATH';
                break;
            case 'system':
                modeNote.textContent = '使用系统 PATH 环境变量中找到的 ffmpeg';
                break;
            case 'embedded':
                modeNote.textContent = '使用程序 resources 目录内置的 ffmpeg';
                break;
            case 'custom':
                modeNote.textContent = '使用手动指定的自定义路径';
                break;
        }
    }

    // 刷新检测到的路径
    refreshFFmpegDetectedPath();
}

// 刷新 FFmpeg 检测路径
export async function refreshFFmpegDetectedPath() {
    const detectedPathEl = document.getElementById('ffmpeg-detected-path');
    if (!detectedPathEl) return;

    const mode = document.getElementById('setting-ffmpeg-mode').value;
    
    try {
        const result = await apiGet('/api/settings/ffmpeg-path?mode=' + mode);
        if (result.code === 0) {
            const data = result.data || {};
            const path = data.path || '未检测到';
            const source = data.source || 'unknown';
            
            let sourceText = '';
            let icon = 'fa-file';
            switch(source) {
                case 'system':
                    sourceText = '（系统 PATH）';
                    icon = 'fa-desktop';
                    break;
                case 'embedded':
                    sourceText = '（内置版本）';
                    icon = 'fa-box';
                    break;
                case 'custom':
                    sourceText = '（自定义路径）';
                    icon = 'fa-user';
                    break;
                case 'unknown':
                    sourceText = '';
                    icon = 'fa-question-circle';
                    break;
                default:
                    sourceText = '';
            }

            const isAvailable = data.available;
            const statusIcon = isAvailable ? '<i class="fa-solid fa-check-circle status-success"></i>' : '<i class="fa-solid fa-exclamation-triangle status-error"></i>';
            
            detectedPathEl.innerHTML = `
                ${statusIcon} <i class="fa-solid ${icon}"></i> ${escapeHtml(path)} ${sourceText ? `<span class="status-source">${escapeHtml(sourceText)}</span>` : ''}
            `;
            // ponytail: 首次检测可能后端尚未探完，补一次延迟重试
            if (!data.path) setTimeout(refreshFFmpegDetectedPath, 2000);
        } else {
            detectedPathEl.innerHTML = '<i class="fa-solid fa-exclamation-circle status-error"></i> 检测失败';
        }
    } catch (e) {
        detectedPathEl.innerHTML = '<i class="fa-solid fa-exclamation-circle status-error"></i> 检测失败';
    }
}

// 浏览 FFmpeg 路径（调用后端文件选择对话框）
export async function browseFFmpegPath() {
    try {
        // 注：由于浏览器安全限制，无法直接访问文件系统
        // 这里使用 input type="file" 的方式
        const input = document.createElement('input');
        input.type = 'file';
        // 不设置 accept 过滤：Linux/Termux 的 ffmpeg 二进制没有扩展名，过滤会将其挡住
        input.addEventListener('change', function(e) {
            const file = e.target.files[0];
            if (file) {
                // 注意：浏览器只能获取文件名，无法获取完整路径
                // 用户需要手动输入完整路径
                showToast('请手动输入完整的文件路径', 'info');
            }
        });
        input.click();
    } catch (e) {
        showToast('无法打开文件选择器', 'error');
    }
}

// 测试 FFmpeg 是否可用
export async function testFFmpeg() {
    const resultEl = document.getElementById('ffmpeg-test-result');
    if (!resultEl) return;

    resultEl.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> 测试中...';
    
    try {
        const mode = document.getElementById('setting-ffmpeg-mode').value;
        const customPath = document.getElementById('setting-ffmpeg-path').value;
        
        const result = await apiPost('/api/settings/ffmpeg-test', {
            mode: mode,
            custom_path: customPath
        });
        
        const data = result.data || {};
        if (result.code === 0 && data.available) {
            let sourceText = '';
            switch(data.source) {
                case 'system': sourceText = ' [系统PATH]'; break;
                case 'embedded': sourceText = ' [内置]'; break;
                case 'custom': sourceText = ' [自定义]'; break;
            }
            resultEl.innerHTML = `<span class="status-success"><i class="fa-solid fa-check-circle"></i> FFmpeg 可用${sourceText} ${data.version ? '(' + escapeHtml(data.version) + ')' : ''}</span>`;
        } else {
            resultEl.innerHTML = `<span class="status-error"><i class="fa-solid fa-times-circle"></i> ${escapeHtml(result.message || 'FFmpeg 不可用')}</span>`;
        }
    } catch (e) {
        resultEl.innerHTML = `<span class="status-error"><i class="fa-solid fa-times-circle"></i> 测试失败</span>`;
    }
}

export async function loadSettingsFromServer() {
    try {
        const result = await apiGet('/api/settings');
        const data = result.data || {};
        if (result.code === 0 && data.current) {
            const s = data.current;
            _state.settingsSnapshot = structuredClone(s);

            if (s.query) {
                document.getElementById('setting-manual-query-limit').value = s.query.manual_query_limit;
                document.getElementById('setting-auto-query-limit').value = s.query.auto_query_limit;
                document.getElementById('setting-video-quality').value = s.query.video_quality;
                document.getElementById('setting-video-format').value = s.query.video_format;
                document.getElementById('setting-skip-charge-videos').checked = s.query.skip_charge_videos;
                document.getElementById('setting-min-video-quality').value = s.query.min_video_quality ?? 64;
                document.getElementById('setting-codec-preference').value = (s.query.prefer_codecs ?? ['av1', 'hevc', 'avc']).join(',');
                document.getElementById('setting-allow-quality-fallback').checked = s.query.allow_quality_fallback !== false;
            }

            if (s.parallel_download) {
                document.getElementById('setting-max-parallel').value = s.parallel_download.max_parallel;
                document.getElementById('setting-wait-slot-timeout').value = s.parallel_download.wait_slot_timeout;
            }

            // 加载下载模式设置
            if (s.download_mode) {
                let mode = s.download_mode.mode;
                // 兼容旧值 "local" -> "embedded"
                if (mode === 'local') mode = 'embedded';
                document.getElementById('setting-download-mode').value = mode;
                onDownloadModeChange(); // 更新UI显示
            }

            if (s.aria2_rpc) {
                document.getElementById('setting-aria2-host').value = s.aria2_rpc.host;
                document.getElementById('setting-aria2-port').value = s.aria2_rpc.port;
                document.getElementById('setting-aria2-secret').value = s.aria2_rpc.secret;
            }

            if (s.aria2c_basic) {
                document.getElementById('setting-max-conn-per-server').value = s.aria2c_basic.max_connection_per_server;
                document.getElementById('setting-split').value = s.aria2c_basic.split;
                document.getElementById('setting-min-split-size').value = s.aria2c_basic.min_split_size;
                document.getElementById('setting-max-tries').value = s.aria2c_basic.max_tries;
                document.getElementById('setting-retry-wait').value = s.aria2c_basic.retry_wait;
                document.getElementById('setting-max-concurrent-downloads').value = s.aria2c_basic.max_concurrent_downloads;
            }

            if (s.storage) {
                document.getElementById('setting-history-limit').value = s.storage.history_limit;
                document.getElementById('setting-log-limit').value = s.storage.log_limit;
                document.getElementById('setting-per-blogger-retain-default').value = s.storage.per_blogger_retain_default;
            }

            if (s.live) {
                document.getElementById('setting-live-max-concurrent').value = s.live.max_concurrent ?? 2;
                document.getElementById('setting-live-min-free-space').value = s.live.min_free_space_gib ?? 10;
                document.getElementById('setting-live-max-duration').value = s.live.max_duration_hours ?? 12;
                document.getElementById('setting-live-file-template').value = s.live.file_name_template ?? '{room_id}_{title}_{date}';
            }

            if (s.download_path) {
                document.getElementById('setting-auto-organize').checked = s.download_path.auto_organize;
                document.getElementById('setting-path-template').value = s.download_path.path_template ?? '{uid}/{title}';
                document.getElementById('setting-conflict-strategy').value = s.download_path.conflict_strategy ?? 'suffix';
                updatePathPreview();
            }

            // 加载弹幕评论设置
            if (s.danmaku_comment) {
                document.getElementById('setting-auto-download-danmaku').checked = s.danmaku_comment.auto_download_danmaku;
                document.getElementById('setting-auto-download-comments').checked = s.danmaku_comment.auto_download_comments;
                document.getElementById('setting-comments-main-limit').value = s.danmaku_comment.comments_main_limit;
                document.getElementById('setting-comments-reply-mode').value = s.danmaku_comment.comments_reply_mode;
                document.getElementById('setting-comments-filter-regex').value = s.danmaku_comment.comments_filter_regex;
                document.getElementById('setting-sidecar-archive-mode').value =
                    s.danmaku_comment.sidecar_archive_mode ?? 'overwrite';
                document.getElementById('setting-sidecar-archive-limit').value =
                    s.danmaku_comment.sidecar_archive_limit ?? 3;
                onCommentsReplyModeChange();
                onSidecarArchiveModeChange();

                // 智能下载设置
                const enableSmart = s.danmaku_comment.enable_smart_download;
                document.getElementById('setting-enable-smart-download').checked = enableSmart;
                document.getElementById('setting-min-publish-hours').value = s.danmaku_comment.min_publish_hours;

                // 下载时间点设置（标签式）
                const timePoints = s.danmaku_comment.download_time_points;
                _state.smartTimePoints = Array.isArray(timePoints)
                    ? timePoints.map(v => parseInt(v)).filter(v => !isNaN(v) && v >= 0 && v <= 72).sort((a, b) => a - b)
                    : parseTimePoints(String(timePoints));
                renderTimePointChips();

                // 根据智能下载开关显示/隐藏相关设置
                toggleSmartDownloadSettings(enableSmart);
            }

            // 加载 FFmpeg 设置
            if (s.ffmpeg) {
                const ffmpegMode = s.ffmpeg.mode;
                document.getElementById('setting-ffmpeg-mode').value = ffmpegMode;
                document.getElementById('setting-ffmpeg-path').value = s.ffmpeg.custom_path;
                onFFmpegModeChange(); // 更新UI显示状态

                // 加载完设置后刷新检测路径
                setTimeout(refreshFFmpegDetectedPath, 100);
            }

            // MD5 校验设置
            if (s.download && s.download.verify) {
                const v = s.download.verify;
                const mode = v.mode;
                document.getElementById('setting-verify-mode').value = mode;
                document.getElementById('setting-verify-periodic-days').value = v.periodic_days;
                document.getElementById('setting-verify-periodic-batch').value = v.periodic_batch;
                onVerifyModeChange();
            }

            // 看板设置
            if (s.board) {
                document.getElementById('setting-path-display-mode').value = s.board.path_display_mode
                    || (s.board.show_relative_path ? 'relative' : 'hidden');
                document.getElementById('setting-browser-download-enabled').checked =
                    s.board.browser_download_enabled !== false;
            }

            // 监控设置
            if (s.monitor) {
                document.getElementById('setting-detect-reupload').checked = s.monitor.detect_reupload;
                const mpEl = document.getElementById('setting-multi-page-mode');
                if (mpEl) mpEl.value = s.monitor.multi_page_mode ?? 'first';
            }

            // 刷新设置
            if (s.refresh) {
                document.getElementById('setting-l1-interval-minutes').value = s.refresh.l1_interval_minutes;
            }
            if (s.appearance) {
                document.getElementById('setting-theme').value = s.appearance.theme ?? 'system';
                applyTheme(s.appearance.theme ?? 'system');
            }
            // 弹幕烧录参数（未配置时使用默认值，行为与迭代前一致）
            if (s.burn) {
                document.getElementById('setting-burn-opacity').value = s.burn.opacity ?? 0.6;
                document.getElementById('setting-burn-scroll-time').value = s.burn.scroll_time ?? 8;
                document.getElementById('setting-burn-fix-time').value = s.burn.fix_time ?? 4;
                document.getElementById('setting-burn-font-size-scale').value = s.burn.font_size_scale ?? 1;
                document.getElementById('setting-burn-bottom-reserve').value = s.burn.bottom_reserve ?? 50;
                document.getElementById('setting-burn-font-family').value = s.burn.font_family ?? 'auto';
                document.getElementById('setting-burn-color-mode').value = s.burn.color_mode ?? 'source';
                document.getElementById('setting-burn-color').value = s.burn.color ?? 'FFFFFF';
            }
            // CC 字幕设置（未配置时使用默认值：enabled=true, accept_ai=false, languages=[]）
            if (s.subtitle) {
                document.getElementById('setting-subtitle-enabled').checked = s.subtitle.enabled !== false;
                document.getElementById('setting-subtitle-accept-ai').checked = s.subtitle.accept_ai === true;
                document.getElementById('setting-subtitle-languages').value = (s.subtitle.languages || []).join(',');
            }
            // 更新策略
            if (s.update) {
                document.getElementById('setting-update-policy').value = s.update.policy ?? 'manual';
            }

            // 主 Web 仅显示基础配置摘要，不再访问可写的 Setup API。
            loadFoundationSummary();
            loadAiSkillInfo();
            loadUpdateStatus();
        }
    } catch (e) {
        showToast(`加载设置失败：${e.message}`, 'error');
    }
}

export async function saveSettings(btn) {
    let _origHtml = null;
    if (btn) {
        _origHtml = btn.innerHTML;
        btn.disabled = true;
        btn.innerHTML = '<span class="loading"></span> 保存中...';
    }
    try {
        if (!_state.settingsSnapshot) {
            throw new Error('设置尚未加载完成');
        }
        const settings = structuredClone(_state.settingsSnapshot);
        settings.expected_revision = settings.revision;
        Object.assign(settings.query, {
            manual_query_limit: parseInt(document.getElementById('setting-manual-query-limit').value),
            auto_query_limit: parseInt(document.getElementById('setting-auto-query-limit').value),
            video_quality: parseInt(document.getElementById('setting-video-quality').value),
            video_format: parseInt(document.getElementById('setting-video-format').value),
            skip_charge_videos: document.getElementById('setting-skip-charge-videos').checked,
            min_video_quality: parseInt(document.getElementById('setting-min-video-quality').value),
            prefer_codecs: document.getElementById('setting-codec-preference').value.split(','),
            allow_quality_fallback: document.getElementById('setting-allow-quality-fallback').checked,
        });
        Object.assign(settings.appearance, { theme: document.getElementById('setting-theme').value });
        applyTheme(settings.appearance.theme);
        Object.assign(settings.parallel_download, {
            max_parallel: parseInt(document.getElementById('setting-max-parallel').value),
            wait_slot_timeout: parseInt(document.getElementById('setting-wait-slot-timeout').value),
        });
        Object.assign(settings.download_mode, {
            mode: document.getElementById('setting-download-mode').value,
        });
        Object.assign(settings.aria2_rpc, {
            host: document.getElementById('setting-aria2-host').value,
            port: parseInt(document.getElementById('setting-aria2-port').value),
            secret: document.getElementById('setting-aria2-secret').value,
        });
        Object.assign(settings.aria2c_basic, {
            max_connection_per_server: parseInt(document.getElementById('setting-max-conn-per-server').value),
            split: parseInt(document.getElementById('setting-split').value),
            min_split_size: document.getElementById('setting-min-split-size').value,
            max_tries: parseInt(document.getElementById('setting-max-tries').value),
            retry_wait: parseInt(document.getElementById('setting-retry-wait').value),
            max_concurrent_downloads: parseInt(document.getElementById('setting-max-concurrent-downloads').value),
        });
        Object.assign(settings.storage, {
            history_limit: parseInt(document.getElementById('setting-history-limit').value),
            log_limit: parseInt(document.getElementById('setting-log-limit').value),
            per_blogger_retain_default: parseInt(document.getElementById('setting-per-blogger-retain-default').value),
        });
        Object.assign(settings.download_path, {
            auto_organize: document.getElementById('setting-auto-organize').checked,
            path_template: document.getElementById('setting-path-template').value,
            conflict_strategy: document.getElementById('setting-conflict-strategy').value,
        });
        Object.assign(settings.danmaku_comment, {
            auto_download_danmaku: document.getElementById('setting-auto-download-danmaku').checked,
            auto_download_comments: document.getElementById('setting-auto-download-comments').checked,
            comments_main_limit: parseInt(document.getElementById('setting-comments-main-limit').value),
            comments_reply_mode: document.getElementById('setting-comments-reply-mode').value,
            comments_filter_regex: document.getElementById('setting-comments-filter-regex').value.trim(),
            sidecar_archive_mode: document.getElementById('setting-sidecar-archive-mode').value,
            sidecar_archive_limit: parseInt(document.getElementById('setting-sidecar-archive-limit').value),
            enable_smart_download: document.getElementById('setting-enable-smart-download').checked,
            min_publish_hours: parseInt(document.getElementById('setting-min-publish-hours').value),
            download_time_points: [..._state.smartTimePoints],
        });
        Object.assign(settings.ffmpeg, {
            mode: document.getElementById('setting-ffmpeg-mode').value,
            custom_path: document.getElementById('setting-ffmpeg-path').value.trim(),
        });
        Object.assign(settings.download.verify, {
            mode: document.getElementById('setting-verify-mode').value,
            periodic_days: parseInt(document.getElementById('setting-verify-periodic-days').value),
            periodic_batch: parseInt(document.getElementById('setting-verify-periodic-batch').value),
        });
        settings.board.path_display_mode = document.getElementById('setting-path-display-mode').value;
        // 保留旧字段，兼容尚未升级的客户端。
        settings.board.show_relative_path = settings.board.path_display_mode === 'relative';
        settings.board.browser_download_enabled =
            document.getElementById('setting-browser-download-enabled').checked !== false;
        settings.live = {
            ...(settings.live || {}),
            max_concurrent: parseInt(document.getElementById('setting-live-max-concurrent').value),
            min_free_space_gib: parseInt(document.getElementById('setting-live-min-free-space').value),
            max_duration_hours: parseInt(document.getElementById('setting-live-max-duration').value),
            file_name_template: document.getElementById('setting-live-file-template').value.trim(),
        };
        settings.monitor.detect_reupload = document.getElementById('setting-detect-reupload').checked;
        const mpModeEl = document.getElementById('setting-multi-page-mode');
        if (mpModeEl) settings.monitor.multi_page_mode = mpModeEl.value;
        settings.refresh.l1_interval_minutes =
            parseInt(document.getElementById('setting-l1-interval-minutes').value);
        Object.assign(settings.burn, {
            opacity: parseFloat(document.getElementById('setting-burn-opacity').value),
            scroll_time: parseFloat(document.getElementById('setting-burn-scroll-time').value),
            fix_time: parseFloat(document.getElementById('setting-burn-fix-time').value),
            font_size_scale: parseFloat(document.getElementById('setting-burn-font-size-scale').value),
            bottom_reserve: parseFloat(document.getElementById('setting-burn-bottom-reserve').value),
            font_family: document.getElementById('setting-burn-font-family').value,
            color_mode: document.getElementById('setting-burn-color-mode').value,
            color: document.getElementById('setting-burn-color').value.trim().replace(/^#/, '').toUpperCase(),
        });
        Object.assign(settings.subtitle, {
            enabled: document.getElementById('setting-subtitle-enabled').checked,
            accept_ai: document.getElementById('setting-subtitle-accept-ai').checked,
            languages: document.getElementById('setting-subtitle-languages').value
                .split(',').map(s => s.trim()).filter(s => s.length > 0),
        });
        settings.update = {
            ...(settings.update || {}),
            policy: document.getElementById('setting-update-policy')?.value || 'manual',
        };

        const result = await apiPut('/api/settings', settings);
        if (result.code === 0) {
            if (!result.data || typeof result.data !== 'object') {
                throw new Error('保存设置响应缺少设置数据');
            }
            _state.settingsSnapshot = structuredClone(result.data);
            showToast(result.message || '设置已保存', result.message?.includes('Aria2 未能') ? 'warning' : 'success');
        } else {
            showToast(result.message || '保存失败', 'error');
        }
    } catch (e) {
        showToast(e?.message || '保存设置失败', 'error');
        if (e?.status === 409) await loadSettingsFromServer();
    } finally {
        if (btn) {
            btn.disabled = false;
            if (_origHtml !== null) btn.innerHTML = _origHtml;
        }
    }
}

export async function resetSettings() {
    if (!(await confirmDialog('确定要恢复默认设置吗？此操作不可恢复。', { title: '恢复默认', okText: '恢复默认', danger: true }))) return;

    try {
        const result = await apiPost('/api/settings/reset', {});
        if (result.code === 0) {
            showToast('已恢复默认设置', 'success');
            await loadSettingsFromServer();
        } else {
            showToast(result.message || '重置失败', 'error');
        }
    } catch (e) {
        showToast('重置设置失败', 'error');
    }
}

export function updatePathPreview() {
    const autoOrganize = document.getElementById('setting-auto-organize');
    if (!autoOrganize) return;
    const previewText = document.getElementById('path-preview-text');
    if (!previewText) return;

    const template = document.getElementById('setting-path-template')?.value || (autoOrganize.checked ? '{uid}/{title}' : '{title}');
    previewText.innerText = `./data/downloads/${template.replaceAll('{uid}', '123456').replaceAll('{up}', '示例UP主').replaceAll('{date}', '2026-07-26').replaceAll('{title}', '示例视频').replaceAll('{bvid}', 'BV1xx411c7mD').replaceAll('{quality}', '1080p').replaceAll('{codec}', 'av1').replaceAll('{page}', '1').replaceAll('{type}', 'video')}.mp4`;
}

export async function restartAria2(button) {
    const original = button?.innerHTML;
    if (button) {
        button.disabled = true;
        button.innerHTML = '<span class="loading"></span> 正在重连';
    }
    try {
        const result = await apiPost('/api/settings/aria2-restart', {});
        const data = result.data || {};
        if (data.restarted) {
            showToast('Aria2 已重新连接', 'success');
        } else {
            showToast(data.error || result.message || 'Aria2 重新连接失败', 'error', 5000);
        }
    } catch (error) {
        showToast(error.message || 'Aria2 重新连接失败', 'error', 5000);
    } finally {
        if (button) {
            button.disabled = false;
            button.innerHTML = original;
        }
    }
}

export function loadSettings() {
    loadSettingsFromServer();
}

// --- 基础配置只读摘要 ---
async function loadFoundationSummary() {
    const container = document.getElementById('foundation-summary-content');
    if (!container) return;
    try {
        const result = await apiGet('/api/foundation/status');
        const status = result.data || {};
        container.textContent = '';
        const modeNames = { local: '仅本机', lan: 'IPv4/IPv6 局域网', proxy: 'HTTPS 反向代理' };
        const rows = [
            ['基础配置状态', status.configuration_status === 'normal' ? '正常' : '需要检查'],
            ['AI Skill', status.ai_skill_enabled ? '已启用' : '已关闭'],
            ['当前访问模式', modeNames[status.access_mode] || status.access_mode || '未知'],
            ['基础配置入口', status.setup_access === 'remote_available' ? '可访问' : '仅服务端可访问'],
        ];
        rows.forEach(([label, value]) => {
            const row = document.createElement('p');
            row.className = 'form-note';
            row.textContent = `${label}：${value}`;
            container.appendChild(row);
        });
        if (status.restart_required) {
            const note = document.createElement('p');
            note.className = 'form-note form-note-warning';
            note.textContent = '基础网络配置已保存但尚未生效；请重启程序。';
            container.appendChild(note);
        }
    } catch (error) {
        container.textContent = `基础配置状态读取失败：${error.message || '请在服务器后端 TUI 输入 setup'}`;
    }
}

async function copyTextWithFeedback(text, successMessage) {
    try {
        if (navigator.clipboard?.writeText) {
            await navigator.clipboard.writeText(text);
        } else {
            const textarea = document.createElement('textarea');
            textarea.value = text;
            textarea.setAttribute('readonly', '');
            textarea.className = 'clipboard-fallback';
            document.body.appendChild(textarea);
            textarea.select();
            if (!document.execCommand('copy')) throw new Error('浏览器拒绝复制');
            textarea.remove();
        }
        showToast(successMessage, 'success');
    } catch (error) {
        showToast(`复制失败，请手动选择文本：${error.message || '浏览器未授权'}`, 'error', 5000);
    }
}

// --- AI Skill ---
function loadAiSkillInfo() {
    const pathBox = document.getElementById('ai-skill-path-box');

    // 从只读摘要读取状态；主 Web 不再拥有 AI 开关写入口。
    apiGet('/api/foundation/status').then(result => {
        if (result.code === 0) {
            const data = result.data || {};
            if (pathBox) pathBox.hidden = !data.ai_skill_enabled;
            // 后端返回绝对路径，复制后可直接发给 AI 使用。
            const pathText = document.getElementById('ai-skill-path-text');
            if (pathText && data.ai_skill_path) pathText.textContent = data.ai_skill_path;
        }
    }).catch(() => {});

    // 复制 Skill 路径
    const copyBtn = document.getElementById('copy-ai-skill-path-btn');
    if (copyBtn) {
        copyBtn.addEventListener('click', () => {
            const pathText = document.getElementById('ai-skill-path-text');
            if (pathText) {
                copyTextWithFeedback(pathText.textContent, '已复制路径');
            }
        });
    }
}

// --- 更新机制（检查 + 提示 + 手动更新） ---

export async function loadUpdateStatus() {
    try {
        const result = await apiGet('/api/update/status');
        const data = result.data || {};
        const currentEl = document.getElementById('update-current-version');
        if (currentEl) currentEl.textContent = data.current_version || '未知';
        const latestEl = document.getElementById('update-latest-version');
        if (latestEl) {
            latestEl.textContent = data.has_update
                ? `${data.latest_version}（有新版本）`
                : (data.latest_version || '尚未检查');
        }
        const applyBtn = document.querySelector('[data-action="apply-update"]');
        if (applyBtn) applyBtn.hidden = !data.has_update;
    } catch (e) {
        // 状态读取失败静默；"立即检查"会给出明确错误
    }
}

export async function checkUpdate() {
    const resultEl = document.getElementById('update-check-result');
    const applyBtn = document.querySelector('[data-action="apply-update"]');
    if (resultEl) resultEl.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> 正在检查更新...';
    try {
        const result = await apiPost('/api/update/check', {});
        const data = result.data || {};
        const latestEl = document.getElementById('update-latest-version');
        if (result.code === 0) {
            if (data.has_update) {
                if (latestEl) latestEl.textContent = `${data.latest_version}（有新版本）`;
                if (applyBtn) applyBtn.hidden = false;
                if (resultEl) resultEl.innerHTML = `<span class="status-success"><i class="fa-solid fa-circle-up"></i> 发现新版本 ${escapeHtml(data.latest_version)}${data.updatable ? '' : '（当前平台暂无可下载包）'}</span>`;
            } else {
                if (latestEl) latestEl.textContent = data.latest_version || '已是最新';
                if (resultEl) resultEl.innerHTML = '<span class="status-success"><i class="fa-solid fa-check-circle"></i> 已是最新版本</span>';
            }
        } else {
            if (resultEl) resultEl.innerHTML = `<span class="status-error"><i class="fa-solid fa-times-circle"></i> ${escapeHtml(result.message || '检查失败')}</span>`;
        }
    } catch (e) {
        if (resultEl) resultEl.innerHTML = '<span class="status-error"><i class="fa-solid fa-times-circle"></i> 检查更新失败</span>';
    }
}

export async function applyUpdate() {
    const applyBtn = document.querySelector('[data-action="apply-update"]');
    const resultEl = document.getElementById('update-check-result');
    if (!(await confirmDialog('确定要立即更新吗？更新只替换程序文件、不触碰 data/ 目录；完成后需重启程序生效。', { title: '立即更新', okText: '更新' }))) return;
    if (applyBtn) applyBtn.disabled = true;
    if (resultEl) resultEl.innerHTML = '<i class="fa-solid fa-spinner fa-spin"></i> 正在下载并校验更新...';
    try {
        const result = await apiPost('/api/update/apply', {});
        if (result.code === 0) {
            if (resultEl) resultEl.innerHTML = `<span class="status-success"><i class="fa-solid fa-check-circle"></i> ${escapeHtml(result.message || '更新完成')}</span>`;
            showToast(result.message || '更新完成', 'success', 6000);
            await loadUpdateStatus();
        } else {
            if (resultEl) resultEl.innerHTML = `<span class="status-error"><i class="fa-solid fa-times-circle"></i> ${escapeHtml(result.message || '更新失败')}</span>`;
        }
    } catch (e) {
        if (resultEl) resultEl.innerHTML = '<span class="status-error"><i class="fa-solid fa-times-circle"></i> 更新失败</span>';
    } finally {
        if (applyBtn) applyBtn.disabled = false;
    }
}

// --- 全局日志（跨博主，设置页展示，15 秒轮询） ---
_state.globalLogsTimer = null;
_state.globalLogsInFlight = false;

export async function refreshGlobalLogs() {
    if (_state.globalLogsInFlight) return;
    _state.globalLogsInFlight = true;
    try {
        const container = document.getElementById('global-logs-list');
        if (!container) return;
        const result = await apiGet('/api/logs/get?limit=100');
        const logs = result.data?.logs || [];
        if (!logs.length) {
            container.innerHTML = '<div class="empty-state empty-state-padded"><i class="fa-solid fa-inbox"></i><p>暂无日志</p></div>';
            return;
        }
        container.innerHTML = logs.map(l => {
            const level = l.level || 'info';
            const time = l.time || '--:--:--';
            const msg = l.msg || l.message || '';
            const uidTag = l.uid ? `<span class="log-uid">[${escapeHtml(String(l.uid))}]</span>` : '';
            return `<div class="log-entry log-level-${escapeHtml(level)}"><span class="log-time">${escapeHtml(time)}</span>${uidTag}<span>${escapeHtml(msg)}</span></div>`;
        }).join('');
        container.scrollTop = container.scrollHeight;
    } catch (e) {
        // 静默处理网络错误，轮询会自动重试
    } finally {
        _state.globalLogsInFlight = false;
    }
}

export function startGlobalLogsPolling() {
    if (_state.globalLogsTimer) return;
    refreshGlobalLogs();
    _state.globalLogsTimer = setInterval(() => {
        if (document.hidden) return;
        const settingsTab = document.getElementById('tab-settings');
        if (!settingsTab || !settingsTab.classList.contains('active')) return;
        refreshGlobalLogs();
    }, 15000);
}

// --- 移动端适配 ---
export function initMobileSidebar() {
    const dashboard = document.querySelector('.blogger-dashboard');
    if (!dashboard) return;
    
    // 检查是否已存在按钮
    if (document.querySelector('.sidebar-toggle-btn')) return;
    
    // 创建切换按钮
    const toggleBtn = document.createElement('div');
    toggleBtn.className = 'sidebar-toggle-btn';
    toggleBtn.innerHTML = '<div class="mobile-sidebar-label"><i class="fa-solid fa-bars"></i> <span>博主列表</span></div><i class="fa-solid fa-chevron-right"></i>';
    
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
    const sidebarList = document.getElementById('blogger-sidebar-list');
    if (sidebarList) {
        sidebarList.addEventListener('click', (e) => {
            if (window.innerWidth <= 768 && e.target.closest('.blogger-list-item')) {
                if (sidebar) sidebar.classList.remove('active');
                overlay.classList.remove('active');
            }
        });
    }
}

// --- 测试功能 ---
// "测试下载"已按审计决定（R1）整体删除：/api/video/get-video-urls 是抽屉真实使用的
// 接口，保留；只删除这个调试入口（原用原生 window.prompt()，体验差且无实际用途）。

function applyTheme(theme) {
    const root = document.documentElement;
    if (theme === 'system') root.removeAttribute('data-theme');
    else root.dataset.theme = theme;
}
