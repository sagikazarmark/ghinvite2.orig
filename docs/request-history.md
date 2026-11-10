# Console request history

From an invitation-link detail page, **Request history** opens
`/console/accounts/{login}/links/{link_id}/requests`. Each page contains at most
25 requests, newest admission first with request ID as the deterministic tie
breaker. **Older requests** uses an exclusive, link-bound keyset cursor;
**Latest requests** restarts browsing. Invalid cursors restart at the latest page.
New projections can arrive between page reads, so this is not a snapshot or a
claim that the history is complete.

`/console/accounts/{login}/requests/{request_id}` is the stable Console detail
route for pending and terminal requests. It uses the existing current,
immutable-GitHub-ID account-admin authorization. Foreign resources and requests
whose owning link cannot be established are concealed. Both pages are private,
non-cacheable reads. Requester profiles are optional enrichment; immutable user
IDs remain visible when a profile cannot be loaded.

History loads requester logins and optional cached GitHub profile facts in its single bounded SQL statement. Its index
uses a fixed-width normalized-time/ID key, so deep pages seek past even large
equal-timestamp groups. A database-work regression checks a 10,000-row group;
query-count coverage checks that profile enrichment adds no round trips.

Details show projected admission/deadline/decision facts, admin-only justification
and decline reason, and the owning link's immutable repository scope and
permission. Delivery receipts and later GitHub invitation lifecycle records are
shown per repository. An absent outcome means that dispatch or projection may be
delayed, not that delivery failed. A failed delivery read is explicitly unavailable;
any retained facts from another successful read remain visible. Current delivery generations are checked against retained request progress;
historical receipts remain labeled separately from current delivery status.

## Effective access preview

Every request detail shows a row for each immutable target and a **Preview
effective access** action (also linked from the queue). A plain detail or queue
read does not fetch preview evidence. The explicit detail query `?preview=1`
first passes the same account and complete-target authorization as the detail.
It resolves the requester's stored login against the immutable numeric user ID,
verifies repository identity and owning account, organization identity, and team
identity within that organization, then reads effective access using installation
credentials. No requester, target or account supplied in a query can replace the
retained request's identities.

The shared bulk-review reader caches evidence for less than five minutes, with
app/installation and numeric user/target role-query partitions. Identity reads
are cached separately and revalidated. Future or expired evidence is rejected.
Every uncached resource read charges `AUTHZ_RL` and shares the request-local
50-read ceiling with Console authorization. The queue never performs a per-row
fanout. Missing installations, upstream refusal, identity mismatch, invalid
cached evidence and exhausted read budgets leave explicit unavailable cells.

Read, triage, write, maintain and admin repository roles, custom roles, and active
or pending organization/team membership remain distinct. The oldest successful
identity/role observation time is displayed. Preview results are informational:
they neither authorize approval nor alter requested grants, decision receipts,
budget reservations or delivery behavior. Delivery still rereads before writing.
Existing queue decision controls and historical admin-only decline reasons are
unchanged. The preview works through ordinary links with JavaScript disabled.

Verification:

```sh
cargo test -p ghinvite-storage-sqlx --test sqlx_suite
cargo test -p ghinvite-storage-sqlx request_history_seeks_index
cargo test -p ghinvite-web --test console_flow request_history::
cargo test -p ghinvite-web --lib authority::tests::request_preview
cargo test -p ghinvite-web --test console_flow approval_preview::
cargo test -p ghinvite-ui --lib request_preview::
# From tests/worker (requires matching wasm-bindgen CLI):
npm run test:history
# From tests/browser:
npx playwright test --config request-history.config.mjs
```

The approval-preview slice passes five native service cases, three inherited bulk
review cases, three real-router HTTP cases, two preview UI cases and two inherited
queue UI cases. The HTTP cases prove no preview reads on ordinary views, cache
reuse, wrong-resource and partial-delegation concealment before requester reads,
and bounded unavailable/error rendering. Seven request-history regressions also
pass (one browser-server fixture is intentionally ignored). Logs:
`/tmp/approval-preview-{service,bulk-regression,http2,ui,queue-ui,history-regression}.log`.
Native web/UI/Console strict Clippy passes `/tmp/approval-preview-clippy2.log`.
This evidence does not claim a live GitHub or production browser deployment.

## Pending navigation count

Every authorized Console document supplies desktop and mobile navigation with
the scoped projected count of pending requests. The count uses the same complete
manifest predicate as the queue; a repository administrator cannot infer hidden
multi-target requests from a broader account count. The current account name
binds the render context, so another account's count is ignored.

If the scope is partial or the count read fails, the badge says its count is
unavailable. Delegated search-first discovery also displays an unavailable count
without spending repository permission reads on navigation. Zero is displayed
only after a successful complete-scope count. Counts are display facts and do
not authorize decisions or admission.

## Requester profile display

The approval queue, request detail and history display an avatar and GitHub
account age when available (§6.4.2). Sign-in's existing authenticated `/user`
read supplies optional `avatar_url` and `created_at` fields, as documented in
[GitHub's authenticated-user response](https://docs.github.com/en/rest/users/users?apiVersion=2022-11-28#get-the-authenticated-user).
A malformed, missing or future creation time displays as unavailable; local
sign-in or request admission time is never substituted. The numeric GitHub user
ID remains the identity. These facts confer no authorization and are not risk
scores. They are cached in `users`; migration 0027 adds a nullable timestamp
without backfilling historical rows. A partial legacy cache update preserves
previously observed metadata. Session identity and retained workflow receipts
are unchanged.

New sign-ins normalize avatar metadata to a numeric GitHub CDN URL, discarding
arbitrary upstream URL contents. Browser HTML contains only the same-origin
`/console/accounts/{account}/requests/{request}/avatar` route. That route proves
current account and complete-target visibility, binds the cached user to the
request's numeric requester, and derives the CDN URL from that ID. It makes one
public GET with no token, cookie or referrer and a five-second deadline. The
native and Worker transports prohibit redirects and cap streamed response bodies
at 10 MiB; the avatar helper accepts at most 256 KiB and only matching PNG, JPEG or
WebP MIME types and magic bytes. SVG, HTML, redirects and malformed responses
produce a missing image. Responses are private/no-store, nosniff and no-referrer.
The existing `img-src 'self' data:` CSP is unchanged. Neutral initials remain
under the optional image, and missing profile data does not prevent decisions.

Page rendering adds no GitHub profile calls or per-row database queries: the
queue/history reuse bounded user joins and detail reads one cached user after
authorization. Images are lazy and require no JavaScript.

Profile verification (2026-10-17): full toolkit290, UI155, web165 and Console123
native tests pass (three Console fixture servers are intentionally ignored).
Shared SQLite52 and actual D152 pass, including a raw invalid creation-clock
update rollback and legacy omission in migration0027. Desktop and mobile
Chromium without JavaScript both load the same-origin avatar and show the real
GitHub creation date; final CSS screenshots were inspected. Logs:
`/tmp/requester-profile-{gh-toolkit,ghinvite-ui,ghinvite-web,console_flow}-runtime.log`,
`/tmp/requester-profile-sqlx_suite-runtime.log`,
`/tmp/requester-profile-d152.log`, and
`/tmp/requester-profile-browser-final.log`. The actual D1 run uses the fresh
workflow artifact from `/tmp/requester-profile-workflow-capture2.log`.
The existing browser history gate and shared storage gate include these checks;
no separate live GitHub avatar acceptance is claimed.
