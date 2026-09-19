import { defineConfig } from '@playwright/test';
import documentConfig from './document.config.mjs';

export default defineConfig({
  ...documentConfig,
  testMatch: 'console-accessibility.spec.mjs',
  use: { ...documentConfig.use, screenshot: 'only-on-failure', trace: 'retain-on-failure' },
  projects: ['light', 'dark'].map((theme) => ({
    name: theme,
    use: { viewport: { width: 1440, height: 1000 } },
  })),
});
