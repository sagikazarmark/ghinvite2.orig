# /autoplan review — Plan 1 Foundations

**Date:** 2026-05-04
**Target:** `docs/superpowers/specs/2026-05-04-ghinvite-v1-design.md` (CEO phase) and `docs/superpowers/plans/2026-05-04-ghinvite-foundations.md` (Eng + DX phases)
**Mode:** /autoplan in degraded single-voice mode — Codex CLI was unavailable in the environment, so each phase ran with one Claude subagent rather than the usual Claude+Codex consensus pair. All phases tagged `[subagent-only]` in the degradation matrix.
**Outcome:** All 3 user challenges rejected (user direction holds); 4 taste recommendations accepted; 15 auto-decided fixes applied to Plan 1 (amendments section A1–A15) and the spec (60s admin cache, recipient form sign-in display, link-form defaults, "stop accepting new requests" UI label).

This file preserves the full review-phase outputs as the historical record. Plan 1's audit trail captures the decisions at one-line granularity; this file holds the original arguments.

---

## Phase 1 — CEO (strategic) review

**Voice:** Claude subagent. **Verdict:** NEEDS-REVISION.

### 1. Premise challenge

**P1: "Shareable links are the right affordance vs. JIT or IdP-anchored."** Stated in §1, differentiation. Confidence: medium-low (~50%). The spec asserts this but doesn't validate it. The premise hinges on a buyer who (a) doesn't have an IdP for the recipient, (b) cares enough about audit/expiry to install a third-party App, and (c) won't just DM a PAT or click the GitHub UI's "Add people" button twice a quarter. That's a thin slice. Org owners with real compliance needs already have Okta + SCIM + GitHub Enterprise. Solo devs and tiny teams have GitHub UI + Slack DMs. The "shareable link wherever people communicate" framing sounds like a feature, not a buying trigger. The fact that probot/invite died suggests the ICP is harder to find than this spec admits.

**P2: "Restate is justified for this scope."** Implicit / hand-waved (§1, Q4). Confidence: low (~30%). The state machines here — pending → approved/declined/expired with a 7-day timer, plus webhook reconciliation — are textbook "boring database with a cron." Restate Cloud adds: a managed service dependency, an unproven wasm SDK story (§20 admits this), a second deploy target, a second binary, secret-scoping rationale that exists *because* of the second binary. The whole "exactly-once-effects on outbound GitHub calls" is solved by 4 lines of code: insert idempotency-key row, call API, mark sent. Restate is the spec's most architecturally load-bearing decision and it is justified by aesthetic preference, not requirement.

**P3: "Audit-log-as-system-of-record is a real differentiator."** Stated (§1). Confidence: medium (~55%). It's plausible — GitHub's 90-day vs. SOC2's 12-month is a real gap. But (a) most buyers in this segment don't have SOC2 audits, (b) those that do already export GitHub audit logs to Splunk/Datadog, and (c) "we keep an audit log forever" isn't a moat — it's a row in a table. The differentiator collapses if a competitor adds the same 50 lines.

**P4: "GitHub Apps are the right credential model."** Stated (§14, §6). Confidence: high (~85%). This one is genuinely correct. PATs are scary, OAuth user tokens can't grant collaborator access cleanly across orgs, GitHub Apps with installation tokens are the textbook answer. No quibble.

**P5: "B+ multi-tenant SaaS is the right deployment model for v1."** Stated (§4, Q1). Confidence: medium (~50%). "Publicly hostable, no billing, no quotas" is a polite way of saying "free SaaS with operational liability and no revenue path." If this is a portfolio piece, fine. If it's a product, you need to know who pays and when before you build the multi-tenant onboarding flow that gets you sued under GDPR.

### 2. The 6-month regret scenario

**Severity: High.** The Restate dependency. In six months, you'll be debugging a wasm/Restate-SDK incompatibility (§20 already flags this as unverified), running a Fly.io VM you didn't want, paying for a Restate Cloud tier, and explaining to your two early users why their approval emails are 4 hours late because Restate had an incident. The replacement is embarrassingly small: a `pending_actions` table, a Cloudflare Cron Trigger that runs every 60s and processes due rows with `SELECT ... FOR UPDATE SKIP LOCKED`-equivalent semantics (D1 doesn't have it; use a claim column with TTL), and idempotency keys on outbound GitHub calls. You'd ship faster, have one binary, and own the reliability story end-to-end.

### 3. Right problem to solve?

**Alternative A: A GitHub Action + repo-as-config (à la ministryofjustice/github-collaborators).** The buyer commits a `collaborators.yaml` to a config repo, the Action reconciles GitHub state to YAML on push and on cron. PRs against the YAML are the audit log, the approval workflow, *and* the change history. **Verdict: this is the correct product for orgs that already think in GitOps**, and it's what the SOC2-conscious buyer actually wants. ghinvite's audience is the *narrower* slice that doesn't want GitOps and doesn't have an IdP.

**Alternative B: A CLI (`ghinvite share --repo foo --perm push --expire 7d`).** Outputs a link, runs locally with the user's gh credentials, no SaaS, no Restate, no D1. **Verdict: kills 80% of the architecture and serves the same use case for the IC who's the actual user.** The "admin dashboard" is a `--list` flag. You lose the multi-tenant story, but you also lose the multi-tenant cost.

**Alternative C: A Slack bot.** `/invite-repo @alice foo,bar push 7d`. **Verdict: probably wrong** — assumes the recipient is in your Slack, which is exactly the assumption ghinvite claims to break.

**Alternative D: A GitHub App with no UI at all — pure webhook + comment-driven.** Drop a comment `/invite @alice push 7d` on an issue, App handles it. **Verdict: charming, but requires the inviter to already have repo write access, which collapses the use case.**

The CLI reframing (B) is the strongest challenger and is *not addressed anywhere in the spec*. **Severity: High.**

### 4. Alternatives dismissed too quickly

The GitOps Action approach **is not in the spec at all**. The spec mentions ministryofjustice/github-collaborators only by inference (via "prior-art research" gestured at in §1). For a meaningful slice of the buyer set — anyone with a platform team, anyone running Terraform, anyone with SOC2 — the YAML-in-a-repo + scheduled Action model is **strictly cheaper to build (no SaaS, no DB, no Restate, no auth), strictly cheaper to operate (zero ops), and strictly more auditable (git history is the audit log)**. The reason it's "not adequate" for ghinvite's ICP is that the ICP supposedly *can't* do GitOps — but the spec never names that ICP concretely enough to know whether they exist in numbers.

Also dismissed without serious treatment:
- **Single-user CLI** (above).
- **A pure Cloudflare Worker + D1 + Cron Trigger** stack with no Restate. The spec evaluates "Restate vs DB-only" but the DB-only side of that comparison is strawmanned — there's no engagement with `pending_actions` + cron + idempotency, which is the standard pattern.
- **Hosting on Fly.io with Postgres instead of CF + D1.** Removes the wasm constraint that's driving half the architectural pain.

**Severity: High.**

### 5. Competitive / market risks

**Why is probot/invite abandoned?** The spec assumes "the space is open." More likely: the maintainer learned the demand isn't there. probot/invite died because (a) GitHub's native invite UX got better, (b) the audience that needed it got Okta+SCIM, and (c) the audience that didn't need Okta also didn't need a tool — they DM'd a username. That's the most uncomfortable read and it's not engaged with.

**Moat?** None named in the spec. "Append-only audit table" is not a moat. "Restate workflows" is not a moat (it's a cost). "Shareable links" is a UX pattern any competitor copies in a sprint.

**Weekend-build defense?** A motivated platform team builds this internally in a weekend with: a Slack slash command, a GitHub App, and a Google Sheet for audit. They will. The product needs to be 10x better than the weekend build, and currently it's about 1.2x better with 50x the code.

**Severity: Critical** for the business case; **High** for the build.

### 6. Scope calibration

- **Audit UI deferred to v1.1.** Wrong call. The spec literally claims audit-log-as-system-of-record is a *core differentiator*. Shipping v1 with no way to view it means v1's differentiator is invisible. Move audit UI into v1. **Severity: High.**
- **Notifications deferred to v2.** Wrong call. "Admin approval required" with no notification means approvals sit for hours/days because the admin doesn't know. The product is *broken without email* the moment `approval_required=true` is set, which the spec implies will be the common case. Either ship email in v1 or remove approval-required from v1 entirely. **Severity: Critical.**
- **Auto-refresh of recipient pending page deferred to v1.1.** The recipient sees "manual refresh." This is a 2008 UX. **Severity: Medium** — fine if you ship in 6 weeks, terrible if you're benchmarking against modern tools.
- **Editing share links deferred to v2.** Defensible. Keep deferred.
- **Cascade revoke deferred to v2.** Means "revoke" doesn't actually revoke pending downstream invitations. The word "revoke" is misleading. **Severity: High** — either rename it to "stop accepting new requests" or implement cascade in v1.
- **Quorum / multi-approver to v2.** Defensible.

### 7. Strategic blind spots

1. **GDPR / data residency.** §4 says "publicly hostable, anyone can install." The moment a German org installs, you're a data processor. There's no DPA, no data-residency story, no deletion path (§19 lists "Soft-delete / GDPR redaction tooling" as v2). Hosting this publicly is a legal exposure the spec hasn't priced. **Severity: Critical.**

2. **Abuse vector: open invitation-link generation as a phishing platform.** A bad actor installs ghinvite on a throwaway org, generates a link to `microsoft/typescript`-look-alike repo names, sends to a target. Target sees a real ghinvite-hosted page with real GitHub OAuth and a real "you're being granted access to ..." flow. They sign in. They've now consented to your OAuth scope, and you've trained them on a phish UX that looks legitimate. The generic-404 design (§16) helps you, not the recipient. **Severity: High.**

3. **GitHub App rate limits on installation tokens.** 5000/hour/installation is fine for one org, but if you have 200 installations and one buggy reconciler, you'll see thundering-herd reconcile storms. The daily reconciler (§9.4) is described as "for each non-uninstalled installation, lists pending invitations" — that's an unbounded loop with no jitter, no rate budget, no per-install backoff. **Severity: High.**

4. **The "5-min admin recheck" cache (§5) is a security hole.** A demoted owner has 5 minutes to approve invitations after losing org-owner status. For a security-positioned product, that's the wrong default. Should be re-checked on *every* state-changing action with no cache, or you should drop the security framing. **Severity: High.**

5. **Cloudflare D1 limits and durability.** D1 has 10GB/database limits and is still labeled "in production" with caveats. Audit-log-as-system-of-record + indefinite retention + multi-tenant means D1 will be the first thing that breaks at any real scale. There's no migration story to a real Postgres. **Severity: Medium.**

6. **`session.user_id == installation.account_id` (§5) breaks on personal-account renames.** The spec says `account_id` is immutable. True. But the *session* ties to whatever GitHub returns at sign-in. If a user changes their GitHub login, the session's `user_id` is stable, fine — but every audit row joining `actor_id → users.login` is now misleading on display because `users.login` is mutable and you cache it. A renamed admin shows up as their old name in audit. **Severity: Medium.**

### Verdict

**NEEDS-REVISION.** The Restate decision, the deferred-notifications decision, the missing GDPR/abuse story, and the un-evaluated GitHub-Action-with-YAML alternative are individually fixable but collectively suggest the spec optimized for an interesting build rather than the right product. Cut Restate, cut the second binary, ship notifications in v1, ship audit UI in v1, name the ICP concretely, and decide whether this is a CLI before writing more wasm.

---

## Phase 2 — Design review

**Skipped.** Plan 1 is foundations — types, audit events, storage trait, sqlx impl, migration. No UI surface to evaluate. Will run in Plans 5 and 6 against the dashboard and recipient flow.

---

## Phase 3 — Eng review (Plan 1 specifically)

**Voice:** Claude subagent (with CEO summary as context). **Verdict:** PROCEED-WITH-FIXES.

### 1. Plan-internal consistency

**Trait ↔ impl signature match.** The `Storage` trait declared in Task 13 and the methods implemented across Tasks 16–21 line up cleanly. All 24 methods including parameter order and types verified: `insert_installation(&Account)`, `mark_installation_uninstalled(u64, DateTime<Utc>)`, `update_installation_repos(u64, &SelectedRepos)`, `record_request_decision(&RequestDecision)`, etc. The `unimplemented!` stubs in Task 16 reference the right future task numbers and all get filled in.

**Schema ↔ record types match.** The 8 tables in Task 12 each map 1:1 to a `*Row` in Task 14. Column counts and orders match. Subtle: `share_links` has 14 columns and the `INSERT` in Task 18 binds 14 placeholders; verified.

**Suite ↔ trait signature match.** `run_suite` (Task 22) calls every method via the `Storage: 'static + Send + Sync` bound and uses the same newtype wrappers (`NewShareLink`, `NewInvitationRequest`, `RequestDecision`, `GithubInvitationUpdate`). The signature `pub async fn run_suite<S: Storage>(s: S)` is fine.

**`sqlx::migrate!("../../migrations")` path.** Correct. The macro resolves relative to `CARGO_MANIFEST_DIR` of the *invoking* crate, which is `crates/storage/Cargo.toml`, so `../../migrations` lands at the workspace-root `migrations/` directory.

**Mismatches found:**
- Task 13 declares `pub mod tests;` unconditionally, then the test suite ships in production binaries. The bigger issue: `tests.rs` references `crate::Error` inside non-`#[cfg(test)]` code, which compiles fine but ships dead weight to consumers.
- Task 14 references `domain::permission::UnknownPermission` — works because `pub mod permission` is public, but fragile.

### 2. Code that won't compile or work as written

**Critical: `chrono::DateTime<Utc>` ↔ SQLite TEXT round-trip.** With `sqlx 0.8` + `features = ["chrono"]`, `DateTime<Utc>` implements `Type<Sqlite>` as `TEXT` (RFC3339 with `+00:00`). Round-trip works, lossless on nanoseconds. D1 same parser. *But*: the spec says timestamps are also written from JS in later plans — those will write ISO-8601 with `Z` suffix. sqlx 0.8 reads both `+00:00` and `Z`. Confirm in Plan 7.

**`&mut *tx` for `sqlx::Transaction`.** Correct in sqlx 0.8.

**`db.is_unique_violation()`.** This is a method on `&dyn DatabaseError`. In sqlx 0.8 it does exist and dispatches per backend. For SQLite it checks SQLite error codes 2067 (`SQLITE_CONSTRAINT_UNIQUE`) and 1555 (`SQLITE_CONSTRAINT_PRIMARYKEY`). Code path correct.

**`installation_id` u64 → i64 overflow.** GitHub installation IDs are documented as 64-bit but in practice well below 2³¹. SQLite's `INTEGER` is signed 64-bit. The plan does `account.installation_id as i64` — silent truncation if the value ever exceeds `i64::MAX` (astronomically unlikely). Same for `account_id`, `repo_id`, `github_invitation_id`, `user_id`. Fine in practice but technically lossy. A `try_into` debug assert in one helper would make the assumption explicit.

**`Result<T, sqlx::Error>` → `Result<T, Error>` via `#[from]`.** Works correctly through the derive.

**`#[tokio::test]` runtime setup.** Tokio workspace dep enables `["macros", "rt"]`. `#[tokio::test]` requires the `rt` feature; check ✓. Single-threaded `current_thread` runtime is sufficient.

**`async-trait` + `Send + Sync + 'static`.** Correct. Strict enough for `Arc<dyn Storage>` workflows.

### 3. Test design quality

**What's tested well:** insert/get round-trip per table, conflict on unique index, idempotent revoke (second call → `NotFound`), uses_count atomicity (Task 19), partial-unique-index per `(link, requester) WHERE state = 'pending'` (Task 19), audit per-account scoping (Task 21), full lifecycle in `run_suite`.

**Scenarios missing or weak:**

1. **Timestamp precision round-trip.** No test asserts microsecond/nanosecond fidelity. RFC3339 with sqlx-chrono *does* preserve nanoseconds, but nothing in the plan proves that — and D1's text column won't necessarily round-trip the same way.

2. **Foreign-key violations.** The plan enables `foreign_keys(true)` (Task 15) but never tests that, e.g., inserting a `share_link` with `installation_id = 999` fails. Without this test, a regression that turns FKs off goes undetected.

3. **Cascade-on-delete-of-user.** What happens if a user_id is referenced by `audit_events.actor_id` and the user row is later deleted? Schema has no `ON DELETE` policy on `audit_events` (and `actor_id` isn't even a `REFERENCES` constraint), so deletion would orphan. Intentional (audit must survive user deletion) but not asserted.

4. **Race on partial unique index.** No test attempts two concurrent `insert_invitation_request_and_increment_uses` for the same `(link, requester)`.

5. **`scenario_request_uses_and_uniqueness` semantic gap.** The plan's `insert_invitation_request_and_increment_uses` doesn't check `max_uses` (the comment says "caller has already verified is_active"). So if a caller skips the check, the DB will silently let `uses_count` exceed `max_uses`. The atomicity claim ("uses_count consistent with requests") is true, but the *exhaustion* invariant is not enforced at the DB level. Worth a Medium finding.

6. **Suite state leakage.** The `run_suite` function does NOT reset between scenarios — it relies on disjoint `installation_id`/`account_id`/`user_id` numbers (1001, 2001, … 5001). This works but is fragile: any future scenario added with overlapping IDs will silently corrupt.

### 4. Storage trait shape

**Method grain.** Mostly right. Two concerns:

- `get_share_link_by_id` and `get_share_link_by_slug` both call `list_repos_for_link` after fetching the row → 2 round trips per link. `list_share_links_for_account` is `1 + N` round-trips. **Worth fixing now**, since changing this later requires re-doing the whole storage crate's query layer.

- `insert_invitation_request_and_increment_uses` is the right grain — one transaction, one method. Good.

**Newtype wrappers.** `NewShareLink { link: ShareLink }` and `NewInvitationRequest { request: InvitationRequest }` are essentially noise — they wrap exactly one field of an already-public type. They add ceremony at every call site without conveying additional invariants. `RequestDecision` and `GithubInvitationUpdate` *do* earn their keep: they group fields the impl writes atomically and exclude fields the caller shouldn't touch.

**`audit(&AuditEvent)` write-only-by-design.** Yes — there is no `update_audit`, no `delete_audit`, and no `list_audit` on the trait. A caller cannot misuse it through the public API. *But*: Task 21 adds `debug_list_audit` as `#[cfg(test)] pub async fn` on the inherent impl. This compiles only in test profile, so it can't be misused from production code.

**D1 portability of the trait shape.** The trait is implementable on D1, but with friction:
- D1's `worker::D1Database` does not expose true multi-statement transactions. Tasks 18 and 19 both rely on `pool.begin()` ... `tx.commit()`. On D1 you would need to rewrite as `D1Database::batch(&[stmt1, stmt2, ...])`, building all statements upfront.
- Bind-parameter syntax: D1 uses `?` (positional), sqlx accepts `?N`. The plan uses `?N` and will need `?` for D1.

### 5. SQLite/D1 portability

- **Partial unique indexes (`WHERE state = 'pending'`):** Supported by D1. ✓
- **`CHECK (account_type IN ('User','Organization'))`:** Supported by D1. ✓
- **`BOOLEAN` stored as `INTEGER` 0/1:** Reliable across both. ✓
- **`REFERENCES` constraints:** D1 supports `REFERENCES` syntactically, and as of 2024 enforces them when `PRAGMA foreign_keys = ON`. *Important*: the plan enables FKs in `SqliteConnectOptions::foreign_keys(true)`, but **D1 has FKs OFF by default**. This is a Plan 7 footgun the foundations plan doesn't note. Worth a comment in `migrations/README.md`.

### 6. TDD discipline

The plan calls itself "TDD-style" but Tasks 3–10 are not TDD. Each task writes the impl and the tests in the same file edit, then runs the tests once and they pass. The fail-then-pass cycle never happens. Fine for *type definitions* — but calling it "TDD-style" is inaccurate terminology.

### 7. Hidden complexity

1. **`ulid::Ulid::from_str` 5× per share link list.** `try_into_domain` parses the ID strings on every read. For a 100-link account dashboard this is 100 ulid parses + 100 sub-list ulid parses. Cheap but real.

2. **`selected_repos` JSON encoding.** `#[serde(untagged)] enum SelectedRepos { All, Subset(Vec<u64>) }` — but the `encode_selected_repos` helper writes `"all"` (literal string) or a JSON array. That is *not* what `serde_json::to_string` of the untagged enum would produce: serde would emit `"All"` (capitalized) for the unit variant or the array for the tuple variant. The encoder is hand-rolled and the serde derivation is unused for storage. Latent bug if anyone serializes `SelectedRepos` for an audit event.

3. **Partial unique index race.** `sqlx::pool` with `max_connections(1)` for in-memory tests serializes everything → no actual race tested. With file-backed SQLite (`max_connections(8)`), under WAL mode contention you can get `SQLITE_BUSY`, which `sqlx::Error::Database(db)` does NOT classify as a unique violation — it'd surface as a generic database error.

### 8. Overall plan quality

**Specificity (8/10).** A reasonable Rust engineer can implement this without ambiguity.

**Test coverage (7/10).** Happy-path coverage excellent. Edge cases missing: FK violations, timestamp precision, race conditions, concurrency.

**Reversibility (9/10).** If the CEO review forces dropping Restate for cron + `pending_actions`, this plan is **almost untouched**. Exactly *one* table needs adding, and the trait gains 3-4 methods. The domain types, IDs, slug, audit, and storage shape all stand. This is a strong design property.

### Findings

**Critical (block proceeding):** None.

**High (fix before merging Plan 1's PR):**
1. `SelectedRepos` serde derivation is broken vs. storage encoding.
2. N+1 query on share-link reads.
3. Tests don't verify foreign-key enforcement.
4. `run_suite` doesn't run on a fresh database per scenario.

**Medium (follow-up issues):**
1. `max_uses` invariant not enforced at DB level.
2. `NewShareLink`/`NewInvitationRequest` newtypes are noise.
3. `debug_list_audit` lives in the impl, not on the trait.
4. D1 vs. sqlx FK-default mismatch.
5. `tests` module is `pub`.
6. TDD framing is inaccurate.
7. Timestamp precision not asserted.
8. u64-as-i64 cast convention undocumented.

### Verdict

**PROCEED-WITH-FIXES.** The plan is implementable, internally consistent, and produces a foundation the next plans can build on. The four High findings are small surgical edits. The CEO's strategic concerns (Restate, notifications, audit UI) don't materially affect this plan — Plan 1 is structurally insulated from those decisions.

---

## Phase 3.5 — DX review

**Voice:** Claude subagent (with CEO + Eng summaries as context). **Verdict:** NEEDS-REVISION.

### Part A: Internal DX (the foundations crates as a library)

#### A1. Storage trait ergonomics — **5/10**

The trait surface is honest but uneven. Method-name guessability ranges from "obvious" (`get_user`, `upsert_user`, `audit`) to "you must read the source" (`insert_invitation_request_and_increment_uses`, `list_pending_github_invitations_for_installation`, `mark_share_link_revoked`). The 4-word-plus method names broadcast that the trait is doing more than a CRUD interface — that's correct, but they're hard to type and hard to grep. A future Plan 3 engineer writing a Restate handler will autocomplete `storage.get_share_link_…` and have to choose between `_by_id` and `_by_slug` — fine — but there is no `get_share_link_by_slug_active_only` so they'll forget to filter on `revoked_at IS NULL`/`expires_at` themselves. **High** — recipient lookups will routinely call `is_active` correctly only because that's the only obvious thing to do, but a half-asleep handler could skip it.

The wrapper types — `NewShareLink { link: ShareLink }`, `NewInvitationRequest { request: InvitationRequest }` — are pure noise. **Medium**: collapse to passing `&ShareLink` directly. `RequestDecision` and `GithubInvitationUpdate` are better — they actually reshape the data, not just box it.

The domain/storage boundary leaks: `ShareLink` carries `pub slug: String` instead of the typed `Slug` from `domain::slug`. So a handler reading a share link gets back `String` and has no compile-time guarantee it's a valid base62 slug — undermining the whole point of `Slug` having a `from_string` validator. **High**: change `ShareLink::slug` to `Slug`. The plan implicitly admits this with the comment "Slug stored as String here so the type can travel without rng-tied checks" — but `Slug::from_string` doesn't need an RNG.

The `Error` enum is **actionable for `NotFound` and `Conflict`**, but `Database(sqlx::Error)` and `Corrupt(String)` aren't. A handler catching `Conflict("share_link with that id or slug already exists")` has to string-match to know whether to retry with a fresh slug or surface a duplicate-id error. **Medium**: `Conflict` should be `Conflict { kind: ConflictKind }` with `DuplicateSlug`, `DuplicatePendingRequest`, `DuplicateId` variants.

#### A2. Domain enum DX — **6/10**

The serialization-convention drift is real: `AccountType` is `PascalCase`, `Permission` is `lowercase`, `InvitationState`/`RequestState`/`TargetKind` are `snake_case`, `ActorKind` is `lowercase`. The plan justifies each choice in passing — `AccountType` matches the GitHub API verbatim, `Permission` matches GitHub's lowercase, others are "internal." But a contributor adding a new event type or state will have to remember which convention applies. **Medium**: pick one for *internal* enums (`snake_case`) and document the rule prominently.

Parse-error types are a paper cut. Five distinct unit-error structs with identical shape. **Low**: introduce one `domain::ParseEnumError { enum_name: &'static str, value: String }`.

The bigger risk is forgetting to update the `EVENT_TYPES` const when a new event type appears — that's a `&[&str]` constant disconnected from the actual emit-sites. **Medium**: turn `EVENT_TYPES` into a Rust enum.

#### A3. Documentation completeness — **4/10**

This is the weakest area. Public types have docstrings only when they're load-bearing: `Slug` has a 1-line doc — does NOT explain why constant-time compare matters, why the alphabet is the GitHub-allowed set, what entropy it provides. `ShareLink::is_active` has zero docs. `SelectedRepos::All` vs `Subset` — no doc on what "all" means at the GitHub level. The `Storage` trait itself has a 3-line module doc; *no method has a docstring*. A future Plan 3 engineer opening rust-doc for `insert_invitation_request_and_increment_uses` sees a signature and nothing else. **Critical**: every `Storage` method needs a 1-paragraph doc explaining preconditions, error cases, and idempotency.

#### A4. Test discoverability — **5/10**

Three test homes with no clear convention: per-module unit tests, `crates/storage/src/tests.rs` parameterized scenarios, `crates/storage/tests/sqlx_suite.rs` per-impl wiring. The plan never tells a future contributor where a new test belongs. **High**: add a CONTRIBUTING note at the top of `tests.rs` and refactor `run_suite` to take a `make_storage: impl Fn() -> S` factory so each scenario gets a fresh DB.

#### A5. Migration / schema iteration DX — **4/10**

Plan 1 ships exactly one migration and a one-paragraph README. The README does NOT cover: how to add a column when CHECK constraints would force a rebuild; what `sqlx-migrate` does on D1 (it doesn't — D1 has its own migration table); whether `_sqlx_migrations` is actually the table D1's wrangler creates (it isn't — wrangler uses `d1_migrations`). **High**: this will bite the second migration.

### Part B: External DX (the deployed product per spec)

#### B1. Recipient onboarding TTHW — **6 steps, 60–90s happy path; broken on wrong-account**

URL click → `/i/:slug` (preview repos). 1 step. → "Sign in with GitHub" CTA → GitHub OAuth consent (~10s if logged in, ~30s if not). 2-3 steps. → Back to `/i/:slug/request` → optional justification → submit. 4-5 steps. → `/i/:slug/pending/:request_id` (manual refresh).

**Wrong-account failure mode is bad.** Spec §10.1 has no "you signed in as the wrong user" detection. If the URL was DM'd to `alice@gmail` but she's logged into GitHub as her work account `acme-alice`, the request gets filed for `acme-alice`. There's no "did you mean to use a different account?" interstitial. **High**: spec §10.1 should add an explicit "signed in as @<login>; not you? sign out" link on `/i/:slug/request`.

#### B2. Admin onboarding — **3-link create form is reasonable, defaults missing**

Inferring fields from the data model: repos (multi-select, guessable), permission (5 GitHub levels — **no default specified**, should default to `pull`), `approval_required` boolean, `expires_at` (null means never — **no suggested default**, should default to "30 days from now"), `max_uses` (same problem), `internal_note`. **Medium**: without sensible defaults, every admin creates an unlimited never-expiring `admin`-permission link the first time, exactly the misuse the product is positioned against.

#### B3. Error message quality — average 1/3

| Spec §16 error | Problem | Cause | Fix | Score |
|---|---|---|---|---|
| "App needs reinstallation" | yes | partial (token revoked) | no | **1/3** |
| "session expired, try again" | yes | yes | yes | **2/3** |
| "we couldn't process your action" | yes | no | no | **1/3** |
| Generic 404 (revoked/expired/exhausted link) | hides info from attacker but also from legit user | none | none | **0/3** for legit users |
| 401 on bad HMAC | yes | n/a | n/a | **3/3** for consumer |
| "already decided" toast | yes | implicit | implicit | **2/3** |

**High**: the spec should require "what to try, in what order, and where to find more info." Sample rewrite: "Your installation's GitHub token was revoked or expired. **Reinstall the App** from your account settings → Installed GitHub Apps → ghinvite. Need help? [docs link]"

#### B4. Manual-refresh recipient pending page — abandonable in 5–15 minutes

Recipient sits on `/i/:slug/pending/:request_id`. With **no notification**, the recipient has to refresh manually. Realistic abandonment: ~3-4 refreshes over 5-15 minutes, then close the tab. By 1 hour, abandonment is ~80%. By 7-day expiry, ~99%. **Critical** (already flagged by CEO).

#### B5. Audit log invisible in v1 — buyer hostile

Spec §1 calls "audit log as system of record" a "Differentiation from existing tooling." Spec §15.3 then defers the UI to v1.1. **Critical**. A SOC2-conscious admin installs ghinvite, asks "show me the log," can't. They will (a) open a support ticket, (b) sketch a CSV-export stopgap themselves, (c) churn. Roughly half do (c). The audit log being invisible turns the *core differentiator into vapor*. **Recommendation**: ship a read-only audit page in v1 — even paginated "last 100 events" is enough.

#### B6. Documentation surface — none ships

The plan creates `migrations/README.md` (engineer-facing). No admin README, no `/docs` route, no in-app help. An admin who clicks "Create link" with no idea what `triage` permission means has no in-app explanation. **High**: at minimum, link to GitHub docs from form labels.

### Part C: Cross-cutting

#### C1. 5-minute admin recheck cache — **bad DX, not just bad security**

CEO flagged the security angle. The DX angle: admin gets demoted at T=0. At T+3min the (now-non-)admin approves a request. At T+5min the cache expires. The admin doesn't know *why* their previous action retroactively failed; the requester doesn't know *why* their access was clawed back. **High**: shorten the cache to 60 seconds, OR re-fetch on every admin write.

#### C2. Three theme zones — aspirational without a design system

Spec §12 says "three theme zones" should look "different." There is no DESIGN.md, no DaisyUI theme names picked, no tokens. **Medium**: spec §12 should name the three DaisyUI theme keys.

#### C3. Justification field — no admin-side UX scoped

Recipient writes free-text justification. Admin reviews. Spec §11 lists `/accounts/:login/requests` as the queue route but doesn't say whether it's a list-with-expand, a one-at-a-time flow, or a card grid. **Medium**: spec needs UX density.

### DX Scorecard

| Dimension | Score |
|---|---|
| Internal Library DX | **5/10** |
| Recipient Onboarding | **4/10** (broken without notifications) |
| Admin Onboarding | **5/10** (no defaults) |
| Error Messages | **2/10** (problem-only) |
| Documentation | **3/10** |
| **Overall DX** | **4/10** |

**TTHW (recipient flow):** 60–90 seconds happy-path; 5–15 minutes if approval required and admin is online; abandonable past 1 hour if approval required and admin is offline.

### Verdict

**NEEDS-REVISION.** The foundations crates plan is implementable as written, but two cross-cutting issues — (1) approval-required flow without any notification mechanism, (2) audit log being the marketed differentiator with no v1 UI — combine to ship a product that doesn't deliver its own pitch. Internal DX is recoverable with the docstring/wrapper-type/Slug-typing fixes; external DX needs at minimum a read-only audit page and a notification stopgap before v1 is buyer-ready.

---

## Cross-phase themes

Independent voices converged on three themes:

**Theme 1: notifications + audit-UI deferral collapses the v1 product.** Flagged in CEO (Critical) and DX (Critical). Highest-confidence finding. → Surfaced as User Challenge 1 + 2.

**Theme 2: 5-minute admin recheck cache is wrong on multiple axes.** Flagged in CEO (security hole) and DX (worse-than-broken UX — retroactive failures). → Auto-fixed: shortened to 60s in spec §5 and §10.3.

**Theme 3: Spec's "audit-log differentiator" claim is inconsistent with its v1.1 deferral.** CEO + DX both flag the positioning gap. → Folded into User Challenge 2.

---

## Final disposition

User chose **option B with all 3 user challenges rejected**. Locks held:

- Restate stays (User Challenge 3 rejected).
- Audit UI defers to v1.1 (User Challenge 2 rejected).
- Notifications defer to v2 (User Challenge 1 rejected).

Taste decisions accepted (4) and auto-decided fixes applied (15). See Plan 1's "Post-/autoplan amendments" section for the concrete code changes and the audit-trail table at the foot of Plan 1 for one-line summaries.

The strategic findings above are preserved as historical record. They were considered and consciously declined; they may become relevant again if v1 ships and adoption surfaces the predicted abandonment rates from User Challenge 1 (recipient drop-off without notifications), or compliance objections from User Challenge 2 (audit-log invisibility), or operational pain from User Challenge 3 (Restate dependency).
