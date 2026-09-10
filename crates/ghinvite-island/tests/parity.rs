//! Markup parity: the island must render, on its first frame, exactly the HTML
//! the server rendered for the same props.
//!
//! `LinkFormIsland` is the reactive form (dioform bindings + progressive
//! submit) and `ghinvite_ui::links::LinkCreateForm` is the server's plain
//! component. Both are rendered here with `dioxus_ssr` (the `dioform` facade
//! works natively; hooks run, the seeding hook included) and the strings are
//! compared byte for byte — no normalisation. That pins down not only the
//! layout, which the island shares by calling the same component, but the
//! island's *state mapping*: the initial model built from the props, the
//! server errors seeded through dioform's submission lifecycle, the raw
//! numeric text that does not parse, and how visible errors fold back into
//! `LinkFormErrors` (summary line, one message per field).
//!
//! In the browser this is the frame that replaces the server-rendered form
//! when the island mounts; identical markup means no visible change.

use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use ghinvite_island::LinkFormIsland;
use ghinvite_ui::link_form::{
    self, LinkFormErrors, LinkFormIslandProps, LinkFormValues, RepositoryChoice,
};
use ghinvite_ui::links::LinkCreateForm;

const ACTION: &str = "/console/accounts/acme/links";

fn render<F>(component: F) -> String
where
    F: 'static + Clone + Fn() -> Element + Send,
{
    let mut vdom = VirtualDom::new_with_props(component, ());
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-05-04T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn acme_repos() -> Vec<RepositoryChoice> {
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

/// The server's view of the form and the island's view of the same props,
/// rendered side by side.
fn render_both(values: LinkFormValues, repos: Vec<RepositoryChoice>) -> (String, String) {
    let (server_values, server_repos) = (values.clone(), repos.clone());
    let server = render(move || {
        rsx! {
            LinkCreateForm {
                action: ACTION.to_string(),
                form: server_values.clone(),
                repos: server_repos.clone(),
            }
        }
    });
    let island = render(move || {
        rsx! {
            LinkFormIsland {
                action: ACTION.to_string(),
                values: values.clone(),
                repos: repos.clone(),
                now: now(),
            }
        }
    });
    (server, island)
}

fn assert_parity(values: LinkFormValues, repos: Vec<RepositoryChoice>) {
    let (server, island) = render_both(values, repos);
    assert!(server.starts_with("<form "), "server did not render a form");
    assert_eq!(island, server, "island markup differs from the server's");
}

/// What the server hands back after a POST in which every rule failed:
/// values preserved verbatim, including a tampered permission, raw numeric
/// text that does not parse, and a repository id the installation does not
/// expose. The messages are the ones the shared parsers and validators emit
/// for exactly these values — so the island, which re-runs them, must arrive
/// at the same errors.
fn failed_submission() -> LinkFormValues {
    LinkFormValues {
        description: "   ".to_string(),
        permission: "owner".to_string(),
        approval_required: true,
        max_uses: "abc".to_string(),
        expires_in_days: "0".to_string(),
        internal_note: "Keep this note".to_string(),
        selected_repo_ids: vec![999],
        errors: LinkFormErrors {
            summary: vec![link_form::SUMMARY_MESSAGE.to_string()],
            description: Some(link_form::DESCRIPTION_REQUIRED.to_string()),
            permission: Some(link_form::PERMISSION_UNSUPPORTED.to_string()),
            max_uses: Some(link_form::MAX_USES_NOT_POSITIVE.to_string()),
            expires_in_days: Some(link_form::EXPIRES_IN_DAYS_NOT_POSITIVE.to_string()),
            repo_scope: Some(link_form::REPO_SCOPE_REQUIRED.to_string()),
        },
    }
}

#[test]
fn fresh_form_renders_identically() {
    assert_parity(LinkFormValues::default(), acme_repos());
}

#[test]
fn failed_submission_renders_identically() {
    assert_parity(failed_submission(), acme_repos());
}

#[test]
fn failed_submission_without_available_repositories_renders_identically() {
    assert_parity(failed_submission(), vec![]);
}

#[test]
fn server_only_errors_render_identically() {
    // Errors the browser cannot reproduce (no client rule rejects these
    // values): the island must seed them through the submission lifecycle
    // rather than rely on its own validators, and a summary-only message
    // must land in the alert.
    let values = LinkFormValues {
        description: "AI coding workshop".to_string(),
        permission: "push".to_string(),
        approval_required: true,
        max_uses: "7".to_string(),
        expires_in_days: "45".to_string(),
        internal_note: String::new(),
        selected_repo_ids: vec![10, 12],
        errors: LinkFormErrors {
            summary: vec![
                link_form::SUMMARY_MESSAGE.to_string(),
                "This installation is suspended; no links can be created right now.".to_string(),
            ],
            description: Some("A link with this description already exists.".to_string()),
            ..LinkFormErrors::default()
        },
    };
    assert_parity(values, acme_repos());
}

#[test]
fn island_renders_the_expected_error_state_not_just_the_same_string() {
    // Guard against both sides agreeing on something wrong: spot-check the
    // rendered island for the seeded state.
    let (_, island) = render_both(failed_submission(), acme_repos());

    assert!(island.contains("id=\"link-form-errors\""));
    assert!(island.contains(link_form::SUMMARY_MESSAGE));
    assert!(island.contains(&format!(
        "<p id=\"description-error\" class=\"text-sm font-medium text-error\">{}</p>",
        link_form::DESCRIPTION_REQUIRED
    )));
    assert!(island.contains(&format!(
        "<p id=\"permission-error\" class=\"text-sm font-medium text-error\">{}</p>",
        link_form::PERMISSION_UNSUPPORTED
    )));
    assert!(island.contains(&format!(
        "<p id=\"max_uses-error\" class=\"text-sm font-medium text-error\">{}</p>",
        link_form::MAX_USES_NOT_POSITIVE
    )));
    assert!(island.contains(&format!(
        "<p id=\"expires_in_days-error\" class=\"text-sm font-medium text-error\">{}</p>",
        link_form::EXPIRES_IN_DAYS_NOT_POSITIVE
    )));
    assert!(island.contains(&format!(
        "<p id=\"repo_ids-error\" class=\"text-sm font-medium text-error\">{}</p>",
        link_form::REPO_SCOPE_REQUIRED
    )));
    // Raw numeric text preserved (the parse error keeps it), values verbatim.
    assert!(island.contains("name=\"max_uses\" value=\"abc\""));
    assert!(island.contains("name=\"expires_in_days\" value=\"0\""));
    let description = island
        .split("<input")
        .find(|input| {
            input
                .split('>')
                .next()
                .unwrap()
                .contains("id=\"description\"")
        })
        .unwrap()
        .split('>')
        .next()
        .unwrap();
    assert!(description.contains("name=\"description\""));
    assert!(description.contains("value=\"   \""));
    assert!(island.contains("name=\"approval_required\" value=\"true\" checked"));
    assert!(island.contains(">Keep this note</textarea>"));
    // Tampered values never reach the markup.
    assert!(!island.contains("owner"));
    assert!(!island.contains("999"));
    assert!(!island.contains(" checked=true class=\"checkbox checkbox-sm\""));
    assert_eq!(island.matches("<form").count(), 1);
}

#[test]
fn island_props_from_the_page_blob_round_trip_into_the_component() {
    // The entrypoint deserialises the page's JSON block into
    // `LinkFormIslandProps`; make sure a blob produced by the page code
    // renders to the same markup as the typed props.
    let props = LinkFormIslandProps {
        action: ACTION.to_string(),
        values: failed_submission(),
        repos: acme_repos(),
        now: now(),
    };
    let json = props.script_json();
    let parsed: LinkFormIslandProps = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, props);

    let (server, island) = render_both(parsed.values, parsed.repos);
    assert_eq!(island, server);
}
