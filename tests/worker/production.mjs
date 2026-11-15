import { registerFinalLogScan } from './runtime-output.mjs';
import { installedWebApp, signinApp } from './app-config.mjs';
import assert from 'node:assert/strict';
import { identitySigner } from './restate-identity.mjs';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';

// Consume worker-build output directly, without either runtime-test shim.
const configurations = JSON.parse(readFileSync(new URL('./.production.json', import.meta.url)));
for (const { name, scriptPath } of configurations) {
  let outgoing = 0;
  const signer = identitySigner();
  const logs = [];
  let logBytes = 0;
  const options = {
    handleRuntimeStdio(stdout, stderr) {
      for (const stream of [stdout, stderr]) stream.on("data", chunk => {
        logBytes += chunk.length;
        logs.push(chunk.toString());
      });
    },
    log: new Log(LogLevel.ERROR), modules: true, scriptPath,
    // Keep worker/shim.mjs as the entrypoint while allowing its ../index.js
    // import inside the complete artifact, independently of the caller's cwd.
    modulesRoot: resolve(dirname(scriptPath), '..'),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }, { type: 'ESModule', include: ['**/*.js'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    ratelimits: { RL_IP: { simple: { limit: 30, period: 60 } }, RL_USER: { simple: { limit: 10, period: 60 } }, AUTHZ_RL: { simple: { limit: 10, period: 60 } } },
  kvNamespaces: ['SESSIONS', 'AUTHZ_CACHE'], d1Databases: ['DB'],
    bindings: {
      RESTATE_IDENTITY_KEYS: signer.key,
      P1_HOST: 'production.test', P1_KEYRING: 'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',
      RESTATE_INGRESS_URL: 'https://restate.invalid', RESTATE_INGRESS_TOKEN: 'synthetic-worker-ingress-key',
      GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
      ...signinApp, ...installedWebApp, GHINVITE_WEBHOOK_SECRET: 'dummy',
      GH_REPO_APP_ID: '123', GH_REPO_CLIENT_ID: 'Iv.fixture-repo',
      GH_REPO_PRIVATE_KEYS: `fixture:${readFileSync(new URL('../../test-fixtures/github-app-rsa.pem', import.meta.url)).toString('base64')}`,
    },
    outboundService() { outgoing++; return new Response('Outbound disabled', { status: 502 }); },
  };
  const mf = new Miniflare(options);
  try {
    if (name === 'ghinvite-web') {
      const response = await mf.dispatchFetch('https://production.test/health');
      assert.equal(response.status, 200);
      assert.equal(await response.text(), 'ok');
      const scheduledWorker = await mf.getWorker();
      assert.notEqual((await scheduledWorker.scheduled({ cron: '5 * * * *' })).outcome, 'ok', 'missing schema must fail the production scheduled event');
      const db = await mf.getD1Database('DB');
      for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(f => f.endsWith('.sql')).sort()) {
        const sql = readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8');
        await db.exec(sql.replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
      }
      assert.equal((await scheduledWorker.scheduled({ cron: '5 * * * *' })).outcome, 'ok');
      const digest = await db.prepare('SELECT * FROM blocked_digest_days').first();
      assert.equal(digest.complete, 1);
      assert.equal(digest.day, digest.observed_at.slice(0, 10));
      assert.equal((await scheduledWorker.scheduled({ cron: '5 * * * *' })).outcome, 'ok');
      assert.deepEqual(await db.prepare('SELECT * FROM blocked_digest_days').first(), digest);
      console.log('PASS production generated scheduled entrypoint: failure, recovery and frozen daily snapshot');

    } else {
      const response = await mf.dispatchFetch('https://production.test/discover', {
        headers: { accept: 'application/vnd.restate.endpointmanifest.v3+json', ...signer.headers('/discover') },
      });
      assert.equal(response.status, 200);
      const manifest = await response.json();
      for (const name of ['InvitationLink', 'InvitationProjection', 'Installation', 'AccountInstallation', 'InstallationProjection']) {
        assert.ok(manifest.services.some(service => service.name === name), `${name} must be packaged`);
      }
    }
    // Enumerate the source's fixture routes, so new fixture operations cannot be
    // silently omitted from the production exclusion check.
    const source = readFileSync(new URL(`../../crates/${name === 'ghinvite-web' ? 'ghinvite-web-worker' : 'ghinvite-workflows-worker'}/src/fixture.rs`, import.meta.url), 'utf8');
    const paths = name === 'ghinvite-web'
      ? [...source.matchAll(/method == "([^"]+)"|^\s*"([^"]+)"\s*=>/gm)].map(match => `/__fixture/${match[1] || match[2]}`)
      : [...source.matchAll(/"(\/__fixture\/[^\"]+)"/g)].map(match => match[1]);
    assert.ok(paths.length >= 3, 'fixture inventory must not be empty');
    for (const path of new Set(paths)) {
      const response = await mf.dispatchFetch(`https://production.test${path}`, {
        method: 'POST', body: 'null', headers: { 'content-type': 'application/json', ...(name === 'ghinvite-web' ? {} : signer.headers(path)) }, redirect: 'manual',
      });
      const body = await response.text();
      assert.equal(response.status, name === 'ghinvite-web' ? 404 : 400, `${name} must not expose ${path}: ${body}`);
      if (name !== 'ghinvite-web') assert.ok(body.includes(`Bad path '${path}'`), 'production Restate router rejects fixture paths');
    }
    if (name !== 'ghinvite-web') {
      for (const path of ['/discover', '/invoke/InvitationLink/create', '/health']) {
        const denied = await mf.dispatchFetch(`https://production.test${path}`);
        assert.equal(denied.status, 401, 'production requires request identity');
        await denied.text();
      }
    }
    assert.equal(outgoing, 0);
    console.log(`PASS production-feature ${name}: runtime entrypoint, fixture endpoints absent`);
  } finally {
    await mf.dispose();
  }
  if (name === 'ghinvite-web') {
    for (const [label, changed] of [
      ['missing-host', {P1_HOST:undefined,GHINVITE_BASE_URL:'https://production.test'}],
      ['missing-ingress-token', {RESTATE_INGRESS_TOKEN:undefined,GHINVITE_RESTATE_AUTH:'local-unauthenticated'}],
      ['http-ingress', {RESTATE_INGRESS_URL:'http://127.0.0.1:8080'}],
      ['credential-ingress', {RESTATE_INGRESS_URL:'https://SECRET_MARKER@ingress.test'}],
      ['noncanonical-ingress', {RESTATE_INGRESS_URL:'https://ingress.test.'}],
      ['malformed-token', {RESTATE_INGRESS_TOKEN:'SECRET_MARKER\n'}],
      ['malformed-oauth', {GH_SIGNIN_CLIENT_SECRET:'SECRET_MARKER\n'}],
      ['entropy-fixture', {P1_LINK_SECRET_FIXTURE:'known-marker-v1'}],
      ['empty-entropy-fixture', {P1_LINK_SECRET_FIXTURE:''}],
    ]) {
      const bindings = Object.fromEntries(Object.entries({...options.bindings,...changed}).filter(([,value])=>value!==undefined));
      const rejected = new Miniflare({...options,bindings});
      try {
        const response = await rejected.dispatchFetch('https://production.test/health');
        assert.equal(response.status,500,label);
        assert.equal(await response.text(),'Service configuration unavailable',label);
      } finally { await rejected.dispose(); }
    }
    console.log('PASS production web canonical host/HTTPS bearer configuration, no aliases or entropy fixture binding');
  }
  if (name !== 'ghinvite-web') {
    for (const [label, changed] of [
      ['missing-identity', { RESTATE_IDENTITY_KEYS: undefined }],
      ['empty-identity', { RESTATE_IDENTITY_KEYS: '' }],
      ['malformed-identity', { RESTATE_IDENTITY_KEYS: 'publickeyv1_SECRET_MARKER' }],
      ['fixture-binding', { P1_DELIVERY_RUN_RETRY_FIXTURE: 'three-losses-v1' }],
      ['empty-fixture-binding', { P1_DELIVERY_RUN_RETRY_FIXTURE: '' }],
    ]) {
      const bindings = Object.fromEntries(Object.entries({ ...options.bindings, ...changed })
        .filter(([, value]) => value !== undefined));
      const rejected = new Miniflare({ ...options, bindings });
      try {
        const response = await rejected.dispatchFetch('https://production.test/discover', {
          headers: { accept: 'application/vnd.restate.endpointmanifest.v3+json', ...signer.headers('/discover') },
        });
        assert.equal(response.status, 500, label);
        assert.doesNotMatch(await response.text(), /SECRET_MARKER|BEGIN PRIVATE KEY/);
      } finally { await rejected.dispose(); }
    }
    console.log('PASS production workflow startup rejects missing/invalid identity and any delivery retry fixture binding');
  }
  assert.ok(logBytes <= 1_048_576, 'startup logs must be complete within bound');
  assert.doesNotMatch(logs.join(''), /SECRET_MARKER|BEGIN PRIVATE KEY/);
  assert.equal(outgoing, 0);

}

registerFinalLogScan(bytes => { assert.doesNotMatch(bytes.toString(),/SECRET_MARKER|BEGIN PRIVATE KEY/); });
