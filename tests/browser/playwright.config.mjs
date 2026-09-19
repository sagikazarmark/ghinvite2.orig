import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: '*.spec.mjs',
  // Specs with their own config and fixture server. They would fail here:
  // `server.mjs` serves the invitation link form, not the routers they drive.
  testIgnore: ['admission.spec.mjs', 'document.spec.mjs', 'delivery.spec.mjs', 'read-recovery.spec.mjs', 'mutation-recovery.spec.mjs', 'revocation.spec.mjs'],
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 2 : undefined,
  reporter: [['list'], ['html', { open: 'never' }]],
  use: {
    baseURL: 'http://127.0.0.1:4173',
    browserName: 'chromium',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: ['desktop', 'mobile'].flatMap((layout) =>
    ['light', 'dark'].map((colorScheme) => ({
      name: `${layout}-${colorScheme}`,
      use: {
        viewport: layout === 'desktop'
          ? { width: 1440, height: 1000 }
          : { width: 390, height: 844 },
        isMobile: layout === 'mobile',
        hasTouch: layout === 'mobile',
        colorScheme,
      },
    })),
  ),
  webServer: {
    command: 'node server.mjs',
    url: 'http://127.0.0.1:4173/health',
    reuseExistingServer: false,
    timeout: 300_000,
  },
});
