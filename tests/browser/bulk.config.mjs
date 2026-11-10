import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.', testMatch: 'bulk.spec.mjs', workers: 1,
  use: { baseURL: 'http://127.0.0.1:4198', browserName: 'chromium' },
  projects: [
    { name: 'javascript', use: { javaScriptEnabled: true } },
    { name: 'no-javascript', use: { javaScriptEnabled: false } },
  ],
  webServer: {
    command: 'cargo test -p ghinvite-web --test bulk_console bulk_browser_server -- --ignored --nocapture',
    cwd: '../..', url: 'http://127.0.0.1:4198/health', timeout: 180_000,
  },
});
