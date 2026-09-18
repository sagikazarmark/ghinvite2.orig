import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'read-recovery.spec.mjs',
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:4175', browserName: 'chromium', javaScriptEnabled: false },
  webServer: {
    command: 'cargo test -p ghinvite-web --test console_flow read_recovery_browser_server -- --ignored --nocapture',
    cwd: '../..',
    url: 'http://127.0.0.1:4175/health',
    timeout: 120_000,
  },
});
