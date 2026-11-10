import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { identitySigner } from './restate-identity.mjs';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';

// Calls the actual production startup path, outside /__fixture. The Rust bundle
// may enable runtime-tests for other suites; these requests do not bypass config.
const key = readFileSync(new URL('../../test-fixtures/github-app-rsa.pem', import.meta.url)).toString('base64');
const current = identitySigner();
const previous = identitySigner();
const outsider = identitySigner();
const valid = {
  RESTATE_IDENTITY_KEYS: `${current.key}, ${previous.key}`,
  P1_HOST: 'startup.test', GH_REPO_APP_ID: '123',
  GH_REPO_CLIENT_ID: 'Iv.explicit-startup-fixture', GH_REPO_PRIVATE_KEYS: `fixture:${key}`,
};
const emailRing = 'akr1;name=p1-email;current=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.81678c3e040a46b7';
const org = {...valid,P1_APP_STAGE:'repo-and-org',GH_ORG_APP_ID:'456',GH_ORG_CLIENT_ID:'Iv.fixture-org',GH_ORG_PRIVATE_KEYS:`fixture:${key}`,GH_ORG_DELIVERY_ENABLED:'true'};
const cases = [
  ['bulk-repo',{...valid,GH_BULK_ENABLED:'true'},200],
  ['bulk-email',{...org,GH_BULK_ENABLED:'true',P1_EMAIL_KEYRING:emailRing},200],
  ['bulk-invalid',{...valid,GH_BULK_ENABLED:'SECRET_MARKER_DO_NOT_ECHO'},500],
  ['bulk-email-missing-keyring',{...org,GH_BULK_ENABLED:'true'},500],
  ['bulk-email-wrong-purpose',{...org,GH_BULK_ENABLED:'true',P1_EMAIL_KEYRING:emailRing.replace('name=p1-email','name=p1')},500],
  ['bulk-email-malformed-keyring',{...valid,P1_EMAIL_KEYRING:'SECRET_MARKER_DO_NOT_ECHO'},500],
  ['reissue-enabled',{...valid,GH_REISSUE_ENABLED:'true'},200],
  ['reissue-invalid',{...valid,GH_REISSUE_ENABLED:'SECRET_MARKER_DO_NOT_ECHO'},500],

  ['valid', valid, 200],
  ['explicit-repo', {...valid,P1_APP_STAGE:'repo'},200],
  ['repo-and-org',{...valid,P1_APP_STAGE:'repo-and-org',GH_ORG_APP_ID:'456',GH_ORG_CLIENT_ID:'Iv.fixture-org',GH_ORG_PRIVATE_KEYS:`fixture:${key}`},200],
  ['org-delivery-enabled',{...valid,P1_APP_STAGE:'repo-and-org',GH_ORG_APP_ID:'456',GH_ORG_CLIENT_ID:'Iv.fixture-org',GH_ORG_PRIVATE_KEYS:`fixture:${key}`,GH_ORG_DELIVERY_ENABLED:'true'},200],
  ['org-delivery-disabled',{...valid,P1_APP_STAGE:'repo-and-org',GH_ORG_APP_ID:'456',GH_ORG_CLIENT_ID:'Iv.fixture-org',GH_ORG_PRIVATE_KEYS:`fixture:${key}`,GH_ORG_DELIVERY_ENABLED:'false'},200],
  ['org-delivery-no-stage',{...valid,GH_ORG_DELIVERY_ENABLED:'true'},500],
  ['org-delivery-invalid',{...valid,GH_ORG_DELIVERY_ENABLED:'SECRET_MARKER_DO_NOT_ECHO'},500],
  ['invalid-stage',{...valid,P1_APP_STAGE:'SECRET_MARKER_DO_NOT_ECHO'},500],
  ['empty-stage',{...valid,P1_APP_STAGE:''},500],
  ['missing-org',{...valid,P1_APP_STAGE:'repo-and-org'},500],
  ['partial-org',{...valid,P1_APP_STAGE:'repo-and-org',GH_ORG_APP_ID:'456',GH_ORG_CLIENT_ID:'Iv.fixture-org'},500],
  ['retired-key-removed', { ...valid, RESTATE_IDENTITY_KEYS: current.key }, 200],
  ['missing-identity', { ...valid, RESTATE_IDENTITY_KEYS: undefined }, 500],
  ['empty-identity', { ...valid, RESTATE_IDENTITY_KEYS: '' }, 500],
  ['malformed-identity', { ...valid, RESTATE_IDENTITY_KEYS: 'publickeyv1_SECRET_MARKER_DO_NOT_ECHO' }, 500],
  ['mixed-invalid-identity', { ...valid, RESTATE_IDENTITY_KEYS: `${current.key},SECRET_MARKER_DO_NOT_ECHO` }, 500],
  ['legacy-identity-only', { ...valid, RESTATE_IDENTITY_KEYS: undefined, RESTATE_IDENTITY_KEY: current.key }, 500],
  ['legacy-only', { GHINVITE_GITHUB_APP_ID: '123', GHINVITE_GITHUB_APP_PRIVATE_KEY: key }, 500],
  ['missing-client-id', Object.fromEntries(Object.entries(valid).filter(([key]) => key !== 'GH_REPO_CLIENT_ID')), 500],
  ['malformed-key', { ...valid, GH_REPO_PRIVATE_KEYS: 'primary:SECRET_MARKER_DO_NOT_ECHO' }, 500],
  ['malformed-client-id', { ...valid, GH_REPO_CLIENT_ID: 'SECRET_MARKER_DO_NOT_ECHO\n' }, 500],
];
for (const [name, bindings, status] of cases) {
  let outgoing = 0;
  const logs = [];
  let logBytes = 0;
  const mf = new Miniflare({
    log: new Log(LogLevel.ERROR),
    handleRuntimeStdio(stdout, stderr) {
      for (const stream of [stdout, stderr]) stream.on('data', chunk => {
        logBytes += chunk.length; logs.push(chunk.toString());
      });
    },
    modules: true, scriptPath: fileURLToPath(new URL('./workflows-worker.mjs', import.meta.url)),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    d1Databases: ['DB'], bindings: Object.fromEntries(Object.entries(bindings).filter(([,value]) => value !== undefined)),
    outboundService() { outgoing++; return new Response('Outbound disabled', { status: 502 }); },
  });
  try {
    const response = await mf.dispatchFetch('https://startup.test/discover', {
      headers: { accept: 'application/vnd.restate.endpointmanifest.v3+json', ...current.headers('/discover') },
    });
    const body = await response.text();
    assert.equal(response.status, status, `${name}: startup status`);
    assert.doesNotMatch(body, /SECRET_MARKER_DO_NOT_ECHO|-----BEGIN PRIVATE KEY-----/);
    if (status === 200) {
      const manifest = JSON.parse(body);
      const orgEnabled=bindings.GH_ORG_DELIVERY_ENABLED==='true', bulkEnabled=bindings.GH_BULK_ENABLED==='true', reissueEnabled=bindings.GH_REISSUE_ENABLED==='true';
      for (const [service,enabled] of [['OrganizationDelivery',orgEnabled],['InviteBudget',orgEnabled||bulkEnabled||reissueEnabled],['BulkInviteBatch',bulkEnabled],['EmailInviteDelivery',orgEnabled&&bulkEnabled]]) {
        assert.equal(manifest.services.some(s=>s.name===service),enabled,`${name}: ${service} must follow explicit composition`);
      }
      for (const expected of ['InvitationLink', 'Installation', 'AccountInstallation']) {
        assert.ok(manifest.services.some(service => service.name === expected), `${expected} must be registered`);
      }
    }
    if (status === 200) {
      const attempts = [
        ['previous-key', previous.headers('/discover'), bindings.RESTATE_IDENTITY_KEYS.includes(previous.key) ? 200 : 401],
        ['missing-signature', {}, 401],
        ['unsigned', { 'x-restate-signature-scheme': 'unsigned' }, 401],
        ['untrusted-key', outsider.headers('/discover'), 401],
        ['wrong-path', current.headers('/invoke/Installation/onboard'), 401],
        ['expired', current.headers('/discover', { exp: Math.floor(Date.now() / 1000) - 10 }), 401],
      ];
      for (const [test, signed, expected] of attempts) {
        const checked = await mf.dispatchFetch('https://startup.test/discover', {
          headers: { accept: 'application/vnd.restate.endpointmanifest.v3+json', ...signed },
        });
        assert.equal(checked.status, expected, test);
        await checked.text();
      }
      for (const path of ['/invoke/Installation/onboard', '/health']) {
        const denied = await mf.dispatchFetch(`https://startup.test${path}`);
        assert.equal(denied.status, 401, path);
        await denied.text();
      }
    }
    assert.equal(outgoing, 0, 'registry startup/discovery must not call GitHub');
  } finally {
    await mf.dispose();
  }
  assert.ok(logBytes <= 1_048_576, 'startup logs must be complete within bound');
  assert.doesNotMatch(logs.join(''), /SECRET_MARKER_DO_NOT_ECHO|-----BEGIN PRIVATE KEY-----/);
  console.log(`PASS actual workflow startup ${name}: HTTP ${status}, no outbound, no secret disclosure`);
}

registerFinalLogScan(bytes => { assert.doesNotMatch(bytes.toString(),/SECRET_MARKER_DO_NOT_ECHO|-----BEGIN PRIVATE KEY-----/); });
