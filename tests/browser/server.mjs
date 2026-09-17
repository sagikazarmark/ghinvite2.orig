import { execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const read = (path) => readFileSync(new URL(`../../${path}`, import.meta.url));

// Read the actual Rust policy, not a second policy that can silently drift.
const source = read('crates/ghinvite-web/src/middleware/csp.rs').toString();
const policy = source.match(/pub const CONTENT_SECURITY_POLICY: &str = "([^"]+)";/)?.[1]
  .replace(/\\\r?\n\s*/g, '');
if (!policy) throw new Error('Cannot read the production CSP constant');

// Preload only deployable assets. No arbitrary filesystem paths are served.
const assets = new Map([
  ['/static/styles.css', ['text/css', read('crates/ghinvite-web/assets/styles.built.css')]],
  ['/static/app.js', ['text/javascript', read('crates/ghinvite-web/assets/app.js')]],
]);
for (const name of readdirSync(new URL('../../dist/public/assets/', import.meta.url))) {
  if (!/^[\w.-]+\.(js|wasm)$/.test(name)) continue;
  assets.set(`/assets/${name}`, [
    name.endsWith('.wasm') ? 'application/wasm' : 'text/javascript',
    read(`dist/public/assets/${name}`),
  ]);
}
if (!assets.has('/assets/ghinvite-island.js')) {
  throw new Error('Run scripts/build-island.sh before browser tests');
}

const pages = new Map();
// Fixture authority travels through the real SSR context, props, and form.
const csrfToken = randomBytes(32).toString('base64url');
for (const [path, args] of [
  ['/', []], ['/failed', ['--with-errors']], ['/preserved', ['--preserved-values']],
  ['/permission-invalid', ['--permission-only-invalid']],
  ['/permission-empty', ['--permission-only-invalid', '--empty-permission']],
  ['/permission-unvalidated', ['--permission-only-invalid', '--unvalidated']],
  ['/max_uses-invalid', ['--numeric-only-invalid']],
  ['/expires_in_days-invalid', ['--numeric-only-invalid', '--expiration']],
  ['/rejection-mixed', ['--browser-rejection']],
  ['/rejection-max_uses', ['--browser-rejection', '--parse-blocked']],
  ['/rejection-expires_in_days', ['--browser-rejection', '--parse-blocked', '--expiration']],
  ['/rejection-form', ['--browser-rejection', '--form-only']],
  ['/multiline-description', ['--multiline-description']],
]) {
  pages.set(path, execFileSync('cargo', [
    'run', '--quiet', '-p', 'ghinvite-island', '--example', 'ssr_fixture', '--', ...args,
  ], { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'],
    env: { ...process.env, GHINVITE_FIXTURE_CSRF: csrfToken } }));
}

createServer(async (request, response) => {
  const path = new URL(request.url, 'http://127.0.0.1:4173').pathname;
  response.setHeader('Cache-Control', 'no-store');
  response.setHeader('X-Content-Type-Options', 'nosniff');
  if (request.method === 'POST' && path === '/console/accounts/acme/links') {
    // Echo repeated keys as ordered entries, preserving native checkbox rules.
    request.setEncoding('utf8');
    let body = '';
    for await (const chunk of request) {
      body += chunk;
      if (body.length > 64 * 1024) {
        response.writeHead(413).end();
        return;
      }
    }
    if (new URLSearchParams(body).getAll('csrf_token').join() !== csrfToken) {
      response.writeHead(403).end('Invalid CSRF token');
      return;
    }
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ entries: [...new URLSearchParams(body)] }));
  } else if (request.method === 'GET' && pages.has(path)) {
    response.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Content-Security-Policy': policy,
    });
    response.end(pages.get(path));
  } else if (request.method === 'GET' && assets.has(path)) {
    const [type, body] = assets.get(path);
    response.writeHead(200, { 'Content-Type': type });
    response.end(body);
  } else if (request.method === 'GET' && path === '/health') {
    response.writeHead(200).end('ready');
  } else {
    response.writeHead(404).end('Not found');
  }
}).listen(4173, '127.0.0.1', () => {
  console.log('Browser fixture: http://127.0.0.1:4173');
});
