// E2E 真后端冒烟配置：启动真实 bulibuli 后端（独立临时数据目录 + 固定端口），
// 对其发真实 HTTP/页面请求。原全 mock 配置 playwright.config.mjs 保留不动；
// 运行方式：cd web && npx playwright test --config playwright.real-backend.mjs
import { defineConfig } from '@playwright/test';
import fs from 'node:fs';

const localChromeCandidates = process.platform === 'win32'
  ? [
      process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH,
      'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
      'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
    ].filter(Boolean)
  : [];
const localChrome = localChromeCandidates.find(candidate => fs.existsSync(candidate));

const port = Number(process.env.BILI_E2E_PORT || 4188);
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: '../tests',
  testMatch: 'real_backend.spec.mjs',
  timeout: 30_000,
  fullyParallel: false,
  // CI 下容忍偶发网络/时序抖动，本地保持失败立即可见
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [['dot'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL,
    headless: true,
    trace: 'retain-on-failure',
    ...(localChrome ? { launchOptions: { executablePath: localChrome } } : {}),
  },
  webServer: {
    command: 'node scripts/real-backend.mjs',
    url: `${baseURL}/api/health`,
    timeout: 180_000,
    reuseExistingServer: false,
  },
});
