// E2E 真后端冒烟用例：针对真实 bulibuli 后端进程（见 web/scripts/real-backend.mjs），
// 验证主界面可渲染且 /api/health 可达。原 mock 用例（vue_app.spec.mjs）不受影响。
import { test, expect } from '../web/node_modules/@playwright/test/index.mjs';

test('real backend smoke: home page renders and health endpoint responds', async ({
  page,
  request,
}) => {
  // 健康检查：真实后端返回标准信封 {code,message,data}。
  const health = await request.get('/api/health');
  expect(health.status()).toBe(200);
  const body = await health.json();
  expect(body.code).toBe(0);
  expect(typeof body.message).toBe('string');

  // 主界面：SPA 入口可加载且渲染出内容（全新数据目录下应呈现配对/初始化相关 UI）。
  const response = await page.goto('/');
  expect(response?.status()).toBe(200);
  await expect(page.locator('body')).not.toBeEmpty();
  const authState = await request.get('/api/auth/state');
  expect(authState.status()).toBe(200);
  const authBody = await authState.json();
  expect(authBody.data.authenticated).toBe(false);
  expect(JSON.stringify(authBody)).not.toContain('csrf_token');
});
