// Build the octoemu GitHub emulator (libs/octoemu) that the Worker drivers
// start through octoemu.mjs.
import { buildOctoemu } from './octoemu.mjs';

console.log(`Built octoemu at ${buildOctoemu()}`);
