import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'delivery.spec.mjs',
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:4175', browserName: 'chromium', trace: 'retain-on-failure' },
  projects: [
    { name: 'desktop', use: { viewport: { width: 1440, height: 1000 } } },
    { name: 'mobile-no-javascript', use: { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, javaScriptEnabled: false } },
  ],
  webServer: {
    command: 'cargo test -p ghinvite-web --test invitation_resolution delivery_browser_server -- --ignored --nocapture',
    cwd: '../..',
    url: 'http://127.0.0.1:4175/health',
    timeout: 120_000,
  },
});
