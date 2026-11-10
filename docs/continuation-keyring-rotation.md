# Verified continuation keyring rotation

This control implements the D1/SQLite continuation row gate in keyring §7.2 and
P1 build-plan §7. It does not retire a key, deploy a descriptor, rotate credentials,
or replace the independent KV session TTL gate and immutable-journal gate.

## Preconditions and authority

Apply additive migration0028 through the database's existing migration mechanism.
It preserves every continuation row and adds one mutation epoch plus three triggers.
Do not reset that epoch, disable its triggers, or restore a database while a pass
is active. Any such administrative action requires a fresh pass afterward.

Follow Add and Promote with the keyring CLI and private descriptor files. Keep the
old generation readable. Establish all three KR-18a checks for every Worker: one newly created active
version at 100% with no gradual rollout, 20 consecutive matching diagnostic
fingerprints, then ten minutes without an older fingerprint in keyring load/error
logs. Complete this gate before sweeping. Old serving deployments
must no longer write under the old current generation. A completed scan cannot
promise anything about a future old-generation writer. No gradual deployment or
rollback across Promote is authorized by this operation.

Only a currently authenticated numeric user in `P1_OPERATOR_IDS` can access
`/console/keyring/continuations`; account administration is not sufficient. Every
POST checks the protected session, current operator allowlist and synchronizer
CSRF token. Responses are private/no-store. The endpoint never sends plaintext,
ciphertext rows, stored scope/binding, or database diagnostics to the browser.

## Execute the two passes

Use the same-origin operator session in a private browser. GET the route to obtain
its JSON control descriptor and CSRF token. POST URL-encoded bodies to the same
route, with `csrf_token`, `phase=rewrap`, and, on subsequent pages, the exact `next`
value as `progress`. Keep CSRF and progress in the body, never in URLs or logs.
The authenticated browser console can use the following bounded driver; do not
paste session cookies or keyring descriptors into tools or command arguments:

```javascript
async function continuationPass(phase) {
  const path = '/console/keyring/continuations';
  const start = await fetch(path, {credentials:'same-origin', cache:'no-store'});
  if (!start.ok) throw new Error('Operator controls unavailable');
  const control = await start.json();
  let progress;
  for (let pages=0; pages<10000; pages++) {
    const body = new URLSearchParams({csrf_token:control.csrf_token, phase});
    if (progress) body.set('progress',progress);
    const response = await fetch(path, {method:'POST', credentials:'same-origin',
      cache:'no-store', body});
    if (!response.ok) throw new Error('Pass stopped; no completion established');
    const report = await response.json();
    // Numeric row IDs and fixed error categories only. Never print `next`.
    if (report.issues.length) console.warn(report.issues);
    if (report.phase_finished) {
      const {next, ...result} = report;
      return result;
    }
    progress = report.next;
  }
  throw new Error('Page cap reached; no completion established');
}
```

First await `continuationPass('rewrap')`. Its `complete` is always false, even
when every old row was successfully rewritten. Then independently await
`continuationPass('verify')`. A completed gate requires **all** of:
`phase='verify'`, `phase_finished=true`, `complete=true`, `counts.old=0`,
`counts.bad=0`, the intended current version and rollout fingerprint. Record this
non-secret result with its observed database revision and timestamp.

Each page visits at most four rows with one lookahead. Every retained row is
included, even if expired or unreachable through ordinary browser recovery.
Metadata/ciphertext size limits produce a bad-row result rather than allocating
unbounded input. Oversized or malformed database identity integers fail the pass.
The application verifies the entire envelope and stored purpose/context before
rewrapping; the CAS compares old ciphertext, row identity, context, expiry and
mutation epoch. A successful update must return exactly the intended rowid;
D1 total-change metadata also counts the epoch trigger and is not a CAS receipt.
It changes only the encrypted payload's wrapping envelope.

A concurrent mutation, including expiry cleanup or insertion behind the cursor,
causes409 and requires a fresh pass. A lost write acknowledgement causes503;
start a fresh pass to observe any committed rewrap. Already committed rewraps
remain safe. No operation identity or unknown-ack retry can overwrite newer
bytes. Progress is sealed to the numeric operator, phase and exact keyring
fingerprint, expires one hour from the first page, and never extends itself.
A busy deployment may require a controlled continuation-write quiet period to
finish; this control never silently disables traffic or ignores epoch changes.

Bad rows are identified only by database rowid and a fixed category. They are
left byte-for-byte intact. Unknown generations may mean a lagging deployment;
restore the correct readable descriptor before retrying. Investigate corrupt,
copied-context, removed-key and oversized rows through the established private
incident process. Deletion requires a separate explicit retention/authority
judgment; this tool neither deletes nor treats TTL expiry as successful rotation.
After approved repair/deletion, start a new full Verify pass.

Before Remove, independently satisfy the session KV TTL and any other consumer
rotation gates. Recheck current deployment fingerprints and absence of old
writers. The zero-old/zero-bad result is a point-in-time table proof, not a key
removal authorization or evidence that Cloudflare propagation has been measured.

## Local verification

After applying the staged change, execute the real native `continuation_rotation`
web test target and shared storage suite, then `npm run test:continuation-rotation
--prefix tests/worker` against a freshly built runtime-tests Worker. The latter
uses actual D1, complete raw log finalization and a full-table plaintext scan.
Run the production packaging gate as well: its fixture-route inventory must
continue refusing this test-only route. No compile or runtime success is implied
by the presence of these tests.
