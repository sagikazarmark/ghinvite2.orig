// The octoemu GitHub emulator for the Worker drivers (GE-09b).
//
// `node build-octoemu.mjs` builds the binary once (libs/octoemu, its own
// workspace and target directory); `startOctoemu(seed)` spawns it on a free
// loopback port, seeds it through the admin API and returns a handle. The
// Workers reach it through the GE-09a bindings (`emulator.bindings`), which
// only their `runtime-tests` builds accept. Failures are injected with fault
// rules and observed in the call log, never with an inline GitHub fake.
// `OCTOEMU_BIN` selects a prebuilt binary.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { existsSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { trackFixtureOutput } from './runtime-output.mjs';

const emulatorWorkspace = fileURLToPath(new URL('../../libs/octoemu/', import.meta.url));
const manifest = join(emulatorWorkspace, 'Cargo.toml');

// `OCTOEMU_TARGET_DIR` builds into another target directory (CI uses the
// root workspace's, which its Rust cache keeps).
const targetDir = () => process.env.OCTOEMU_TARGET_DIR ? resolve(process.env.OCTOEMU_TARGET_DIR) : undefined;

/** Builds the `octoemu` binary (the emulator workspace's target directory). */
export function buildOctoemu() {
  execFileSync('cargo', ['build', '--locked', '--manifest-path', manifest, '-p', 'octoemu', '--bin', 'octoemu',
    ...(targetDir() ? ['--target-dir', targetDir()] : [])], {
    stdio: 'inherit', timeout: 600_000, killSignal: 'SIGKILL',
  });
  return octoemuBinary();
}

/** `OCTOEMU_BIN`, else the debug binary `buildOctoemu` builds. */
export function octoemuBinary() {
  if (process.env.OCTOEMU_BIN) return process.env.OCTOEMU_BIN;
  if (targetDir()) return join(targetDir(), 'debug', 'octoemu');
  const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--offline', '--no-deps', '--format-version', '1', '--manifest-path', manifest], {
    encoding: 'utf8', timeout: 120_000,
  }));
  return join(metadata.target_directory, 'debug', 'octoemu');
}

// ghinvite's fixture App key (test-fixtures/github-app-rsa.pem), as the Apps'
// registered public key: the Workers sign their App JWTs with the private half.
export const fixturePublicKey = readFileSync(new URL('../../libs/octoemu/crates/octoemu/tests/fixtures/rsa2048.pub.pem', import.meta.url), 'utf8');

/**
 * Seed parts shared by the drivers: the sign-in App (client ID and secret as
 * in app-config.mjs) calling back to `hosts`, and the repository App 123.
 */
export function signinAppSeed({ id = 1, clientId = 'Iv.fixture-signin', clientSecret = 'fixture-signin-secret', hosts = [] } = {}) {
  return { id, slug: 'p1-signin', client_id: clientId, client_secret: clientSecret, public_keys: [fixturePublicKey],
    callback_urls: hosts.map(host => `https://${host}/oauth/callback`),
    user_permissions: { email_addresses: 'read' } };
}
export function repoAppSeed({ id = 123, clientId = 'Iv.fixture-repo' } = {}) {
  return { id, slug: 'p1-repo', client_id: clientId, client_secret: 'fixture-repo-secret', public_keys: [fixturePublicKey],
    permissions: { administration: 'write', metadata: 'read' } };
}

/** One running emulator process. */
export class Octoemu {
  constructor(child, ready) {
    this.child = child;
    this.apiBase = ready.api_base;
    this.webBase = ready.web_base;
    this.adminBase = ready.admin_base;
    this.avatarBase = `${ready.api_base}/avatars`;
    this.origin = new URL(ready.api_base).origin;
  }

  /** The GE-09a bindings pointing a `runtime-tests` Worker at this emulator. */
  get bindings() {
    return { GITHUB_API_BASE: this.apiBase, GITHUB_WEB_BASE: this.webBase, GITHUB_AVATAR_BASE: this.avatarBase };
  }

  /** Whether a Worker's outbound request is addressed to this emulator. */
  owns(request) { return new URL(request.url).origin === this.origin; }

  /**
   * Passes a Worker's outbound request through to the emulator unchanged, for
   * drivers whose `outboundService` also handles other origins (Restate).
   * `content-length` is dropped because fetch sets it from the body. Miniflare
   * installs its undici as the global dispatcher, and from 7.2x undici rejects
   * the doubled value ("invalid content-length header"). The Restate ingress
   * forwarders in the other drivers drop it for the same reason.
   */
  async forward(request) {
    const body = ['GET', 'HEAD'].includes(request.method) ? undefined : Buffer.from(await request.arrayBuffer());
    const response = await fetch(request.url, { method: request.method, headers: [...request.headers].filter(([name]) => name !== 'content-length'), body, redirect: 'manual' });
    return new Response([204, 205, 304].includes(response.status) ? null : await response.arrayBuffer(), { status: response.status, headers: response.headers });
  }

  async admin(method, path, body, expected) {
    const response = await fetch(`${this.adminBase}${path}`, {
      method, headers: body === undefined ? {} : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(60_000),
    });
    const text = await response.text();
    if (expected === undefined) assert.ok(response.ok, `octoemu ${method} ${path}: ${response.status} ${text}`);
    else assert.equal(response.status, expected, `octoemu ${method} ${path}: ${text}`);
    return text ? JSON.parse(text) : null;
  }

  seed(seed) { return this.admin('POST', '/seed', seed); }
  state() { return this.admin('GET', '/state'); }
  advanceClock(seconds) { return this.admin('POST', '/clock/advance', { seconds }); }

  /**
   * The call log, optionally only the calls to these routes (`METHOD
   * /pattern`, as the log names them). A call a fault rule answered without
   * running the handler has no `route`; it counts by method and path.
   */
  async calls(...routes) {
    const { calls } = await this.admin('GET', '/calls');
    return routes.length ? calls.filter(call => routes.some(route => matches(route, call))) : calls;
  }
  clearCalls() { return this.admin('DELETE', '/calls'); }
  /** How many logged calls each of these routes received, keyed by route. */
  async count(...routes) {
    const calls = await this.calls(...routes);
    return Object.fromEntries(routes.map(route => [route, calls.filter(call => matches(route, call)).length]));
  }

  /** Adds a fault rule; returns its ID. */
  async fault(rule) { return (await this.admin('POST', '/faults', rule, 201)).id; }
  removeFault(id) { return this.admin('DELETE', `/faults/${id}`); }
  clearFaults() { return this.admin('DELETE', '/faults'); }
  releaseFault(id) { return this.admin('POST', `/faults/${id}/release`); }
  waitHeld(id, held = 1, timeoutMs = 30_000) { return this.admin('POST', `/faults/${id}/wait`, { held, timeout_ms: timeoutMs }); }

  /** Runs an invitee action as a seeded user (`act/{login}/…`). */
  act(login, path) { return this.admin('POST', `/act/${encodeURIComponent(login)}/${path}`); }

  /**
   * The browser's part of sign-in: open the authorize URL the Worker
   * redirected to, choose `login` on the emulator's consent page (as the
   * user would, by submitting its form) and return the redirect back to the
   * Worker's callback (`/oauth/callback?code=…&state=…`).
   */
  async authorize(location, login) {
    const authorize = new URL(location);
    assert.equal(authorize.origin, new URL(this.webBase).origin, 'sign-in goes to the configured GitHub web base');
    const page = await fetch(authorize, { redirect: 'manual' });
    const html = await page.text();
    assert.equal(page.status, 200, html);
    const form = [...html.matchAll(/<form method="post" action="([^"]+)">(.*?)<\/form>/gs)]
      .find(([, , inner]) => inner.includes(`name="login" value="${login}"`));
    assert.ok(form, `${login} is offered on the authorize page`);
    const fields = new URLSearchParams([...form[2].matchAll(/<input type="hidden" name="([^"]+)" value="([^"]*)">/g)]
      .map(([, name, value]) => [name, unescape(value)]));
    fields.set('authorize', '1');
    const consent = await fetch(new URL(form[1], authorize), { method: 'POST', redirect: 'manual',
      headers: { 'content-type': 'application/x-www-form-urlencoded' }, body: fields.toString() });
    assert.equal(consent.status, 302, await consent.text());
    const back = new URL(consent.headers.get('location'));
    assert.ok(back.searchParams.get('code'), `authorization answered ${back}`);
    return back;
  }

  /**
   * A whole sign-in through a web Worker: `/login`, the emulator's consent
   * page as `login`, then `/oauth/callback` with the code. `fetch` is the
   * Worker's (`mf.dispatchFetch` or a `getWorker` fetcher), `origin` its
   * `https://{P1_HOST}`. Returns the signed-in session cookie.
   */
  async signIn(fetch, origin, login, headers = {}) {
    const start = await fetch(`${origin}/login`, { redirect: 'manual', headers });
    assert.equal(start.status, 303, await start.clone().text());
    const cookie = start.headers.get('set-cookie').split(';')[0];
    const back = await this.authorize(start.headers.get('location'), login);
    assert.equal(back.origin, origin, 'the emulator calls the Worker back');
    const done = await fetch(back.href, { redirect: 'manual', headers: { ...headers, cookie } });
    assert.equal(done.status, 303, await done.clone().text());
    return done.headers.get('set-cookie').split(';')[0];
  }

  async stop() {
    if (this.child.exitCode !== null || this.child.signalCode !== null) return;
    const closed = once(this.child, 'close');
    this.child.kill('SIGTERM');
    const kill = setTimeout(() => this.child.kill('SIGKILL'), 5_000);
    try { await closed; } finally { clearTimeout(kill); }
  }
}

function matches(route, call) {
  if (call.route) return call.route === route;
  const [method, pattern] = route.split(' ');
  const path = new RegExp(`^${pattern.replace(/[.*+?^$()|[\]\\]/g, '\\$&').replace(/\\?\{[^}]+\}/g, '[^/]+')}$`);
  return call.method === method && path.test(decodeURIComponent(call.path.split('?')[0]));
}

const unescape = value => value.replaceAll('&quot;', '"').replaceAll('&#39;', "'").replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&amp;', '&');

/**
 * Starts an emulator with `seed` merged into its empty world. It exits on
 * its own after `idleTimeout` seconds without a request, should a driver
 * forget to stop it.
 */
export async function startOctoemu(seed, { idleTimeout = 900, binary = octoemuBinary() } = {}) {
  assert.ok(existsSync(binary), `octoemu binary missing at ${binary}: run node tests/worker/build-octoemu.mjs`);
  const child = spawn(binary, ['--port', '0', '--idle-timeout', String(idleTimeout)], { stdio: ['ignore', 'pipe', 'pipe'] });
  trackFixtureOutput(child, 'octoemu');
  const ready = await new Promise((resolve, reject) => {
    let stdout = '', stderr = '';
    const timer = setTimeout(() => { child.kill('SIGKILL'); reject(new Error(`octoemu startup timeout: ${stderr}`)); }, 15_000);
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('exit', code => { clearTimeout(timer); reject(new Error(`octoemu exited ${code}: ${stderr}`)); });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.stdout.on('data', chunk => {
      stdout += chunk;
      const newline = stdout.indexOf('\n');
      if (newline < 0) return;
      clearTimeout(timer);
      try { resolve(JSON.parse(stdout.slice(0, newline))); } catch (error) { reject(error); }
    });
  });
  child.removeAllListeners('exit');
  const emulator = new Octoemu(child, ready);
  try { if (seed) await emulator.seed(seed); }
  catch (error) { await emulator.stop(); throw error; }
  return emulator;
}

/**
 * The GitHub credentials octoemu mints (`ghu_`/`ghp_` + 36, `ghr_` + 76,
 * `ghs_<APPID>_<JWT>`), standing alone (not inside a longer base64 run, so
 * random encoded bytes do not match): the emulator's tokens are random and
 * never seen by the drivers, so their privacy scans look for the shape
 * instead of a fixture marker.
 */
export const GITHUB_TOKEN = /(?<![A-Za-z0-9+/_=-])(?:gh[up]_[A-Za-z0-9]{36}|ghr_[A-Za-z0-9]{76}|ghs_\d+_[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+)(?![A-Za-z0-9])/;
