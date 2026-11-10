import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.', testMatch: 'reissue.spec.mjs', workers: 1,
  use: { baseURL: 'http://127.0.0.1:4197', browserName: 'chromium' },
  projects: [
    { name: 'javascript', use: { javaScriptEnabled: true } },
    { name: 'no-javascript', use: { javaScriptEnabled: false } },
  ],
  webServer: {
    command: 'cargo test -p ghinvite-web --test console_delegation reissue_browser_server -- --ignored --nocapture',
    cwd: '../..', url: 'http://127.0.0.1:4197/health', timeout: 180_000,
  },
});
