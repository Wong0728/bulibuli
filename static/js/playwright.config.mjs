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

export default defineConfig({
    testDir: '../../tests',
    testMatch: '**/*.spec.mjs',
    timeout: 30_000,
    fullyParallel: true,
    reporter: process.env.CI ? [['dot'], ['html', { open: 'never' }]] : 'list',
    use: {
        baseURL: 'http://127.0.0.1:4173',
        headless: true,
        trace: 'retain-on-failure',
        ...(localChrome ? { launchOptions: { executablePath: localChrome } } : {}),
    },
    webServer: {
        command: 'python -m http.server 4173 --directory ..',
        url: 'http://127.0.0.1:4173/index.html',
        reuseExistingServer: !process.env.CI,
    },
});
