import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'admission.spec.mjs',
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:4174', browserName: 'chromium', javaScriptEnabled: false },
  webServer: {
    command: 'cargo test -p ghinvite-web --test invitation_resolution admission_browser_server -- --ignored --nocapture',
    cwd: '../..',
    url: 'http://127.0.0.1:4174/health',
    timeout: 120_000,
  },
});
