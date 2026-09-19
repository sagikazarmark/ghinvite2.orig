import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.', testMatch: 'request-history.spec.mjs', workers: 1,
  use: { baseURL: 'http://127.0.0.1:4178', browserName: 'chromium', javaScriptEnabled: false },
  projects: [
    { name: 'desktop', use: { viewport: { width: 1440, height: 1000 } } },
    { name: 'mobile', use: { viewport: { width: 390, height: 844 }, isMobile: true } },
  ],
  webServer: {
    command: 'cargo test -p ghinvite-web --test console_flow request_history_browser_server -- --ignored --nocapture',
    cwd: '../..', url: 'http://127.0.0.1:4178/health', timeout: 120_000,
  },
});
