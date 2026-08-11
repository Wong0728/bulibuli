import { test, expect } from '@playwright/test';

const bvid = 'BV1smoke00001';

function envelope(data = {}, message = 'ok') {
    return { code: 0, message, data };
}

function historyVideo(canOpenDirectory) {
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

async function mockApi(page, { canOpenDirectory = false } = {}) {
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
        } else if (path === '/api/download/status') {
            body = envelope({ statuses: {} });
        } else if (path === '/api/download/health') {
            body = envelope({ status: 'connected', diagnostics: {} });
        } else if (path === '/api/history/list' && url.searchParams.has('bvid')) {
            body = envelope({ video: historyVideo(canOpenDirectory) });
        } else if (path === '/api/history/list') {
            body = envelope({
                total: 1,
                server_time: Date.now(),
                counts: { downloading: 0, completed: 1, failed: 0, removed: 0, pay_blocked: 0 },
                items: [{ uid: '12345', name: 'Smoke UP 主', face: '', videos: [historyVideo(canOpenDirectory)] }],
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
    await expect(page.locator('#drawer-file-list')).toContainText('评论');
    await expect(page.locator('#video-drawer [data-action="open-history-directory"]')).toHaveCount(0);
});

test('directory capability allows the action and surfaces backend errors', async ({ page }) => {
    await openApp(page, { canOpenDirectory: true });
    await page.locator('.nav-tab[data-tab="history"]').click();
    await page.locator(`[data-action="open-video"][data-bvid="${bvid}"]`).click();
    const button = page.locator('#video-drawer [data-action="open-history-directory"]');
    await expect(button).toHaveCount(1);
    await button.click();
    await expect(page.locator('.toast')).toContainText('目录打开失败');
});
