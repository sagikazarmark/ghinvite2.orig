import { defineConfig } from '@playwright/test';

// Sign-in through the octoemu GitHub emulator (GE-09b): the fixture server is
// the local-dev configuration with GITHUB_*_BASE pointing at an embedded
// emulator. Both JavaScript modes: the bounce is plain navigation either way.
export default defineConfig({
  testDir: '.', testMatch: 'emulator-signin.spec.mjs', workers: 1, timeout: 60_000,
  use: { baseURL: 'http://127.0.0.1:4176', browserName: 'chromium' },
  projects: [
    { name: 'no-javascript', use: { javaScriptEnabled: false } },
    { name: 'javascript', use: { javaScriptEnabled: true } },
  ],
  webServer: {
    command: 'cargo test -p ghinvite-web --test emulator_browser emulator_signin_browser_server -- --ignored --nocapture',
    cwd: '../..', url: 'http://127.0.0.1:4176/health', timeout: 120_000,
  },
});
