import { defineConfig } from '@playwright/test';

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
    },
    webServer: {
        command: 'python -m http.server 4173 --directory ..',
        url: 'http://127.0.0.1:4173/index.html',
        reuseExistingServer: !process.env.CI,
    },
});
