import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'admission.spec.mjs',
  workers: 1,
  timeout: 60_000,
  use: { baseURL: 'http://127.0.0.1:4174', browserName: 'chromium' },
  projects: [
    { name: 'no-javascript', use: { javaScriptEnabled: false } },
    { name: 'javascript', use: { javaScriptEnabled: true } },
  ],
  webServer: {
    command: 'cargo test -p ghinvite-web --test invitation_resolution admission_browser_server -- --ignored --nocapture',
    cwd: '../..',
    url: 'http://127.0.0.1:4174/health',
    timeout: 120_000,
  },
});
