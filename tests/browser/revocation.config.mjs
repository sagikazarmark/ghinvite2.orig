import { defineConfig } from '@playwright/test';
import recovery from './mutation-recovery.config.mjs';

export default defineConfig({
  ...recovery,
  testMatch: 'revocation.spec.mjs',
  use: { ...recovery.use, javaScriptEnabled: true },
});
