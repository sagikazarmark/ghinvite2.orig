# Authenticated Restate ingress: remote binding gate (#61)

#77 supplies the web client capability. This procedure is **operator-owned and
not yet executed against remote production bindings**. Local mock HTTP tests and
Wasm typechecking do not establish Cloud routing, signatures or deployed secrets.
The other [Worker/D1 rollout gates](worker-admission-gate.md#remaining-rollout-gates--fault-model-limits)
remain under #61.

## Prepare a disposable remote deployment

1. Deploy production-feature web/workflow artifacts to disposable remote Workers,
   D1 and KV with a dedicated Restate Cloud environment. Record commit/artifact
   versions, deployment IDs, ingress host, binding names and test fixture IDs.
2. Follow [credential provisioning](deploy.md#6-set-secrets). Set web
   `GHINVITE_RESTATE_AUTH=bearer`, `GHINVITE_RESTATE_API_KEY` as a Worker secret,
   and `GHINVITE_RESTATE_INGRESS` to the environment's HTTPS ingress. Configure
   `RESTATE_IDENTITY_KEY` separately on the workflow Worker and register it.
3. Verify deployment discovery and signed Restate → workflow requests independently;
   an unsigned direct request to the workflow endpoint must be rejected.
4. Use a disposable GitHub App/account/repository and test users. Use the cutover
   runbook before enabling authoritative admission; this gate is not permission
   to change a live production account's admission mode.

## Verify the actual web bindings

Exercise the normal browser/GitHub routes, rather than substituting an operator's
direct authenticated curl request for the Worker's own secret binding:

| Path | Required evidence with the valid key |
|---|---|
| GitHub installation setup/update return | Setup completes through `Installation` ordinary ingress call; account/repositories become available |
| Invitation link creation / requester page | Authoritative mutation and read complete; rendered state matches the accepted result |
| Invitation request submission / status / admin decision | Admission/decision succeeds and status can be read; original-operation replay preserves the result |
| Signed repository-selection webhook | Web send is accepted, then the corresponding runtime invocation completes and repository selection converges |

Repeat against the native web binary pointed at the same protected disposable
ingress, with the API key injected into its process environment and appropriate
disposable native storage/OAuth settings. This checks native credential loading as
well as Worker bindings; native hosting remains development/testing scoped.

## Negative checks and rotation

1. In an isolated copy of the web deployment, remove the ingress secret. Requests
   must fail configuration, with no fallback to unauthenticated ingress. Repeat
   with an empty or malformed key and verify sanitized errors.
2. Install a syntactically valid but revoked/wrong-environment key. Exercise each
   table row: ordinary and authoritative operations fail without success redirects
   or fabricated state, signed webhook sends fail instead of acknowledging success.
   Verify 401/403 rejection at ingress and no new accepted invocation for these
   probes. A prior accepted invocation may continue running.
3. From a controlled operator client, probe ingress with no key and an invalid key
   on call and send URLs. Both must return 401/403. This verifies the gateway is
   actually protected even when the application is misconfigured. Never put keys
   in command arguments, URLs, shell tracing, or verbose HTTP dumps; inject headers
   from the operator's secret manager and retain only status/metadata.
4. Restore the valid binding and recover uncertain browser operations using the
   same operation ID. Verify success and no duplicate invitation request/use.
5. Follow [rotation](deploy.md#restate-ingress-api-key): replacement succeeds on
   all serving versions, revoked old key is rejected, replacement still succeeds.

Inspect HTML, serialized island props, response headers, downloaded JS/Wasm,
browser network traffic and application logs for the synthetic test key and
Authorization value. Neither may appear. Inspect failure paths too; do not store
unredacted HAR/log files or paste keys in issue evidence. Disable request-header
capture in any operator-managed gateway/APM logging.

Publish date, versions, binding **names** (never values), fixture/operation IDs,
observed HTTP statuses, invocation completion and redacted browser/log results to
#61. Record each unrun/failed row and an explicit affirmative or blocked verdict;
a passing `/health`, build or local mock suite alone is not an affirmative gate.
