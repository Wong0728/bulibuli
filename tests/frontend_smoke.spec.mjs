import { test, expect } from '../static/js/node_modules/@playwright/test/index.mjs';

const bvid = 'BV1smoke00001';

function envelope(data = {}, message = 'ok') {
    return { code: 0, message, data };
}

function historyVideo(canOpenDirectory, canBrowserDownload, canBurn = true) {
    return {
        bvid,
        title: 'Smoke 测试视频',
        state: 'completed',
        duration: 120,
        pub_date: '2026-08-11',
        view: 1234,
        file_path: 'downloads/12345/BV1smoke00001.mp4',
        relative_path: '12345/BV1smoke00001.mp4',
        can_open_directory: canOpenDirectory,
        can_browser_download: canBrowserDownload,
        can_burn: canBurn,
        md5: '0123456789abcdef0123456789abcdef',
        sidecar: { danmaku: true, comments: true, subtitle: true },
        burned: { danmaku: false, subtitle: false },
        blogger: { uid: '12345', name: 'Smoke UP 主' },
        files: [
            {
                file_type: 'video', name: 'BV1smoke00001.mp4', path: '12345/BV1smoke00001.mp4',
                size: 1024, format: 'mp4', location: 'auto:12345', is_current: true, version: null,
            },
            {
                file_type: 'comment', name: 'BV1smoke00001_comments.html', path: '12345/BV1smoke00001_comments.html',
                size: 128, format: 'html', location: 'auto:12345', is_current: true, version: null,
            },
        ],
    };
}

async function mockApi(page, { canOpenDirectory = false, canBrowserDownload = false, canBurn = true } = {}) {
    await page.route('**/api/**', async route => {
        const request = route.request();
        const url = new URL(request.url());
        const path = url.pathname;
        let body = envelope();
        let status = 200;

        if (path === '/api/auth/state') {
            body = envelope({ authenticated: true, csrf_token: 'playwright-csrf', role: 'operator' });
        } else if (path === '/api/blogger/list') {
            body = envelope({ bloggers: [], server_utc_offset: '+08:00' });
        } else if (path === '/api/blogger/saved/list') {
            body = envelope({ bloggers: [] });
        } else if (path === '/api/download/status') {
            body = envelope({ statuses: {} });
        } else if (path === '/api/download/health') {
            body = envelope({ status: 'connected', diagnostics: {} });
        } else if (path === '/api/history/list' && url.searchParams.has('bvid')) {
            body = envelope({ video: historyVideo(canOpenDirectory, canBrowserDownload, canBurn) });
        } else if (path === '/api/history/list') {
            body = envelope({
                total: 1,
                server_time: Date.now(),
                counts: { downloading: 0, completed: 1, failed: 0, removed: 0, pay_blocked: 0 },
                items: [{ uid: '12345', name: 'Smoke UP 主', face: '', videos: [historyVideo(canOpenDirectory, canBrowserDownload, canBurn)] }],
            });
        } else if (path === '/api/live/dashboard') {
            body = envelope({
                server_timezone: 'Asia/Shanghai',
                server_now: '2026-08-11T12:00:00+08:00',
                monitor: { running: true, last_success_at: '2026-08-11T11:59:00+08:00' },
                sources: [{
                    room_id: 123456, uid: '12345', anchor_name: 'Smoke 主播', title: 'Smoke 直播间',
                    auto_record_enabled: true, schedule_all_day: true, weekly_schedule: { mon: ['09:00-18:00'] },
                    capture_mode: 'standard', max_qn: 10000,
                    runtime: { live_status: 1, last_checked_at: '2026-08-11T11:59:00+08:00' },
                }],
                sessions: [], merge_jobs: [], recovery: [],
                disk: { available_bytes: 1024 * 1024 * 1024, total_bytes: 2 * 1024 * 1024 * 1024 },
            });
        } else if (path === '/api/live/history') {
            body = envelope({ items: [] });
        } else if (path === '/api/live/events') {
            body = envelope({ items: [], next_seq: 0 });
        } else if (path === '/api/history/open-directory') {
            status = 500;
            body = { code: 500, message: '目录打开失败（smoke mock）', data: null };
        } else if (path === '/api/video/resolve') {
            body = envelope({ media: { type: 'video_bv', id: bvid }, media_type: '' });
        } else if (path === '/api/video/info') {
            body = envelope({
                bvid, title: 'Smoke 测试视频', pic: '', duration: 100,
                pub_timestamp: 1755000000, stat: { view: 100 }, pages: [],
            });
        } else if (path === '/api/video/get-video-urls') {
            body = envelope({
                qualities: [{
                    quality: 80, quality_name: '1080P 高清', width: 1920, height: 1080,
                    format: 'mp4', url: 'https://www.bilibili.com/video-stream.mp4',
                }],
                accept_quality: [80],
            });
        } else if (path === '/api/video/get-audio-url') {
            body = envelope({
                qualities: [{ id: 30280, bandwidth: 192000, url: 'https://www.bilibili.com/audio-stream.m4s' }],
                ext: 'm4a',
            });
        } else if (path === '/api/video/gate-download') {
            body = envelope({ allow: true });
        } else {
            // 未匹配的端点不再静默返回成功：前端调错端点/拼写错误时测试会显式失败，
            // 而不是在 mock 掩盖下"全绿"。
            await route.fulfill({
                status: 500,
                contentType: 'application/json',
                body: JSON.stringify({ code: 500, message: `smoke mock 未覆盖该端点: ${path}`, data: null }),
            });
            return;
        }

        await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
    });
}

async function openApp(page, options) {
    await mockApi(page, options);
    await page.goto('/index.html');
    await expect(page.locator('#settings-fragment-mount')).toHaveAttribute('data-loaded', 'true');
}

test('live schedule switches between all-day and weekly modes', async ({ page }) => {
    await openApp(page);
    await page.getByRole('tab', { name: /直播/ }).click();
    await expect(page.locator('#live-room-list .live-room-item')).toHaveCount(1);

    await page.locator('[data-action="source-edit"]').click();
    await expect(page.locator('#live-source-modal')).toHaveClass(/active/);
    await expect(page.locator('#live-source-schedule-all-day')).toBeChecked();

    await page.locator('#live-source-schedule-weekly').check();
    await expect(page.locator('#live-schedule-mon-0-start')).toBeEnabled();
    await page.locator('#live-schedule-mon-0-start').fill('10:00');
    await page.locator('#live-schedule-mon-0-end').fill('12:00');
    await expect(page.locator('#live-schedule-error')).toHaveText('');

    await page.locator('#live-source-schedule-all-day').check();
    await expect(page.locator('#live-schedule-mon-0-start')).toBeDisabled();
    await expect(page.locator('#live-schedule-mon-0-start')).toHaveValue('');
});

test('drawer gates directory access and reports open-directory failure', async ({ page }) => {
    await openApp(page, { canOpenDirectory: false });
    await page.locator('.nav-tab[data-tab="history"]').click();
    const card = page.locator(`[data-action="open-video"][data-bvid="${bvid}"]`);
    await expect(card).toBeVisible();
    await card.click();
    await expect(page.locator('#video-drawer')).toHaveClass(/active/);
    await expect(page.locator('#video-drawer .drawer-sidecar-browser')).toContainText('评论');
    await expect(page.locator('#video-drawer [data-action="open-history-directory"]')).toHaveCount(0);
});

test('directory capability allows the action and surfaces backend errors', async ({ page }) => {
    await openApp(page, { canOpenDirectory: true });
    await page.locator('.nav-tab[data-tab="history"]').click();
    await page.locator(`[data-action="open-video"][data-bvid="${bvid}"]`).click();
    const button = page.locator('#video-drawer [data-action="open-history-directory"]').first();
    await expect(button).toBeVisible();
    await button.click();
    await expect(page.locator('.msg-toast')).toContainText('目录打开失败');
});

test('drawer server-to-local selection downloads checked files', async ({ page }) => {
    await openApp(page, { canBrowserDownload: true });
    await page.locator('.nav-tab[data-tab="history"]').click();
    await page.locator(`[data-action="open-video"][data-bvid="${bvid}"]`).click();
    await expect(page.locator('#video-drawer')).toHaveClass(/active/);

    // 未开启选择模式：勾选框隐藏、下载按钮不显示。
    const master = page.locator('#video-drawer .drawer-browser-master input');
    await expect(master).not.toBeChecked();
    await expect(page.locator('#video-drawer .drawer-file-check').first()).toBeHidden();
    await expect(page.locator('#drawer-browser-download-btn')).toBeHidden();

    // 开启主勾选框：默认全选（1 个主产物 + 1 个评论版本），出现“下载所选（2）”。
    await master.check();
    const checks = page.locator('#video-drawer .drawer-file-check input');
    await expect(checks).toHaveCount(2);
    await expect(checks.first()).toBeChecked();
    const downloadBtn = page.locator('#drawer-browser-download-btn');
    await expect(downloadBtn).toBeVisible();
    await expect(downloadBtn).toContainText('2');

    // 手动取消一个后按剩余勾选下载。
    await checks.nth(1).uncheck();
    await expect(downloadBtn).toContainText('1');

    const downloadRequest = page.waitForRequest(request =>
        request.url().includes('/api/history/file-download'));
    await downloadBtn.click();
    const request = await downloadRequest;
    const url = new URL(request.url());
    expect(url.searchParams.get('bvid')).toBe(bvid);
    expect(url.searchParams.get('path')).toBe('12345/BV1smoke00001.mp4');
    expect(url.searchParams.has('speed')).toBe(false);
});

test('drawer hides browser download entry when capability is off', async ({ page }) => {
    await openApp(page, { canBrowserDownload: false });
    await page.locator('.nav-tab[data-tab="history"]').click();
    await page.locator(`[data-action="open-video"][data-bvid="${bvid}"]`).click();
    await expect(page.locator('#video-drawer')).toHaveClass(/active/);
    await expect(page.locator('#video-drawer .drawer-browser-download-bar')).toHaveCount(0);
    await expect(page.locator('#video-drawer .drawer-file-check')).toHaveCount(0);
});

test('manual drawer saves video and audio to local via server proxy', async ({ page }) => {    await openApp(page, {});
    await page.locator('.nav-tab[data-tab="manual"]').click();
    await page.locator('.mode-btn[data-mode="link"]').click();
    await page.locator('#manual-link-input').fill(`https://www.bilibili.com/video/${bvid}`);
    await page.locator('#manual-resolve-btn').click();

    const card = page.locator(`[data-action="open-manual-video"][data-bvid="${bvid}"]`);
    await expect(card).toBeVisible();
    await card.click();
    await expect(page.locator('#video-drawer')).toHaveClass(/active/);

    // 保存到本机：视频流 + 音频流各触发一次代理下载，不落盘服务器。
    const videoRequest = page.waitForRequest(request =>
        request.url().includes('/api/download/proxy') && request.url().includes('video-stream'));
    const audioRequest = page.waitForRequest(request =>
        request.url().includes('/api/download/proxy') && request.url().includes('audio-stream'));
    await page.locator('#video-drawer [data-action="save-manual-to-local"]').click();
    const request = await videoRequest;
    expect(request.url()).toContain('filename=');
    expect(decodeURIComponent(request.url())).toContain('Smoke');
    await audioRequest;
});

test('offline failure keeps a single persistent toast and disables backend actions', async ({ page }) => {
    await openApp(page, {});
    // 后端突然不可达：中断所有 API 请求。
    await page.route('**/api/**', route => route.abort());

    // 触发失败的网络操作（进入下载管理会拉取看板）。
    await page.locator('.nav-tab[data-tab="history"]').click();
    const toasts = page.locator('.msg-toast');
    await expect(toasts).toHaveCount(1);
    await expect(toasts.first()).toContainText('网络连接异常');

    // 反复触发失败操作：始终只有同一条持续提示，不再堆叠。
    await page.locator('.nav-tab[data-tab="search"]').click();
    await page.locator('.nav-tab[data-tab="history"]').click();
    await expect(toasts).toHaveCount(1);

    // 持续显示：超过普通 toast 时长后仍在。
    await page.waitForTimeout(3200);
    await expect(toasts).toHaveCount(1);

    // 需要后端的操作全部禁用；本地切换（tab）仍可用（上面已多次点击成功）。
    await expect(page.locator('#board-refresh-btn')).toHaveClass(/network-disabled/);
    await expect(page.locator('#blogger-search-btn')).toHaveClass(/network-disabled/);
});

test('drawer disables burn buttons when ffmpeg cannot burn', async ({ page }) => {
    await openApp(page, { canBurn: false });
    await page.locator('.nav-tab[data-tab="history"]').click();
    await page.locator(`[data-action="open-video"][data-bvid="${bvid}"]`).click();
    await expect(page.locator('#video-drawer')).toHaveClass(/active/);

    const danmakuBtn = page.locator('#video-drawer [data-action="burn-media"][data-kind="danmaku"]');
    await expect(danmakuBtn).toBeVisible();
    await expect(danmakuBtn).toBeDisabled();
    await expect(danmakuBtn).toHaveAttribute('title', /FFmpeg 不支持烧录/);
    const subtitleBtn = page.locator('#video-drawer [data-action="burn-media"][data-kind="subtitle"]');
    await expect(subtitleBtn).toBeDisabled();
    await expect(subtitleBtn).toHaveAttribute('title', /设置/);

    // 支持烧录时按钮正常可用。
});
