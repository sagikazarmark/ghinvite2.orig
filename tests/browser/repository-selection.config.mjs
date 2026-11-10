import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.', testMatch: 'repository-selection.spec.mjs', workers: 1,
  timeout: 30_000, reporter: [['list']],
  use: { browserName: 'chromium' },
  projects: [
    { name: 'desktop', use: { viewport: { width: 1440, height: 1000 } } },
    { name: 'mobile', use: { viewport: { width: 390, height: 844 }, isMobile: true } },
  ],
});
