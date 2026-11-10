// Build ghinvite-github's fake-GitHub fixture (the `stub` example) in this
// workspace's target directory. admission.mjs obtains this same directory from
// P1 Cargo metadata.
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const app = fileURLToPath(new URL('../..', import.meta.url));
execFileSync('cargo', ['build', '--locked', '-p', 'ghinvite-github', '--features', 'test-stub', '--example', 'stub'], {
  cwd: app, stdio: 'inherit',
});
