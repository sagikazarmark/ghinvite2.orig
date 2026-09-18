import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';

// Consume worker-build output directly, without either runtime-test shim.
const configurations = JSON.parse(readFileSync(new URL('./.production.json', import.meta.url)));
for (const { name, scriptPath } of configurations) {
  let outgoing = 0;
  const mf = new Miniflare({
    log: new Log(LogLevel.ERROR), modules: true, scriptPath,
    // Keep worker/shim.mjs as the entrypoint while allowing its ../index.js
    // import inside the complete artifact, independently of the caller's cwd.
    modulesRoot: resolve(dirname(scriptPath), '..'),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }, { type: 'ESModule', include: ['**/*.js'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    kvNamespaces: ['SESSIONS'], d1Databases: ['DB'],
    bindings: {
      GHINVITE_ADMISSION_MODE: 'authoritative',
      GHINVITE_BASE_URL: 'https://production.test', GHINVITE_SESSION_SECRET: '07'.repeat(32),
      GHINVITE_RESTATE_INGRESS: 'https://restate.invalid', GHINVITE_RESTATE_AUTH: 'local-unauthenticated',
      GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
      GHINVITE_GITHUB_CLIENT_ID: 'dummy', GHINVITE_GITHUB_CLIENT_SECRET: 'dummy', GHINVITE_WEBHOOK_SECRET: 'dummy',
      GHINVITE_GITHUB_APP_ID: '123',
      GHINVITE_GITHUB_APP_PRIVATE_KEY: readFileSync(new URL('../../crates/ghinvite-github/src/jwt_test_key.pem', import.meta.url)).toString('base64'),
    },
    outboundService() { outgoing++; return new Response('Outbound disabled', { status: 502 }); },
  });
  try {
    if (name === 'ghinvite-web') {
      const response = await mf.dispatchFetch('https://production.test/health');
      assert.equal(response.status, 200);
      assert.equal(await response.text(), 'ok');
    } else {
      const response = await mf.dispatchFetch('https://production.test/discover', {
        headers: { accept: 'application/vnd.restate.endpointmanifest.v3+json' },
      });
      assert.equal(response.status, 200);
      const manifest = await response.json();
      for (const name of ['InvitationLinkV1', 'InvitationProjectionV1', 'Installation', 'AccountInstallationV1', 'InstallationProjectionV1']) {
        assert.ok(manifest.services.some(service => service.name === name), `${name} must be packaged`);
      }
    }
    // Enumerate the source's fixture routes, so new fixture operations cannot be
    // silently omitted from the production exclusion check.
    const source = readFileSync(new URL(`../../crates/${name === 'ghinvite-web' ? 'ghinvite-web-worker' : 'ghinvite-workflows-worker'}/src/fixture.rs`, import.meta.url), 'utf8');
    const paths = name === 'ghinvite-web'
      ? [...source.matchAll(/method == "([^"]+)"|^\s*"([^"]+)"\s*=>/gm)].map(match => `/__fixture/${match[1] || match[2]}`)
      : [...source.matchAll(/"(\/__fixture\/[^\"]+)"/g)].map(match => match[1]);
    assert.ok(paths.length >= 4, 'fixture inventory must not be empty');
    for (const path of new Set(paths)) {
      const response = await mf.dispatchFetch(`https://production.test${path}`, {
        method: 'POST', body: 'null', headers: { 'content-type': 'application/json' }, redirect: 'manual',
      });
      const body = await response.text();
      assert.equal(response.status, name === 'ghinvite-web' ? 404 : 400, `${name} must not expose ${path}: ${body}`);
      if (name !== 'ghinvite-web') assert.ok(body.includes(`Bad path '${path}'`), 'production Restate router rejects fixture paths');
    }
    assert.equal(outgoing, 0);
    console.log(`PASS production-feature ${name}: runtime entrypoint, fixture endpoints absent`);
  } finally {
    await mf.dispose();
  }
}
