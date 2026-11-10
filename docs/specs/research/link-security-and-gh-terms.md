# Desk check: invite-link secrecy (P1) and GitHub terms for automation (P1, P2)

Checked on 2026-10-15. Covers two `open` rows of `analysis/risks.md`: **R-P1-link-secrecy** and **R-ALL-gh-terms**. Inputs: `ideas/P1-github-invite.md`, `ideas/P2-dependaboard.md`, `analysis/poc-ghinvite.md` (cited as **PoC §n**), the PoC source at `/home/laborant/ghinvite2.orig` (read only), `research/github-invitations.md`, `research/github-app-tokens.md` and `research/dependaboard-prs.md`. Nothing was created or changed on GitHub.

The GitHub policy texts were read from the docs source in `github/docs` (`content/site-policy/...`, `main` branch, fetched 2026-10-15). They are published at the docs.github.com URLs cited below.

Confidence marks:
- **HIGH**: a primary source states it in so many words.
- **MEDIUM**: follows from primary sources, but the behaviour is implied rather than stated.
- **UNVERIFIED**: no primary source found. It is an inference or an opinion.

---

## 1. R-P1-link-secrecy: when are bearer invite links safe enough?

### 1.1 What P1 and the PoC do today

- **The design.** An admin creates a link with guardrails: max uses, expiry, permission, scope and an optional approval step (P1-F03/F04/F06/F19). The invitee opens it, signs in with GitHub, and requests access (P1-F36/F38). Nothing reaches GitHub until a request is admitted, and approved too when the link requires it.
- **The PoC's link code is the link ULID.** PoC §5 says "Invitation Code … It's now the link ULID". ADR 0009 says "Link ULID = public code = object key". The PoC creates it with `Ulid::new()` (`crates/ghinvite-core/src/ids.rs:16`).
- **Entropy of a ULID.** The ULID spec says "Timestamp 48bits, Randomness 80bits", with a "Cryptographically secure source of randomness, if possible" (https://github.com/ulid/spec). The timestamp part can be guessed by anyone who knows roughly when the link was made, so the secret part is **80 bits**. **HIGH** (spec). That the `ulid` crate's `Ulid::new()` uses a CSPRNG on wasm (getrandom JS backend) is **MEDIUM**: the PoC enables the getrandom JS backend (PoC §2), but the crate internals were not re-read.
- **The code is also an identifier.** It is the Restate object key, the D1 primary key, and it appears in console URLs and audit rows. So the bearer secret shows up in logs, the Restate UI and journal, and database dumps. It can't be hashed at rest, and it can't be rotated without making a new link. **HIGH** (PoC docs and code). This is the real gap, not guessability.
- **Headers and limits in the PoC.** A grep of the PoC for `Referrer-Policy`, `noindex`/`robots` and request rate limiting on the public link route found nothing. The PoC does set a CSP, `script-src 'self' 'wasm-unsafe-eval'` (PoC §2). **MEDIUM**: this was a grep, not a full read.

### 1.2 What the primary sources require

**Entropy and generation**
- RFC 6749 §10.10 (OAuth 2.0): "The probability of an attacker guessing generated tokens (and other credentials not intended for handling by end-users) MUST be less than or equal to 2^(-128) and SHOULD be less than or equal to 2^(-160)." https://www.rfc-editor.org/rfc/rfc6749#section-10.10
- OWASP Session Management: "Session identifiers must have at least 64 bits of entropy to prevent brute-force session guessing attacks." It also says that if part of the ID is predictable, the ID must be longer to make up for it. https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html
- OWASP Forgot Password: URL tokens should be "Generated using a cryptographically secure random number generator", be "Sufficiently long to protect against brute-force attacks", and be stored securely. It also says to "Implement appropriate protection to prevent users from brute-forcing tokens in the URL, such as rate limiting." https://cheatsheetseries.owasp.org/cheatsheets/Forgot_Password_Cheat_Sheet.html
- RFC 6750 §5.2: token references "MUST be infeasible for an attacker to guess". https://www.rfc-editor.org/rfc/rfc6750#section-5.2

**Storage at rest**
- RFC 6819 (OAuth threat model) §5.1.4.1.3: "The authorization server should not store credentials in clear text. Typical approaches are to store hashes instead or to encrypt credentials. If the credential lacks a reasonable entropy level (because it is a user password), an additional salt will harden the storage". Several threats list the mitigation "Store access token hashes only". https://www.rfc-editor.org/rfc/rfc6819#section-5.1.4.1.3
- So a plain SHA-256 of a token with 128 or more bits is enough. A salt or KDF is only needed for low-entropy secrets. **HIGH** (per RFC 6819's wording).

**Leakage through URLs (Referer, logs, history)**
- RFC 6750 §5.3: "Bearer tokens SHOULD NOT be passed in page URLs (for example, as query string parameters)". The reason given is that "Browsers, web servers, and other software may not adequately secure URLs in the browser history, web server logs, and other unsecured data structures". https://www.rfc-editor.org/rfc/rfc6750#section-5.3
- W3C TAG, *Good Practices for Capability URLs*. This is a First Public Working Draft from 18 Feb 2014 that "is not expected to become a Recommendation", so treat it as guidance.
  - It says "the risk of exposure of capability URLs is very high", through URL bars, logs, history, the `Referer` header, third-party scripts and URL shorteners.
  - Its recommendations are: "Capability URLs should be `https` URLs"; "Capability URLs should expire"; the owner should be able to "revoke the capability URLs"; pages reached by a capability URL "should not include links to third-party websites" (otherwise use `rel="noreferrer"`); list the capability path in `robots.txt`; `GET` must have no side effects; return `410 Gone`/`404` after expiry; and redirect from the capability URL to a canonical URL.
  - https://www.w3.org/TR/capability-urls/
- OWASP Forgot Password: the reset page should set a Referrer Policy of `no-referrer`.
- MDN, Referrer-Policy: `no-referrer` means "sent requests do not include any referrer information". The browser default, `strict-origin-when-cross-origin`, already sends only the origin to other sites. https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Referrer-Policy
  - So modern browsers don't leak the path to other sites by default. Setting `no-referrer` explicitly covers older or odd clients and same-origin logs.

**Rate limiting on Workers**
- Workers Rate Limiting binding: "Rate limits that you define and enforce in your Worker are local to the Cloudflare location". It is "permissive, eventually consistent, and intentionally designed to not be used as an accurate accounting system". `period` "Must be either `10` or `60`". Cloudflare also advises against using IP addresses as keys. https://developers.cloudflare.com/workers/runtime-apis/bindings/rate-limit/
- So the binding works as a coarse brake on abuse, keyed per GitHub user ID (and, before sign-in, per IP). It can't be an exact quota. The exact limit (max uses) already lives in the link VO. **HIGH**.

### 1.3 Prior art: shared invite links are a normal, accepted pattern

| Product | How their links work | Source |
|---|---|---|
| Slack | "Each invite link is active for 30 days, and can be used by up to 400 people." Admins can require approval: "members can request invitations for admins to approve or deny." Domain-based joining is a separate feature ("Workspace Owners can approve certain email domains") | https://slack.com/help/articles/201330256 |
| Discord | `max_age`: "between 0 and 604800 (7 days)", default 86400. `max_uses`: "0 for unlimited. between 0 and 100". `temporary`: "only grants temporary membership". Invites can be revoked | https://docs.discord.com/developers/resources/channel |
| GitHub Classroom (GitHub's own invite link) | "Anyone with an invitation URL for an assignment can accept the invitation". "Be careful where you share invitation URLs." Setting it Inactive makes the link invalid. Identity is the student's GitHub account | https://docs.github.com/en/education/manage-coursework-with-github-classroom/teach-with-github-classroom/create-an-individual-assignment |
| Google Drive | "Anyone who has the link can use your file, without signing in". Google later added a resource key to some link URLs, which "helps protect your file from unintended access" | https://support.google.com/docs/answer/2494822 ; https://knowledge.workspace.google.com/admin/drive/manage-the-link-sharing-security-update-for-files |

Takeaways:
- Every one of these uses a bearer link, plus expiry, plus max uses or revocation.
- Slack and GitHub Classroom also tie each use to a signed-in identity, as P1 does.
- Slack offers an approval mode.
- Google's resource-key change shows that links which proved too guessable or too leaky got more secret material added.
- P1's planned set (GitHub sign-in, max uses, expiry, approval, revoke/pause, audit) is stricter than Slack's or Classroom's defaults. **MEDIUM**: the comparison is ours. No source ranks these products.

GitHub has no native "invite link" for orgs or repos. The only first-party example found is GitHub Classroom. **MEDIUM**: no docs page for such a feature was found, and the absence was not proven.

### 1.4 Threat analysis for P1

1. **Online guessing.** Take 80 random bits, 10⁴ live links, and an attacker making 10⁶ guesses/s for a year (about 2⁴⁵ guesses). The chance of a hit is about 2⁻²². With GitHub sign-in on every request and per-user rate limits, this is not a practical threat. It is still below the RFC 6749 bar of 2⁻¹²⁸ per token. **MEDIUM** (arithmetic).
2. **Leakage (the realistic threat).**
   - Links are meant to be shared, so they will end up in chat, email, tickets and logs.
   - Because the code doubles as the object ID, it also leaks through every log line, every Restate key and every DB row.
   - Compensating controls cap the damage from a leaked link:
     - the user must sign in to GitHub, so every use has a named identity and lands in the audit log;
     - max uses and expiry bound how many people can use it;
     - approval mode stops admission until a human approves;
     - the email-domain check (P1-F05, where SP-19 allows it) narrows who can use it;
     - pause/revoke stops new admissions at once (PoC VO, R-P1-link-race).
3. **Abuse and mass requests.** Many sign-ins or requests against one leaked link could:
   - spam approvers;
   - burn max uses (a "use" is a created request, PoC §2);
   - burn GitHub's invite budget (50 per 24 h per repo; 50 or 500 per 24 h per org, `research/github-invitations.md`).

   Mitigations:
   - one pending or approved request per user per link (already in the PoC);
   - per-user and per-link rate limits;
   - an optional cap on pending requests per link;
   - an alert to the admin when a link's request rate spikes.
4. **Open-link links (no approval, unlimited uses, never expires).** This is where leakage costs the most. The spec should make it hard to create: warn the admin, require at least one guardrail, or default expiry to N days, as Slack and Discord do.
5. **OAuth round trip.** The link secret must not ride through GitHub's authorize URL (`state`/`redirect_uri`) or the post-login redirect URL. Keep it in the encrypted session (the PoC already has encrypted KV sessions) and redirect to a secret-free canonical page. **MEDIUM**: this is design guidance drawn from RFC 6750 §5.3 and the W3C capability-URL redirect advice.

### 1.5 Verdict and required controls

**Verdict: feasible-with-changes.** The P1 design (bearer links, plus GitHub OAuth identity, plus guardrails, plus optional approval) matches established prior art and is safe enough. It needs one change to the PoC's link-identity model, plus standard hardening.

Required controls for the P1 spec:
1. **Split the link ID from the link secret.**
   - The link keeps an internal ID (ULID is fine for keys, logs and the console).
   - The shareable URL carries a separate **secret of 128 bits or more** from a CSPRNG, for example 32 bytes as base64url.
2. **Store only the hash.** Keep only `SHA-256(secret)` in D1 and Restate state. Look links up by hash and compare in constant time. Never log the secret: add it to the PoC's redaction set.
3. **Rotate the secret.** Admins can "reset link URL" to issue a new secret for the same link. The old URL dies; uses and audit carry over.
4. **Lifecycle.** Expiry, max uses, pause and revoke are all enforced in the admission VO, as the PoC does. A dead or unknown link returns the same 404/410 response.
5. **Headers on link pages.**
   - `Referrer-Policy: no-referrer`.
   - No third-party scripts or links (the existing CSP already allows `self` only).
   - `X-Robots-Tag: noindex` on the link path, and `Cache-Control: no-store`.
6. **Redirect after use.** `GET /i/{secret}` has no side effects. It stores the pending link in the encrypted session and 302s to a canonical page without the secret, both before and after the GitHub OAuth round trip. The secret never goes into the OAuth `state` or `redirect_uri`.
7. **Identity required.** Every request needs a GitHub-authenticated user. There is one pending or approved request per user per link (already in the PoC).
8. **Rate limits.**
   - Workers Rate Limiting binding per IP on the unauthenticated link route and on `/oauth/callback`, plus per GitHub user ID on request creation.
   - A per-link cap on requests per hour, with a "link under abuse, auto-paused" state and an admin alert.
   - Cloudflare WAF rate-limit rules as an outer layer (optional).
9. **Safe defaults when creating a link.** Default expiry of 7–30 days, as Slack and Discord do. Warn on "no approval + unlimited uses + no expiry". Show the sharing warning GitHub Classroom shows.
10. **Audit.** Record secret creation and rotation (never the secret itself), each request with its GitHub identity, and each auto-pause.

None of these needs a spike. All of them are spec and design items.

---

## 2. R-ALL-gh-terms: do P1 and P2 conflict with GitHub's terms?

### 2.1 What the policies say

**GitHub Acceptable Use Policies, §4 "Spam and Inauthentic Activity on GitHub"** (https://docs.github.com/en/site-policy/acceptable-use-policies/github-acceptable-use-policies#4-spam-and-inauthentic-activity-on-github). The section opens with "We do not allow content or activity on GitHub that is:" and lists:
- "automated excessive bulk activity and coordinated inauthentic activity, such as spamming, cryptocurrency mining;"
- "inauthentic interactions, such as fake accounts and automated inauthentic activity;"
- "rank abuse, such as automated starring or following;"
- "using our servers for any form of excessive automated bulk activity, to place undue burden on our servers through automated means, or to relay any form of unsolicited advertising or solicitation through our servers".

**Acceptable Use Policies, §7** (same URL). It says "Scraping does not refer to the collection of information through our API", and that data must not be used "for spamming purposes, including for the purposes of sending unsolicited emails to users".

**Disrupting the experience of other users** (https://docs.github.com/en/site-policy/acceptable-use-policies/github-disrupting-the-experience-of-other-users) prohibits:
- "Posting off-topic comments";
- "Engaging with platform features in a way that causes excessive notifications for other users".

**Impersonation** (https://docs.github.com/en/site-policy/acceptable-use-policies/github-impersonation): "You may not misrepresent your identity … in a way that misleads or deceives others". One of the listed examples is "Accessing an account or organization with another user's token or credentials".

**Terms of Service**, §B.3 and §H (https://docs.github.com/en/site-policy/github-terms/github-terms-of-service):
- §B.3: "Accounts registered by 'bots' or other automated methods are not permitted. We do permit machine accounts".
- §H: "You agree to these Terms of Service, plus this Section H, when using any of GitHub's APIs …, including use of the API through a third party product that accesses GitHub."
- §H: "Abuse or excessively frequent requests to GitHub via the API may result in the temporary or permanent suspension of your Account's access to the API. GitHub, in our sole discretion, will determine abuse or excessive usage of the API."
- §H: "You may not share API tokens to exceed GitHub's rate limitations."
- §B: "If you are the owner of an Organization … we consider you responsible for the actions that are performed on or through that Organization."

**GitHub Marketplace Developer Agreement** (https://docs.github.com/en/site-policy/github-terms/github-marketplace-developer-agreement). This applies only if we list on the Marketplace.
- §3.7: "Developer shall not violate any explicit rate limitations on calling or otherwise utilizing a GitHub API."
- §5.4(iv): products must not be "designed to or utilized for the purpose of sending commercial electronic messages to any GitHub customers … without their consent".
- §3.1: inform End Users, "provide a legally adequate privacy notice", and "obtain valid End User consent as required".

**Marketplace listing requirements** (https://docs.github.com/en/apps/github-marketplace/creating-apps-for-github-marketplace/requirements-for-listing-an-app). These cover UX and support only, for example "Apps must provide value to customers and integrate with the platform in some way beyond authentication." Nothing here touches acting for users or bulk actions.

**GitHub App docs: acting for a user is the sanctioned pattern.**
- "if your GitHub App takes an action on behalf of a user, it should always use a user access token" (https://docs.github.com/en/apps/creating-github-apps/about-creating-github-apps/best-practices-for-creating-a-github-app).
- "API requests made by an app on behalf of a user will be attributed to that user … the GitHub UI will show the user's avatar photo along with the app's identicon badge" (https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-with-a-github-app-on-behalf-of-a-user).
- Both were already quoted in `research/github-app-tokens.md` Q5.

**Operational limits that act as the enforceable form of "excessive".**
- REST best practices (https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api):
  - "make requests serially instead of concurrently";
  - for many POST/PATCH/PUT/DELETE requests, "wait at least one second between each request";
  - "Continuing to make requests while you are rate limited may result in the banning of your integration."
- Secondary limits (https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api): "no more than 80 content-generating requests per minute and no more than 500 content-generating requests per hour".
- Invitations (from `research/github-invitations.md`):
  - "To prevent abuse, organization owners are limited to creating 50 organization invitations for an organization within a 24 hour period. If the organization is more than one month old or on a paid plan, the limit is 500".
  - "You are limited to sending 50 invitations to a repository per 24 hour period. Note there is no limit if you are inviting organization members to an organization repository."
  - The org invitation endpoint "triggers notifications. Creating content too quickly using this endpoint may result in secondary rate limiting", and returns 422 if "the endpoint has been spammed".

### 2.2 Assessment

No GitHub policy bans automation as such. What the AUP prohibits is *excessive*, *inauthentic*, *spam* or *misleading* activity, and the API terms leave "abuse or excessive usage" to GitHub's discretion. The question is therefore whether P1 and P2 activity is authentic, consented to, and within limits. **HIGH** on the policy wording. The application below is **MEDIUM**.

**P1, links (pull model).**
- Each invitation goes to a person who visited a link, signed in with their own GitHub account, and asked for access. Org or repo admins configured the link and installed the app.
- These are wanted, authentic invitations made through the documented invitation API with an installation token. Nothing here is unsolicited, fake or misleading. **No conflict.**

**P1-F29, bulk invite (push model).**
- An org admin pastes handles or emails for people they mean to invite. GitHub's own UI offers the same action for orgs (an invite dialog with multiple users; `research/github-invitations.md` cites a review step "like GitHub's").
- What matters is volume and consent. GitHub enforces the per-org and per-repo daily caps "to prevent abuse", and the 422 "spammed" response marks the edge.
- Inviting by **email** to addresses that aren't GitHub users sends email from GitHub. Mass-inviting unrelated addresses would read as "unsolicited emails" or spam.
- **No conflict** when the app keeps GitHub's caps (queue with daily-budget backoff, as already required by R-P1-ratelimit), honours `retry-after`/422, and runs only when an authorized admin triggers it.
- **UNVERIFIED:** no GitHub text explicitly blesses app-driven bulk invites. The conclusion is inferred from the absence of any ban plus the documented, rate-limited endpoint.

**P2, bulk merges and `@dependabot rebase` comments as the user.**
- Using a GitHub App user access token is GitHub's documented way to act for a user. The UI shows the app badge, so the action is attributed and not misleading.
- The Impersonation clause targets *misleading* use of another person's token. A user's own OAuth grant to the app, used for actions that user asked for, is the sanctioned case, not that one. **MEDIUM**: this is our reading, since the clause has no app-specific carve-out.
- Merging Dependabot PRs and posting the documented `@dependabot rebase` command on one's own repos are on-topic, functional actions. They are not off-topic comments, rank abuse or promotion.
- Two edges matter:
  - **Volume.** Hundreds of merges or comments in one click can hit the 80/min and 500/h content limits. Bursty comments also notify watchers ("excessive notifications").
  - **Unattended action.** Acting on a stored refresh token long after the user clicked, or on a schedule they didn't set up, weakens the "the user did this" story.
- **No conflict** when bulk jobs are user-initiated or explicitly user-configured, are serialized and throttled under the secondary limits (`research/dependaboard-prs.md` already prescribes a per-user/installation rate governor), and only comment where a rebase is actually needed.

**Machine accounts.** Neither project needs a bot or machine *user account*. Both act as a GitHub App (installation token) or as the signed-in human (user token), so ToS §B.3 isn't engaged. **HIGH**.

**Marketplace.** Listing is optional. If we list, Developer Agreement §3.1, §3.7 and §5.4 add a privacy notice and consent, respect for rate limits, and no unsolicited commercial messages. P1 and P2 already meet these, or can trivially.

### 2.3 Verdict and required constraints

**Verdict: resolved-feasible.** Neither P1 (automated invitations through an App) nor P2 (merges and comments with user tokens) conflicts with the GitHub ToS, the AUP or the Marketplace terms. No feature needs to be reworded or dropped.

Design constraints to carry into both specs:
1. **Human-initiated writes.**
   - P1 invitations come only from an invitee's own request on an admin-made link, or from an admin's explicit bulk action.
   - P2 merges and comments come only from a user click or a user-configured rule, executed with that user's token.
2. **Rate governor.**
   - Serialize mutative calls per installation and per user, with at least 1 s spacing for bulk runs.
   - Stay under 80/min and 500/h content creation, and under the invitation caps (50 per repo per 24 h; 50 or 500 per org per 24 h).
   - Honour `retry-after` and `x-ratelimit-*`, and treat 422 "spammed" as a stop signal, not a retry.
   - Never spread work across tokens to get past limits (ToS §H).
3. **Bulk-invite hygiene.** Bulk invites go through a review step and are capped per run. Email invites only go to addresses the admin vouches for, with no harvested lists (AUP §7).
4. **Comment hygiene (P2).** Post `@dependabot rebase` only on PRs that are actually conflicted. Post at most once per PR until its state changes, so notifications aren't spammed.
5. **Transparency.** Actions carry the app badge (automatic with user tokens). Tell users in the UI and the privacy notice that the app acts as them. Make token revocation easy.
6. **If listed on the Marketplace,** publish a privacy notice and get consent (Developer Agreement §3.1).

What remains unverified, and is acceptable:
- GitHub's discretion over "excessive" (ToS §H) has no numeric threshold beyond the documented rate limits.
- Whether app-made invitations share the per-org invitation bucket is already tracked under R-P1-ratelimit / SP-08.

---

## 3. Risk verdicts

| Risk | Status | One line | Required controls |
|---|---|---|---|
| R-P1-link-secrecy | **feasible-with-changes** | Bearer links with GitHub sign-in, guardrails and optional approval are safe and match prior art (Slack, Discord, GitHub Classroom). The PoC's code = link ULID gives only 80 random bits and doubles as an object ID that shows up in logs, Restate and the DB | A separate ≥128-bit CSPRNG secret, stored as SHA-256 only and redacted from logs; secret rotation; expiry, max uses, pause and revoke in the VO; `Referrer-Policy: no-referrer`, noindex, no-store, no third-party content; secret-free redirect, kept out of OAuth `state`; sign-in required; per-IP, per-user and per-link rate limits (Workers binding plus WAF) with auto-pause and an admin alert; safe defaults when creating links; audit |
| R-ALL-gh-terms | **resolved-feasible** | The ToS and AUP ban excessive, inauthentic, spam or misleading automation, not automation as such. User-token actions with the app badge are GitHub's documented pattern, and invitations go through documented, rate-capped endpoints | Writes are human-initiated or user-configured; a per-installation and per-user rate governor (serial, ≥1 s spacing, 80/min, 500/h, invite caps, `retry-after`, stop on 422); bulk-invite review step and caps, no unsolicited email invites; at most one rebase comment per PR state; transparency and a privacy notice (Marketplace §3.1 if listed) |
