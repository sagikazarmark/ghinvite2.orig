# App onboarding and installation requests

The Console discovers canonical repository-app and organization-app installations together, deduplicated by numeric account ID. Conflicting account type/login bindings fail closed. An organization-only installation can therefore lead directly to its account Console under the existing current-owner authority checks.

`/onboarding` explains the two checked-in manifests separately: the repository app requests Administration write and Metadata read; the organization app requests Members write and Metadata read. It links to the existing installation routes and to authenticated pending requests. The account Apps panel requires current non-delegated Console authority and shows each app's canonical installation/suspension state. Every authorized account Console render supplies a typed, per-document root context to the shared header. Owners see both app states and add/manage links; delegated repository administrators receive only repository-app state and the generic chooser. Facts come from the current authority view, never session state or additional GitHub calls. Account-login binding prevents a context from being rendered under another account.

The selected app JWT calls the complete paginated [GitHub installation-request endpoint](https://docs.github.com/en/rest/apps/apps#list-installation-requests-for-the-authenticated-app). Its documented requester object contains a numeric ID without requiring a login. Global requests are filtered to the signed-in numeric requester; account requests are filtered to the independently authorized numeric account. Invalid IDs, conflicting/duplicate request identities, invalid organization identity or an incomplete list produce an unavailable view. No successful prefix or upstream error prose is displayed.

`setup_action=request` is an untrusted browser hint. It redirects to the authenticated listing; only GitHub's returned request can produce the awaiting-owner-approval copy. An empty list makes no success claim. Sign-in continuation permits only the four exact onboarding paths, including the bounded app selector. Responses are private/no-store. No app JWT, raw payload, or new authority is retained by this flow.

## Verification

- Native HTTP nine tests pass: `/tmp/onboarding-header-final-http.log`. They exercise real JWT issuance with exact app issuer, numeric-only requester decoding, multi-page results, viewer/account privacy, unknown/partial failures, request hints, org-only discovery, exact return paths, current header states across four Console pages, suspension and delegated-header privacy.
- Chromium two projects pass with and without JavaScript: `/tmp/onboarding-header-browser2.log`. Keyboard navigation, the separate permission rationale, confirmed pending request and privacy behavior use the actual native routes. Chooser and account Apps screenshots were visually inspected. Header navigation and the add-other-app link are covered.
- Full UI147 tests pass (`/tmp/onboarding-header-ui-final.log`), including the new Apps navigation link.
- Focused web tests/UI strict Clippy passes: `/tmp/onboarding-policy-final-strict.log`.
- Both browser CI workflows run `onboarding.config.mjs`. The ordinary web Worker build also compiled this route graph in `/tmp/platform-email-review-web-capture2.log`; that capture predates the final exact return-path/account-type checks and is build evidence, not a live GitHub onboarding proof.

ID-65 records the identity/privacy decision. Live SP-02 consent screenshots and installation-token visibility remain external gates; neither these fixtures nor the published API reference establish live org activation.
