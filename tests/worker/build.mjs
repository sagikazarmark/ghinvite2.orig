import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync, mkdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

const root = fileURLToPath(new URL('../../', import.meta.url));
const here = fileURLToPath(new URL('./', import.meta.url));
const cancellationFixture = process.argv[2] === 'cancellation-fixture';
const workflows = process.argv[2] === 'workflows' || cancellationFixture;
const secretFixture = process.argv[2] === 'link-secret-fixture';
assert.ok(!process.argv[2] || workflows || secretFixture, 'Usage: node build.mjs [workflows|link-secret-fixture|cancellation-fixture]');
const crate = workflows ? 'ghinvite-workflows-worker' : 'ghinvite-web-worker';
const directory = cancellationFixture ? '.generated-cancellation' : workflows ? '.generated-workflows' : secretFixture ? '.generated-link-secret' : '.generated';
const cli = process.env.WASM_BINDGEN || 'wasm-bindgen';
const lock = readFileSync(join(root, 'Cargo.lock'), 'utf8');
const version = lock.match(/name = "wasm-bindgen"\r?\nversion = "([^"]+)"/)?.[1];
assert.ok(version, 'wasm-bindgen version missing from Cargo.lock');
const actual = execFileSync(cli, ['--version'], { encoding: 'utf8' }).trim();
assert.equal(actual, `wasm-bindgen ${version}`, 'Install the wasm-bindgen CLI matching Cargo.lock');

execFileSync('timeout', ['--kill-after=5s', '600s', 'cargo', 'build', '--locked', '-p', crate, '--target', 'wasm32-unknown-unknown', '--features', cancellationFixture ? 'cancellation-fixture' : secretFixture ? 'link-secret-fixture' : 'runtime-tests'], {
  cwd: root, stdio: 'inherit', timeout: 615_000, killSignal: 'SIGKILL',
});
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--offline', '--no-deps', '--format-version', '1'], {
  cwd: root, encoding: 'utf8',
}));
const generated = join(here, directory);
rmSync(generated, { recursive: true, force: true });
mkdirSync(generated);
execFileSync(cli, [
  join(metadata.target_directory, `wasm32-unknown-unknown/debug/${crate.replaceAll('-', '_')}.wasm`),
  '--target', 'web', '--no-typescript', '--out-dir', generated, '--out-name', 'index',
], { stdio: 'inherit', timeout: 120_000, killSignal: 'SIGKILL' });
// The web target's initSync accepts workerd's precompiled WebAssembly.Module.
// esbuild follows generated JS snippets, so their hashed paths aren't hard-coded.
await build({
  absWorkingDir: here,
  ...(cancellationFixture ? { stdin: {
    contents: readFileSync(join(here, 'workflows-shim.mjs'), 'utf8').replaceAll('.generated-workflows', directory),
    resolveDir: here, sourcefile: 'cancellation-shim.mjs', loader: 'js',
  } } : {entryPoints: [workflows ? 'workflows-shim.mjs' : secretFixture ? 'link-secret-shim.mjs' : 'shim.mjs']}),
  outfile: cancellationFixture ? 'cancellation-worker.mjs' : workflows ? 'workflows-worker.mjs' : secretFixture ? 'link-secret-worker.mjs' : 'worker.mjs',
  bundle: true,
  format: 'esm',
  external: [`./${directory}/index_bg.wasm`, 'cloudflare:workers', 'cloudflare:sockets', 'node:*'],
});
console.log(`Built actual ${crate} with wasm-bindgen ${version}`);
