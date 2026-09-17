import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'document.spec.mjs',
  workers: 1,
  timeout: 60_000,
  reporter: [['list']],
  use: { baseURL: 'http://127.0.0.1:4175', browserName: 'chromium' },
  projects: [
    {
      name: 'mobile',
      use: { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true },
    },
    { name: 'desktop', use: { viewport: { width: 1440, height: 1000 } } },
  ],
  webServer: {
    command:
      'cargo test -p ghinvite-web --test document_shell document_browser_server -- --ignored --nocapture',
    cwd: '../..',
    url: 'http://127.0.0.1:4175/health',
    timeout: 120_000,
  },
});
