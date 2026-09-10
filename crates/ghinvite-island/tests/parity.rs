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
fn numeric_guardrails_preserve_raw_values_and_metadata_on_the_first_pass() {
    for raw in ["0", "00", "abc", "4294967296", "", "7"] {
        let (max_error, expires_error) = match raw {
            "" | "7" => (None, None),
            "4294967296" => (
                Some(link_form::MAX_USES_TOO_LARGE),
                Some(link_form::EXPIRES_IN_DAYS_TOO_FAR),
            ),
            _ => (
                Some(link_form::MAX_USES_NOT_POSITIVE),
                Some(link_form::EXPIRES_IN_DAYS_NOT_POSITIVE),
            ),
        };
        let values = LinkFormValues {
            description: "Workshop".into(),
            selected_repo_ids: vec![10],
            max_uses: raw.into(),
            expires_in_days: raw.into(),
            errors: LinkFormErrors {
                summary: if max_error.is_some() || expires_error.is_some() {
                    vec![link_form::SUMMARY_MESSAGE.into()]
                } else {
                    vec![]
                },
                max_uses: max_error.map(str::to_string),
                expires_in_days: expires_error.map(str::to_string),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };
        // Repeated independent mounts must not leak registry associations or
        // replace a failed raw value with the typed model's blank fallback.
        for _ in 0..2 {
            let (server, island) = render_both(values.clone(), acme_repos());
            assert_eq!(island, server, "numeric raw={raw:?}");
            assert_numeric_input(&island, "max_uses", raw, max_error);
            assert_numeric_input(&island, "expires_in_days", raw, expires_error);
        }
    }
}

fn assert_numeric_input(html: &str, name: &str, raw: &str, error: Option<&str>) {
    let mut inputs = html
        .split("<input ")
        .skip(1)
        .map(|input| input.split_once('>').unwrap().0)
        .filter(|input| input.contains(&format!("name=\"{name}\"")));
    let input = inputs.next().expect("numeric input is present");
    assert!(inputs.next().is_none(), "numeric input is unique");
    for attribute in [
        format!("id=\"{name}\""),
        format!("value=\"{raw}\""),
        "type=\"number\"".into(),
        "inputmode=\"numeric\"".into(),
        "min=\"1\"".into(),
        format!("aria-labelledby=\"{name}-label\""),
        format!("aria-invalid=\"{}\"", error.is_some()),
    ] {
        assert!(input.contains(&attribute), "missing {attribute}: {input}");
    }
    assert!(!input.contains(" max="));
    assert!(!input.contains(" required="));
    let descriptions = if error.is_some() {
        format!("{name}-help {name}-error")
    } else {
        format!("{name}-help")
    };
    assert!(input.contains(&format!("aria-describedby=\"{descriptions}\"")));
    assert_eq!(
        input.contains(&format!("aria-errormessage=\"{name}-error\"")),
        error.is_some()
    );
    assert_eq!(input.contains("input-error"), error.is_some());
    let label = html
        .split("<label ")
        .skip(1)
        .map(|label| label.split_once('>').unwrap().0)
        .find(|label| label.contains(&format!("id=\"{name}-label\"")))
        .unwrap();
    assert!(label.contains(&format!("for=\"{name}\"")));
    let region = html.split_once(&format!("id=\"{name}-error\"")).unwrap().1;
    let (attributes, content) = region.split_once('>').unwrap();
    assert!(attributes.contains("aria-live=\"polite\""));
    assert!(!attributes.contains("hidden"));
    if let Some(error) = error {
        assert!(content.starts_with(&format!("<div>{error}</div></div>")));
    } else {
        assert!(content.starts_with("</div>"));
    }
    for id in [
        name.to_string(),
        format!("{name}-label"),
        format!("{name}-help"),
        format!("{name}-error"),
    ] {
        assert_eq!(html.matches(&format!(" id=\"{id}\"")).count(), 1);
    }
}

#[test]
fn every_permission_and_display_fallback_renders_identically_on_the_first_pass() {
    for raw in [
        "pull", "triage", "push", "maintain", "admin", "owner", "", "Push", " pull ",
    ] {
        let supported = ["pull", "triage", "push", "maintain", "admin"].contains(&raw);
        let values = LinkFormValues {
            description: "Workshop".into(),
            permission: raw.into(),
            selected_repo_ids: vec![10],
            errors: if supported {
                LinkFormErrors::default()
            } else {
                LinkFormErrors {
                    summary: vec![link_form::SUMMARY_MESSAGE.into()],
                    permission: Some(link_form::PERMISSION_UNSUPPORTED.into()),
                    ..LinkFormErrors::default()
                }
            },
            ..LinkFormValues::default()
        };
        let (server, island) = render_both(values, acme_repos());
        assert_eq!(island, server, "permission={raw:?}");
        let select = island
            .split_once("<select ")
            .unwrap()
            .1
            .split_once("</select>")
            .unwrap()
            .0;
        let (attributes, options) = select.split_once('>').unwrap();
        assert!(attributes.contains("name=\"permission\""));
        assert!(attributes.contains("aria-labelledby=\"permission-label\""));
        assert!(attributes.contains(&format!("aria-invalid=\"{}\"", !supported)));
        assert!(attributes.contains(if supported {
            "aria-describedby=\"permission-help\""
        } else {
            "aria-describedby=\"permission-help permission-error\""
        }));
        assert_eq!(
            attributes.contains("aria-errormessage=\"permission-error\""),
            !supported
        );
        let displayed = if supported { raw } else { "pull" };
        assert!(options.contains(&format!(
            "<option value=\"{displayed}\" selected=true>{displayed}</option>"
        )));
        assert_eq!(options.matches("<option ").count(), 5);
        assert_eq!(options.matches(" selected=true").count(), 1);
        assert!(!options.contains("owner"));
        assert!(!options.contains("value=\"\""));
        let region = island.split_once("id=\"permission-error\"").unwrap().1;
        let (attributes, content) = region.split_once('>').unwrap();
        assert!(attributes.contains("aria-live=\"polite\""));
        if supported {
            assert!(content.starts_with("</div>"));
        } else {
            assert!(content.starts_with(&format!(
                "<div>{}</div></div>",
                link_form::PERMISSION_UNSUPPORTED
            )));
            assert_eq!(island.matches("aria-invalid=\"true\"").count(), 1);
        }
    }
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
    let region = island.split_once("id=\"description-error\"").unwrap().1;
    let (attributes, content) = region.split_once('>').unwrap();
    assert!(attributes.contains("aria-live=\"polite\""));
    assert!(attributes.contains("text-error"));
    assert!(attributes.contains("text-sm font-medium"));
    assert!(!attributes.contains("hidden"));
    assert!(content.starts_with(&format!(
        "<div>{}</div></div>",
        link_form::DESCRIPTION_REQUIRED
    )));
    let region = island.split_once("id=\"permission-error\"").unwrap().1;
    let (attributes, content) = region.split_once('>').unwrap();
    assert!(attributes.contains("aria-live=\"polite\""));
    assert!(attributes.contains("text-error"));
    assert!(attributes.contains("text-sm font-medium"));
    assert!(!attributes.contains("hidden"));
    assert!(content.starts_with(&format!(
        "<div>{}</div></div>",
        link_form::PERMISSION_UNSUPPORTED
    )));
    assert_numeric_input(
        &island,
        "max_uses",
        "abc",
        Some(link_form::MAX_USES_NOT_POSITIVE),
    );
    assert_numeric_input(
        &island,
        "expires_in_days",
        "0",
        Some(link_form::EXPIRES_IN_DAYS_NOT_POSITIVE),
    );
    assert!(island.contains(&format!(
        "<p id=\"repo_ids-error\" class=\"text-sm font-medium text-error\">{}</p>",
        link_form::REPO_SCOPE_REQUIRED
    )));
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
    assert!(description.contains("required=true"));
    assert!(description.contains("aria-invalid=\"true\""));
    assert!(description.contains("input-error"));
    assert!(description.contains("aria-labelledby=\"description-label\""));
    assert!(description.contains("aria-describedby=\"description-help description-error\""));
    assert!(description.contains("aria-errormessage=\"description-error\""));
    for id in [
        "description",
        "description-label",
        "description-help",
        "description-error",
    ] {
        assert_eq!(island.matches(&format!(" id=\"{id}\"")).count(), 1);
    }
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
