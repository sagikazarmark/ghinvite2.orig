# Audit Log Page Design

**Status:** approved for implementation planning  
**Date:** 2026-05-24  
**Scope:** Replace the current `/accounts/{login}/audit` not-implemented response with an authenticated Account Console coming-soon page.

## Goal

Fix the Audit Log route so account admins stay inside the Account Console when they click `Audit log`.

The page should be honest about the feature state. It should communicate that ghinvite records account activity, while making clear that console browsing is not available yet.

## Current State

`crates/web/src/routes/dashboard.rs` registers `/accounts/{login}/audit`, but the route uses a plain `stub` handler that returns HTTP `501 Not Implemented` and text `Audit log is not yet implemented.`

The handler does not take `RequireAdminOf`, unlike the rest of the account dashboard routes. As a result, the not-implemented response is reachable without the normal account-admin authorization behavior.

`DashboardLayout` already includes `Audit log` navigation links on desktop and mobile, but the layout has no active navigation state for audit.

Production storage currently exposes audit event appends through the `Storage::audit` method. There is no production read-model API for listing audit events in the web app. The only audit reader is `SqlxStorage::debug_list_audit`, which is used by tests and debug assertions.

## Chosen Approach

Implement an authenticated empty-state Audit Log page.

- `/accounts/{login}/audit` should render a normal dashboard page with HTTP `200` for authorized account admins.
- The route should use `RequireAdminOf`, matching Overview, Links, Requests, and Settings.
- Unauthorized, unauthenticated, missing-account, and uninstalled-account access should continue to surface through the existing dashboard protection behavior as a plain `404`.
- The page should not read audit events.
- The page should not show disabled tables, filters, sample rows, or table headers.

This avoids fake UI, avoids implying a real query returned zero events, and keeps the implementation small.

## Page Content

Use the existing Account Console visual vocabulary: `DashboardLayout`, the current restrained product theme, existing dashboard spacing, and a single `mac-panel` empty state.

Header:

```text
Audit log
```

Header subtext:

```text
Review account activity for invitation links, requests, and GitHub invitations.
```

Empty-state title:

```text
Audit log coming soon
```

Empty-state body:

```text
ghinvite records account activity for invitation links, requests, and GitHub invitations. Console browsing is not available yet.
```

Primary action:

```text
Back to overview
```

The action links to `/accounts/{login}`.

Avoid copy such as `No events` or `Empty audit log`, because the page is not performing an audit query.

## Navigation

Keep the dashboard navigation label as `Audit log`.

When rendering the audit page, pass `active_nav: Some("audit".to_string())` so the desktop and mobile audit navigation links receive the existing active treatment and `aria-current="page"`.

Do not label the navigation item as `Audit log (coming soon)`. The destination page explains the feature state.

## Implementation Shape

Add a small view module:

- `crates/web/src/views/audit.rs`: `AuditLogPage` Dioxus component.
- `crates/web/src/views/mod.rs`: export the `audit` module.

Update routing and layout:

- Replace the `stub` route handler in `crates/web/src/routes/dashboard.rs` with `audit_page`.
- `audit_page` accepts `RequireAdminOf`, takes flash from the session, and renders `AuditLogPage`.
- Extend `DashboardLayout` audit links to support the `audit` active navigation state in both desktop and mobile navigation.

No storage, command, audit-event, Restate, or domain model changes are required.

## Testing Strategy

Add or update focused tests:

- `crates/web/src/views/audit.rs`: render test for `Audit log`, `Audit log coming soon`, the exact body copy, `Back to overview`, `mac-panel`, `<title>Audit log · acme</title>`, and absence of table/filter placeholder text.
- `crates/web/src/views/layouts.rs`: update the dashboard layout test or add a focused assertion that `active_nav: Some("audit")` marks the Audit Log nav item active with `aria-current="page"`.
- `crates/web/tests/route_smoke.rs`: replace `dashboard_routes_return_501` with an unauthenticated audit-route test that expects plain `404`.
- `crates/web/tests/dashboard_flow.rs`: add an authenticated admin test for `/accounts/acme/audit` that expects HTTP `200`, dashboard chrome, coming-soon copy, overview link, and active nav.

Run:

```bash
cargo fmt
cargo test -p web audit --lib
cargo test -p web dashboard --test dashboard_flow
cargo test -p web dashboard_audit_route_unauthenticated_returns_plain_404 --test route_smoke
```

Before completion, run:

```bash
cargo test -p web
```

## Non-Goals

- Do not implement audit event browsing.
- Do not add storage read APIs for audit events.
- Do not add filtering, pagination, search, export, sorting, or event-detail views.
- Do not render disabled controls, fake rows, sample audit events, or table headers.
- Do not add a new visual system or special illustration treatment.
- Do not change audit event schemas or event emission behavior.

## Acceptance Criteria

- Authorized account admins can visit `/accounts/{login}/audit` and receive a dashboard HTML page with HTTP `200`.
- The page clearly says `Audit log coming soon` and explains that console browsing is not available yet.
- The page links back to the account overview.
- The Audit Log navigation item is active on the page.
- Unauthenticated access to `/accounts/{login}/audit` returns the same plain `404` behavior as protected dashboard routes.
- No audit events are read or rendered.
- No disabled table, filter, or sample-row preview is shown.
