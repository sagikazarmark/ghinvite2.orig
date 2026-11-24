import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'repository-picker.spec.mjs',
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:4177', browserName: 'chromium' },
  // The native form, and the same form under the shared script's selection
  // checks. No island bundle is served, so the island never mounts.
  projects: [
    { name: 'no-js', use: { javaScriptEnabled: false } },
    { name: 'shared-script', use: { javaScriptEnabled: true } },
  ],
  webServer: {
    command: 'cargo test -p ghinvite-web --test console_flow repository_picker_browser_server -- --ignored --nocapture',
    cwd: '../..',
    url: 'http://127.0.0.1:4177/health',
    timeout: 300_000,
  },
});
