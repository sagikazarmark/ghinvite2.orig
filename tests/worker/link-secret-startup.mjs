import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';
import { acmeSeed } from './github-world.mjs';
import { journeyWorker } from './invitation-journey.mjs';
import { startOctoemu } from './octoemu.mjs';

// The journey's Worker configuration, so its GitHub is octoemu in the
// journey's world (`node build-octoemu.mjs` builds it).
const github = await startOctoemu(acmeSeed());
try {
  // Separate isolates: failure cannot be masked by a previously valid bootstrap.
  for (const [fixture, binding, status] of [
    [false, undefined, 200], [false, 'known-marker-v1', 500], [false, '', 500],
    [true, undefined, 500], [true, 'wrong', 500], [true, 'known-marker-v1', 200],
  ]) {
    const worker = journeyWorker('https://restate.invalid', github);
    delete worker.name;
    delete worker.bindings.P1_LINK_SECRET_FIXTURE;
    if (binding !== undefined) worker.bindings.P1_LINK_SECRET_FIXTURE = binding;
    worker.scriptPath = fileURLToPath(new URL(fixture ? './link-secret-worker.mjs' : './worker.mjs', import.meta.url));
    const mf = new Miniflare({ ...worker, log: new Log(LogLevel.ERROR) });
    try {
      const response = await mf.dispatchFetch('https://journey.test/i', {
        headers: { 'CF-Connecting-IP': '192.0.2.31', 'x-test-link-secret-fixture': 'known-marker-v1' },
      });
      assert.equal(response.status, status, 'entropy fixture requires both dedicated build and deployment binding');
      const body = await response.text();
      assert.ok(!body.includes('LINKSECRET_MARKER_'));
      if (status === 500) assert.equal(body, 'Service configuration unavailable');
    } finally { await mf.dispose(); }
  }
} finally { await github.stop(); }
console.log('PASS actual Worker entropy fixture startup: ordinary build rejects binding; dedicated build requires exact binding; request header cannot activate it');
