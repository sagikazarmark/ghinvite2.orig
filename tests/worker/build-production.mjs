import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const manifest = new URL('./.production.json', import.meta.url);
rmSync(manifest, { force: true });
const bindgen = process.env.WASM_BINDGEN || 'wasm-bindgen';
for (const variable of ['CUSTOM_SHIM', 'ESBUILD_BIN', 'WASM_OPT_BIN', 'NO_MINIFY', 'COREDUMP', 'RUN_TO_COMPLETION']) {
  assert.equal(process.env[variable], undefined, `${variable} would change production packaging`);
}
const version = readFileSync(resolve(root, 'Cargo.lock'), 'utf8').match(/name = "wasm-bindgen"\r?\nversion = "([^"]+)"/)?.[1];
assert.equal(execFileSync(bindgen, ['--version'], { encoding: 'utf8' }).trim(), `wasm-bindgen ${version}`);
assert.equal(execFileSync('worker-build', ['--version'], { encoding: 'utf8' }).trim(), '0.8.1', 'Install worker-build 0.8.1');
const configurations = [];
for (const config of ['web.toml', 'restate-svc.toml']) {
  const data = JSON.parse(execFileSync('python3', ['-c',
    'import json, sys, tomllib; print(json.dumps(tomllib.load(open(sys.argv[1], "rb"))))', resolve(root, 'wrangler', config),
  ], { encoding: 'utf8', timeout: 10_000 }));
  assert.equal(data.compatibility_date, '2024-09-23');
  assert.deepEqual(data.compatibility_flags, ['nodejs_compat']);
  assert.ok(!data.build.command.includes('runtime-tests'), 'Deployment must not enable fixture features');
  // The exact deployment build command, production features and worker-build's
  // generated shim. No custom runtime-test packaging or JS fault wrappers.
  execFileSync('timeout', ['--kill-after=5s', '900s', 'bash', '-eu', '-c', data.build.command], {
    cwd: resolve(root, 'wrangler'), stdio: 'inherit', timeout: 915_000, killSignal: 'SIGKILL',
    env: { ...process.env, WASM_BINDGEN_BIN: bindgen },
  });
  configurations.push({ name: data.name, scriptPath: resolve(root, 'wrangler', data.main) });
}
writeFileSync(manifest, JSON.stringify(configurations));
