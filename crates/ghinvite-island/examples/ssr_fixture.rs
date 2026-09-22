//! Print the server-rendered new invitation link page as a static HTML
//! document, for smoke-testing the island in a browser without the full
//! stack (see the crate README).
//!
//! ```bash
//! cargo run -p ghinvite-island --example ssr_fixture               > dist/public/index.html
//! cargo run -p ghinvite-island --example ssr_fixture -- --with-errors > dist/public/index.html
//! ```
//!
//! The output is exactly what `ghinvite-web` sends for the page (it renders
//! the same `LinkCreateFormPage` with the same renderer settings), so the
//! island's props blob and module tag are the real ones. Serve `dist/public`
//! statically and the island mounts from `/assets/ghinvite-island.js`; the
//! stylesheet and `/static/app.js` must also be served for layout/theme checks.
//! `tests/browser` provides a local server with those assets, the production
//! CSP, and a POST echo endpoint; see its README for repeatable browser tests.
//!
//! **Never deploy the resulting `index.html`.** `dist/public` is the Static
//! Assets directory of the Worker, where an `index.html` would shadow `/`.
//! `scripts/build-island.sh` recreates the directory without it.

use chrono::Utc;
use dioxus::prelude::*;
use ghinvite_ui::link_form::{LinkFormErrors, LinkFormValues, RepositoryChoice, SUMMARY_MESSAGE};
use ghinvite_ui::links::LinkCreateFormPage;

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let form = if args.iter().any(|arg| arg == "--browser-rejection") {
        transport_rejection(
            args.iter().any(|arg| arg == "--parse-blocked"),
            args.iter().any(|arg| arg == "--expiration"),
            args.iter().any(|arg| arg == "--form-only"),
        )
    } else if args.iter().any(|arg| arg == "--permission-only-invalid") {
        LinkFormValues {
            description: "Permission workshop".into(),
            permission: if args.iter().any(|arg| arg == "--empty-permission") {
                ""
            } else {
                "owner"
            }
            .into(),
            selected_repo_ids: vec![10],
            errors: if args.iter().any(|arg| arg == "--unvalidated") {
                LinkFormErrors::default()
            } else {
                LinkFormErrors {
                    summary: vec![SUMMARY_MESSAGE.into()],
                    permission: Some(ghinvite_ui::link_form::PERMISSION_UNSUPPORTED.into()),
                    ..LinkFormErrors::default()
                }
            },
            ..LinkFormValues::default()
        }
    } else if args.iter().any(|arg| arg == "--numeric-only-invalid") {
        let expiration = args.iter().any(|arg| arg == "--expiration");
        let mut form = LinkFormValues {
            description: "Numeric workshop".into(),
            selected_repo_ids: vec![10],
            ..LinkFormValues::default()
        };
        form.errors.summary = vec![SUMMARY_MESSAGE.into()];
        if expiration {
            form.expires_in_days = "abc".into();
            form.errors.expires_in_days =
                Some(ghinvite_ui::link_form::EXPIRES_IN_DAYS_NOT_POSITIVE.into());
        } else {
            form.max_uses = "abc".into();
            form.errors.max_uses = Some(ghinvite_ui::link_form::MAX_USES_NOT_POSITIVE.into());
        }
        form
    } else if args.iter().any(|arg| arg == "--multiline-description") {
        // Only a crafted POST reaches the server with newlines in a
        // single-line field, but the server preserves and rejects what it got.
        // A `type="text"` input then shows it without the newline, which the
        // island must not read as the admin having rewritten it.
        LinkFormValues {
            description: "Workshop\nsecond line".into(),
            selected_repo_ids: vec![10],
            errors: LinkFormErrors {
                summary: vec![SUMMARY_MESSAGE.into()],
                description: Some(ghinvite_ui::link_form::DESCRIPTION_SINGLE_LINE.into()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        }
    } else if args.iter().any(|arg| arg == "--preserved-values") {
        preserved_submission()
    } else if args.iter().any(|arg| arg == "--with-errors") {
        failed_submission()
    } else {
        LinkFormValues::default()
    };
    let now = Utc::now();

    let mut vdom = VirtualDom::new_with_props(
        move || {
            rsx! {
                LinkCreateFormPage {
                    action: "/console/accounts/acme/links".to_string(),
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: repos(),
                    form: form.clone(),
                    now,
                }
            }
        },
        (),
    );
    vdom.provide_root_context(ghinvite_ui::csrf::CsrfToken(
        std::env::var("GHINVITE_FIXTURE_CSRF").ok(),
    ));
    vdom.rebuild_in_place();
    println!(
        "{}",
        ghinvite_ui::document::document_shell(&dioxus_ssr::render(&vdom))
    );
}

/// Synthetic response payloads exercise rejection transport, not domain rules.
/// The browser can reproduce the description/parse errors, but neither of the
/// fixture-only messages below is emitted by a production validator.
fn transport_rejection(parse_blocked: bool, expiration: bool, form_only: bool) -> LinkFormValues {
    let mut form = LinkFormValues {
        description: "Transport workshop".into(),
        permission: "push".into(),
        max_uses: "7".into(),
        expires_in_days: "45".into(),
        internal_note: "Preserved transport note".into(),
        selected_repo_ids: vec![10],
        errors: LinkFormErrors {
            summary: vec![
                SUMMARY_MESSAGE.into(),
                "Transport fixture: server-only form rejection.".into(),
            ],
            ..LinkFormErrors::default()
        },
        ..LinkFormValues::default()
    };
    if !form_only {
        form.description = "   ".into();
        form.errors.description = Some(ghinvite_ui::link_form::DESCRIPTION_REQUIRED.into());
        form.errors.permission =
            Some("Transport fixture: server-only permission rejection.".into());
    }
    if parse_blocked {
        if expiration {
            form.expires_in_days = "abc".into();
            form.errors.expires_in_days =
                Some(ghinvite_ui::link_form::EXPIRES_IN_DAYS_NOT_POSITIVE.into());
        } else {
            form.max_uses = "abc".into();
            form.errors.max_uses = Some(ghinvite_ui::link_form::MAX_USES_NOT_POSITIVE.into());
        }
    }
    form
}

/// A rejected description alongside otherwise valid values, to check that
/// mounting and correcting one field do not reset the other controls.
fn preserved_submission() -> LinkFormValues {
    LinkFormValues {
        description: "   ".to_string(),
        permission: "push".to_string(),
        approval_required: true,
        max_uses: "7".to_string(),
        expires_in_days: "45".to_string(),
        internal_note: "Keep this note & its <literal> markup".to_string(),
        selected_repo_ids: vec![11, 12],
        errors: LinkFormErrors {
            summary: vec![SUMMARY_MESSAGE.to_string()],
            description: Some(ghinvite_ui::link_form::DESCRIPTION_REQUIRED.to_string()),
            ..LinkFormErrors::default()
        },
    }
}

fn repos() -> Vec<RepositoryChoice> {
    vec![
        RepositoryChoice {
            id: 10,
            full_name: "acme/api".to_string(),
        },
        RepositoryChoice {
            id: 11,
            full_name: "acme/web".to_string(),
        },
        RepositoryChoice {
            id: 12,
            full_name: "acme/docs".to_string(),
        },
    ]
}

/// A POST in which every rule failed, as the server re-renders it: values
/// preserved verbatim, one message per field (the same fixture as
/// `tests/parity.rs`).
fn failed_submission() -> LinkFormValues {
    use ghinvite_ui::link_form::{
        DESCRIPTION_REQUIRED, EXPIRES_IN_DAYS_NOT_POSITIVE, MAX_USES_NOT_POSITIVE,
        PERMISSION_UNSUPPORTED, REPO_SCOPE_REQUIRED,
    };
    LinkFormValues {
        description: "   ".to_string(),
        permission: "owner".to_string(),
        approval_required: true,
        max_uses: "abc".to_string(),
        expires_in_days: "0".to_string(),
        internal_note: "Keep this note".to_string(),
        selected_repo_ids: vec![999],
        errors: LinkFormErrors {
            internal_note: None,
            summary: vec![SUMMARY_MESSAGE.to_string()],
            description: Some(DESCRIPTION_REQUIRED.to_string()),
            permission: Some(PERMISSION_UNSUPPORTED.to_string()),
            max_uses: Some(MAX_USES_NOT_POSITIVE.to_string()),
            expires_in_days: Some(EXPIRES_IN_DAYS_NOT_POSITIVE.to_string()),
            repo_scope: Some(REPO_SCOPE_REQUIRED.to_string()),
        },
    }
}
