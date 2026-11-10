import { defineConfig } from '@playwright/test';
import recovery from './mutation-recovery.config.mjs';
export default defineConfig({ ...recovery, testMatch: 'link-pause.spec.mjs' });
