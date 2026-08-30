import { test, expect } from '../web/node_modules/@playwright/test/index.mjs';

function envelope(data = {}, message = 'ok') {
  return { code: 0, message, data };
}

async function mockVueApi(page) {
  const state = { paired: false, pairRejects: false, setupCompleted: true, setupRequiresAuth: true, cookies: false, calls: [], liveSource: true, recording: false, historyEntry: true };
  await page.route('**/socket.io/**', route => route.abort());
  await page.route('**/api/**', async route => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    state.calls.push({ path: url.pathname + url.search, method: request.method(), body: request.postDataJSON?.() ?? null });

    let data = {};
    if (path === '/api/ready') data = { ok: true };
    else if (path === '/api/auth/state') {
      data = state.paired
        ? { authenticated: true, role: 'owner' }
        : { authenticated: false, pairing_open: true, pairing_expires_at: Math.floor(Date.now() / 1000) + 300 };
    } else if (path === '/api/auth/csrf') {
      data = { csrf_token: 'vue-test-csrf' };
    } else if (path === '/api/auth/pair') {
      if (state.pairRejects) {
        await route.fulfill({ status: 401, contentType: 'application/json', body: JSON.stringify({ code: 401, message: '配对码无效，请稍后重试', data: null }) });
        return;
      }
      state.paired = true;
      data = { paired: true };
    } else if (path === '/api/cookies/status') data = state.cookies
      ? { configured: true, has_cookies: true, valid: true, uname: 'Vue 测试用户', mid: 123 }
      : { configured: false, has_cookies: false, valid: false };
    else if (path === '/api/setup/status') {
      // 主端口该端点需要会话；Setup 端口免认证（setupRequiresAuth=false）。
      if (state.setupRequiresAuth && !state.paired) {
        await route.fulfill({ status: 401, contentType: 'application/json', body: JSON.stringify({ code: 401, message: '需要先完成设备配对', data: null }) });
        return;
      }
      data = { completed: state.setupCompleted, mode: 'local', configured_mode: 'local' };
    }
    else if (path === '/api/setup/detect') data = { ipv4: ['127.0.0.1'], ipv6: [] };
    else if (path === '/api/setup/ports') data = { main_port: 5000, setup_port: 5001, main_url: 'http://127.0.0.1:5000', setup_url: 'http://127.0.0.1:5001', accessible_urls: [] };
    else if (path === '/api/settings') data = {};
    else if (path === '/api/settings/ffmpeg-path') data = { available: true, path: '' };
    else if (path === '/api/update/status') data = {};
    else if (path === '/api/blogger/list' || path === '/api/blogger/saved/list') data = { bloggers: [] };
    else if (path === '/api/blogger/search') data = { users: [{ uid: 123, name: 'Vue 测试博主', face: '', sign: '' }], total: 1 };
    else if (path === '/api/task/next-check') data = { bloggers: {} };
    else if (path === '/api/download/health') data = { aria2_connected: true, queue_running: 0, queue_pending: 0, queue_paused: 0 };
    else if (path === '/api/live/dashboard') data = {
      sources: state.liveSource ? [{ id: 7, room_id: 123456, anchor_name: 'Vue 测试主播', auto_record_enabled: false, max_qn: 10000, capture_mode: 'standard', runtime: { live_status: 1 } }] : [],
      sessions: state.recording ? [{ recording_id: 9, room_id: 123456, status: 'recording', title: 'Vue 测试直播' }] : [],
      monitor: { running: true }, merge_jobs: [], recovery: [], disk: { available_bytes: 1000 },
    };
    else if (path === '/api/live/history') data = { items: [] };
    else if (path === '/api/live/room-info') data = { room_id: 123456, is_recording: state.recording, can_start: !state.recording };
    else if (path === '/api/live/start') { state.recording = true; data = { recording_id: 9, room_id: 123456, status: 'recording' }; }
    else if (path === '/api/live/stop') { state.recording = false; data = { ok: true }; }
    else if (path === '/api/live/source/delete') { state.liveSource = false; data = { ok: true }; }
    else if (path === '/api/history/list') data = state.historyEntry
      ? { items: [{ uid: '123', name: 'Vue 测试博主', videos: [{ history_id: 42, bvid: 'BV1TEST', title: '历史测试视频', file_path: 'D:/video.mp4', download_time: '2026-08-20T10:00:00Z', sidecar: {} }] }], counts: { completed: 1, failed: 0, downloading: 0 } }
      : { items: [], counts: { completed: 0, failed: 0, downloading: 0 } };
    else if (path === '/api/history/delete') { state.historyEntry = false; data = { ok: true }; }
    else if (path === '/api/logs/get') data = { logs: [] };
    else {
      await route.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ code: 500, message: `unmocked ${path}`, data: null }) });
      return;
    }
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(envelope(data)) });
  });
  return state;
}

test('Vue app shows pairing before business initialization', async ({ page }) => {
  const state = await mockVueApi(page);
  await page.goto('/app/');
  await expect(page.getByRole('heading', { name: '补哩补哩远程配对' })).toBeVisible();
  expect(state.calls.slice(0, 2).map(call => call.path)).toEqual(['/api/ready', '/api/auth/state']);
  // /api/setup/status 是入口的能力探测（主端口未配对时 401），不属于业务初始化。
  const entryPaths = new Set(['/api/ready', '/api/auth/state', '/api/setup/status']);
  expect(state.calls.every(call => entryPaths.has(call.path))).toBeTruthy();
  // 探测性 401 不代表会话失效：不得弹出「设备会话已失效」并清空业务状态。
  await expect(page.getByText('设备会话已失效，请重新配对')).toHaveCount(0);
  // 异步样式表由主 bundle 在加载完成后并入渲染（内联 onload 会被 CSP 拦掉）。
  await expect(page.locator('link[href*="fontawesome.min.css"]')).toHaveJSProperty('media', 'all');
  await expect(page.locator('#pair-code')).toBeEnabled();
});

test('Setup 向导未完成时先进向导，不先卡在配对页', async ({ page }) => {
  const state = await mockVueApi(page);
  state.setupCompleted = false;
  state.setupRequiresAuth = false; // Setup 端口免认证提供向导 API
  await page.goto('/app/');
  await expect(page.getByRole('heading', { name: '欢迎使用补哩补哩' })).toBeVisible();
  expect(state.calls.some(call => call.path === '/api/auth/pair')).toBeFalsy();
  expect(state.calls.some(call => call.path === '/api/cookies/status')).toBeFalsy();
});

test('配对码被拒时只给行内原因，不误报设备会话失效', async ({ page }) => {
  const state = await mockVueApi(page);
  state.pairRejects = true;
  await page.goto('/app/');
  await page.locator('#pair-code').fill('ABCD-EFGH');
  await page.getByRole('button', { name: '确认配对' }).click();
  await expect(page.locator('.hint')).toHaveText('配对码无效，请稍后重试');
  await expect(page.getByText('设备会话已失效，请重新配对')).toHaveCount(0);
});

test('配对码输入从整段粘贴文本中精确提取', async ({ page }) => {
  await mockVueApi(page);
  await page.goto('/app/');
  const input = page.locator('#pair-code');
  // maxlength=9 会截断键入的长文本，粘贴路径只能靠真实 paste 事件覆盖。
  const paste = text => input.evaluate((el, value) => {
    const transfer = new DataTransfer();
    transfer.setData('text', value);
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true, cancelable: true }));
  }, text);

  await paste('{"ok":true,"data":{"pairing_code":"ABCD-K234"}}');
  await expect(input).toHaveValue('ABCD-K234');
  await paste('首次设备配对码：QR7T-2KW9（10 分钟内有效，仅可使用一次。');
  await expect(input).toHaveValue('QR7T-2KW9');
  // 粘贴内容里没有完整 8 位码：不拦截默认行为，输入框保持上一次的结果。
  await paste('note to self');
  await expect(input).toHaveValue('QR7T-2KW9');
  // 逐字键入仍走原有归一规则：小写转大写、第 5 位补短横。
  await input.fill('');
  await input.pressSequentially('ab2d');
  await expect(input).toHaveValue('AB2D');
});

test('Vue pairing enters the main app and uses aligned request contracts', async ({ page }) => {
  const state = await mockVueApi(page);
  await page.goto('/app/');
  await page.locator('#pair-code').fill('ABCD-EFGH');
  await expect(page.locator('#pair-code')).toHaveValue('ABCD-EFGH');
  await page.getByRole('button', { name: '确认配对' }).click();
  // B 站强制登录门：未登录 B 站时先拦截，不允许直接进主界面。
  await expect(page.getByRole('heading', { name: '请先登录 B 站账号' })).toBeVisible();
  expect(state.calls.some(call => call.path === '/api/settings')).toBeFalsy();

  // 模拟扫码登录成功（Cookie 生效），重新加载后放行进入主界面。
  state.cookies = true;
  await page.reload();
  await expect(page.locator('#main-content')).toBeVisible();

  // 未激活的页面不能在后台初始化或轮询。
  expect(state.calls.some(call => call.path === '/api/settings')).toBeFalsy();
  expect(state.calls.some(call => call.path === '/api/live/dashboard')).toBeFalsy();
  expect(state.calls.some(call => call.path === '/api/history/list')).toBeFalsy();
  expect(state.calls.some(call => call.path === '/api/task/next-check')).toBeFalsy();

  await page.locator('#blogger-search-input').fill('测试');
  await page.getByRole('button', { name: '搜索' }).click();
  await expect(page.locator('#blogger-search-results').getByText('Vue 测试博主')).toBeVisible();
  const search = state.calls.find(call => call.path.startsWith('/api/blogger/search'));
  expect(search?.method).toBe('GET');
  expect(new URLSearchParams(search?.path.split('?')[1] || '').get('q')).toBe('测试');

  await page.locator('#tab-live-label').click();
  // 对齐修复后进入直播页会自动选中首个房间，详情面板也会显示主播名；
  // 用侧边栏房间按钮的 role 精确定位，避免 strict mode 命中两个元素。
  await page.getByRole('button', { name: /Vue 测试主播/ }).click();
  // 老框架 live.js:319 的开始录制按钮文案是「手动录制」。
  await page.getByRole('button', { name: '手动录制' }).click();
  const start = state.calls.find(call => call.path === '/api/live/start');
  expect(start?.body).toEqual({ room_id: 123456 });

  // 老框架详情面板与录制列表各有一个「停止并合并」（live.js:318/513），限定详情面板那个。
  const stopButton = page.locator('#live-detail-content').getByRole('button', { name: '停止并合并' });
  await expect(stopButton).toBeVisible();
  await stopButton.click();
  await page.locator('#confirm-modal-ok').click();
  const stop = state.calls.find(call => call.path === '/api/live/stop');
  expect(stop?.body).toEqual({ room_id: 123456 });

  // 老框架 live.js:364 删除直播源按钮文案是「删除源」。
  await page.getByRole('button', { name: '删除源' }).click();
  await page.locator('#confirm-modal-ok').click();
  const remove = state.calls.find(call => call.path === '/api/live/source/delete');
  expect(remove?.body).toEqual({ room_id: 123456 });

  await page.locator('#tab-history-label').click();
  // 与老框架一致：看板卡片没有删除按钮，点卡片打开抽屉，在抽屉里删除记录（连文件一起删）。
  await page.getByText('历史测试视频').click();
  // 已下载抽屉没有单文件「下载」按钮（老框架无此 UI，对齐修复时已移除）。
  await page.getByRole('button', { name: '删除记录' }).click();
  await page.locator('#confirm-modal-ok').click();
  const deleted = state.calls.find(call => call.path === '/api/history/delete');
  expect(deleted?.body).toEqual({ bvid: 'BV1TEST', history_id: 42, delete_files: true });

  // 只在进入设置页时加载设置，避免隐藏 Tab 在启动阶段抢占请求限额。
  await page.locator('#tab-settings-label').click();
  await expect(page.locator('#setting-theme')).toBeVisible();
  expect(state.calls.some(call => call.path === '/api/settings')).toBeTruthy();
  expect(state.calls.some(call => call.path === '/api/live/dashboard')).toBeTruthy();
});
