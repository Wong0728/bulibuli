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

// 前置检查：未构建时 static/app/index.html 不存在，webServer 探活会超时 30s+
// 且报错不指向根因；这里直接给出明确指引。
if (!fs.existsSync(new URL('../static/app/index.html', import.meta.url))) {
  console.error('\n[test] 未找到前端构建产物 ../static/app/index.html。请先执行：npm run build\n');
  process.exit(1);
}

export default defineConfig({
  testDir: '../tests',
  testMatch: 'vue_app.spec.mjs',
  timeout: 30_000,
  fullyParallel: true,
  // CI 下容忍偶发网络/时序抖动，本地保持失败立即可见
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [['dot'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: 'http://127.0.0.1:4173',
    headless: true,
    trace: 'retain-on-failure',
    ...(localChrome ? { launchOptions: { executablePath: localChrome } } : {}),
  },
  webServer: {
    command: 'python -m http.server 4173 --directory ../static',
    url: 'http://127.0.0.1:4173/app/',
    reuseExistingServer: !process.env.CI,
  },
});
