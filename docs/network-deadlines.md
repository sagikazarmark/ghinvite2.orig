# Upstream network deadlines

Production native and Worker callers apply the same per-HTTP-request policy:

| Client | Deadline | Completion boundary |
|---|---|---|
| GitHub `ReqwestTransport` | 30 seconds | Response headers and complete body |
| Restate `call` / `authoritative_call` | 15 seconds | Response headers and complete successful response body |
| Restate `send` | 15 seconds | Ingress acknowledgement headers; the unused body is dropped |

These are total request deadlines, not idle/read timeouts. Receiving headers or
partial body bytes does not restart the timer. All GitHub operations, including
OAuth, installation-token acquisition, reads, and writes, use the transport.
Native GitHub client injection (`with_client`) retains its caller-defined policy.
On Workers the transport always applies the production deadline.

The pinned reqwest 0.12.28 provides `RequestBuilder::timeout` on Wasm even though
`ClientBuilder::timeout` is native-only. Its Wasm abort guard starts a JavaScript
timer before Fetch and retains it through response body consumption. Expiry
aborts Fetch, allowing the Rust caller to return an error; dropping the response
cancels the fetch and clears the timer. Worker execution/CPU limits are not an
upstream wall-clock deadline. No timer shim or shortened policy is used in tests.

## Recovery contract

- A Restate timeout means **outcome unknown**. Aborting the caller's fetch does
  not cancel the durable invocation or prove it did not commit. Preserve the
  operation identity and canonical input. The existing browser recovery form and
  attempt URL retry/recover that same operation; a new ID is not an automatic retry.
- A timeout after a potentially issued GitHub write becomes **outcome unknown**.
  Preserve the input-bound SQL/D1 attempt fence. Further executions can reconcile
  using read-only identity evidence, but absent evidence never authorizes another
  PUT. Only a positively received definitive rejection can use the existing
  safe-retry rules in [ADR 0004](adr/0004-delivery-identity-and-uncertain-results.md).
- Account GitHub observations turn transport failure into a journaled `Unknown`
  observation, arrange projection and the existing 60-second durable recheck,
  and finish the exclusive handler. Fresh admission returns recoverable 503
  without a business rejection receipt; queued account status can then complete.
  Recovery retries the same admission attempt after availability returns.
- Durability, scheduling, and projection retry behavior remain owned by Restate.
  A network timeout does not impose a workflow deadline or replace durable retry.

Each request in pagination/token acquisition has its own budget. A multi-request
observation or browser route may therefore take more than one budget, and a
15-second web timeout can occur while a 30-second GitHub observation continues
durably. This policy bounds stalled network I/O, not total queueing time, page
count, database availability, or event-loop starvation.

## Verification

```sh
npm run test:deadlines --prefix tests/worker
npm run test:admission --prefix tests/worker
cargo test --locked -p ghinvite-github --test transport_deadlines
cargo test --locked -p ghinvite-web --test restate_deadlines
```

The transport gate compiles the actual web Worker and invokes public production
clients via feature-gated test routes. Miniflare upstreams leave headers or a
partially delivered body pending indefinitely; only the production Fetch deadline
can return the result. An outer watchdog fails the test rather than forwarding a
synthetic timeout. It covers GitHub and all three Restate invocation methods.

The admission gate runs real Restate, workflows/web Workers, and D1. Both header
and body stalls verify unknown delivery receipts, no second PUT after replay,
unknown account status, admission 503, queued exclusive status completion, and
same-operation recovery. Browser form tests let Restate commit admission before
stalling its acknowledgement, then verify the original operation/input survives
and recovery consumes only one use. Stalled responses bypass fixture-forwarder
timeouts entirely. Native TCP tests leave connections open indefinitely and
verify the existing default deadlines through the same public clients.
