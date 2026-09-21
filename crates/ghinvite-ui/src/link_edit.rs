//! Native invitation-link metadata editing.

use crate::field::{Field, FieldKind};
use crate::layouts::ConsoleLayout;
use crate::link_form;
use dioform_core::Form;
use dioform_derive::Form;
use dioxus::prelude::*;
use ghinvite_core::{Description, InternalNote, InvitationLinkId};
use serde::{Deserialize, Serialize};

/// Submitted metadata, kept verbatim for redisplay after a failed save.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Form)]
#[form(crate = "::dioform_core")]
pub struct LinkEditValues {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub internal_note: String,
}

/// Field-level messages for a failed metadata save.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkEditErrors {
    pub description: Option<String>,
    pub internal_note: Option<String>,
}

/// Validate with the creation metadata rules and return trimmed metadata.
/// Blank notes become absent; raw values remain unchanged for failed saves.
pub fn validate(values: &LinkEditValues) -> Result<(String, Option<String>), LinkEditErrors> {
    match (
        Description::parse(&values.description),
        InternalNote::parse(&values.internal_note),
    ) {
        (Ok(description), Ok(note)) => Ok((description.into(), note.map(String::from))),
        (description, note) => Err(LinkEditErrors {
            description: description
                .err()
                .map(|error| link_form::description_message(error).to_string()),
            internal_note: note
                .err()
                .map(|_| link_form::INTERNAL_NOTE_TOO_LONG.to_string()),
        }),
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct LinkEditPageProps {
    pub signed_in_login: Option<String>,
    pub account_login: String,
    pub link_id: InvitationLinkId,
    pub values: LinkEditValues,
    pub description_error: Option<String>,
    #[props(default)]
    pub internal_note_error: Option<String>,
    pub form_error: Option<String>,
}

#[component]
pub fn LinkEditPage(props: LinkEditPageProps) -> Element {
    let fields = LinkEditValues::fields();
    let login = &props.account_login;
    let detail_href = format!("/console/accounts/{login}/links/{}", props.link_id);

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Edit invitation link - {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("links".to_string()),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-2",
                    p { class: "text-sm font-medium text-primary", "Invitation links" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "Edit invitation link details" }
                    p { class: "max-w-2xl text-sm leading-6 text-base-content/70",
                        "Update the description and internal note. These details are visible only to admins."
                    }
                }
                form { method: "post", action: "{detail_href}/edit", class: "max-w-3xl space-y-5",
                    crate::csrf::CsrfField {}
                    if props.description_error.is_some() || props.internal_note_error.is_some() || props.form_error.is_some() {
                        div { id: "link-edit-errors", class: "alert alert-error items-start", role: "alert", aria_live: "polite",
                            div {
                                h2 { class: "font-semibold", "Unable to save invitation link details" }
                                ul { class: "mt-1 list-disc space-y-1 pl-5 text-sm",
                                    if let Some(message) = &props.form_error {
                                        li { "{message}" }
                                    }
                                    if let Some(message) = &props.description_error {
                                        li { a { class: "link", href: "#description", "{message}" } }
                                    }
                                    if let Some(message) = &props.internal_note_error {
                                        li { a { class: "link", href: "#internal_note", "{message}" } }
                                    }
                                }
                            }
                        }
                    }
                    section { class: "mac-panel",
                        div { class: "space-y-4 p-4",
                            h2 { class: "text-base font-semibold", "Link details" }
                            Field {
                                id: "description",
                                name: fields.description().field_name().to_string(),
                                label: "Description",
                                kind: FieldKind::Text { maxlength: None },
                                value: props.values.description.clone(),
                                required: true,
                                help: "Visible only to admins. Use a short purpose or audience for this invitation link, up to 120 characters.",
                                error: props.description_error.clone(),
                            }
                            Field {
                                id: "internal_note",
                                name: fields.internal_note().field_name().to_string(),
                                label: "Internal note",
                                kind: FieldKind::Textarea { rows: Some(4) },
                                value: props.values.internal_note.clone(),
                                help: "Optional admin-only notes. Not visible in the invitation request flow. Leave blank to clear the note.",
                                error: props.internal_note_error.clone(),
                            }
                        }
                    }
                    p { class: "text-sm text-base-content/70",
                        "Guardrails are fixed when the invitation link is created. To change max use, expiration, permission level, repository scope, or approval policy, create a new invitation link. Editing details does not reactivate an inactive link."
                    }
                    div { class: "flex justify-end gap-3",
                        a { class: "btn btn-ghost", href: "{detail_href}", "Cancel" }
                        button { r#type: "submit", class: "btn btn-primary", "Save changes" }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_description_uses_the_trimmed_unicode_limit_not_native_utf16_maxlength() {
        for count in [120, 121] {
            let description = format!("  {}  ", "\u{1f680}".repeat(count));
            let values = LinkEditValues {
                description: description.clone(),
                ..LinkEditValues::default()
            };
            let result = validate(&values);
            let html = crate::testing::render(move || {
                rsx! {
                    LinkEditPage {
                        signed_in_login: Some("admin".to_string()),
                        account_login: "acme",
                        link_id: InvitationLinkId::new(),
                        values: values.clone(),
                        description_error: result.clone().err().and_then(|errors| errors.description),
                        form_error: None,
                    }
                }
            });

            assert!(html.contains(&format!("value=\"{description}\"")));
            assert!(html.contains("required=true"));
            assert!(!html.contains("maxlength="));
            assert!(html.contains("120 characters"));
            if count == 120 {
                assert!(!html.contains("aria-invalid"));
                assert!(!html.contains("link-edit-errors"));
            } else {
                assert!(html.contains("aria-invalid=\"true\""));
                assert!(html.contains("aria-describedby=\"description-help description-error\""));
                assert!(html.contains(
                    "<p id=\"description-error\" class=\"text-sm font-medium text-error\">Description must be 120 characters or fewer.</p>"
                ));
            }
        }
    }

    #[test]
    fn service_failure_preserves_raw_input_without_marking_description_invalid() {
        let html = crate::testing::render(|| {
            rsx! {
                LinkEditPage {
                    signed_in_login: Some("admin".to_string()),
                    account_login: "acme",
                    link_id: InvitationLinkId::new(),
                    values: LinkEditValues {
                        description: "  <Workshop>  ".into(),
                        internal_note: "  First cohort\nKeep this note  ".into(),
                    },
                    description_error: None,
                    form_error: Some("Unable to save. Please try again.".to_string()),
                }
            }
        });

        assert!(html.contains("<li>Unable to save. Please try again.</li>"));
        assert!(html.contains("role=\"alert\" aria-live=\"polite\""));
        assert!(html.contains("value=\"  &#60;Workshop&#62;  \""));
        assert!(html.contains(">  First cohort\nKeep this note  </textarea>"));
        assert!(!html.contains("aria-invalid"));
        assert!(!html.contains("description-error"));
        assert!(html.contains("aria-describedby=\"description-help\""));
    }

    #[test]
    fn validated_edits_render_trimmed_metadata_and_cleared_notes() {
        for (raw_note, expected_note) in [
            (
                "  First cohort\n  Keep indentation  \n",
                "First cohort\n  Keep indentation",
            ),
            (" \t\n ", ""),
        ] {
            let values = LinkEditValues {
                description: "  Updated workshop  ".into(),
                internal_note: raw_note.into(),
            };
            let (description, internal_note) = validate(&values).expect("valid metadata");
            let html = crate::testing::render(move || {
                rsx! {
                    LinkEditPage {
                        signed_in_login: Some("admin".to_string()),
                        account_login: "acme",
                        link_id: InvitationLinkId::new(),
                        values: LinkEditValues {
                            description: description.clone(),
                            internal_note: internal_note.clone().unwrap_or_default(),
                        },
                        description_error: None,
                        form_error: None,
                    }
                }
            });

            assert!(html.contains("value=\"Updated workshop\""));
            assert!(html.contains(&format!(">{expected_note}</textarea>")));
            assert!(!html.contains("link-edit-errors"));
            assert!(!html.contains("aria-invalid"));
        }
    }

    #[test]
    fn invalid_edits_preserve_raw_values_and_associate_description_errors() {
        for (description, message) in [
            (
                "   ".to_string(),
                "Description is required. Use short, single-line admin-only context for this invitation link.",
            ),
            (
                "Workshop\nnext cohort".to_string(),
                "Description must be a single line.",
            ),
            (
                "Workshop\r".to_string(),
                "Description must be a single line.",
            ),
            (
                "x".repeat(121),
                "Description must be 120 characters or fewer.",
            ),
        ] {
            let values = LinkEditValues {
                description: description.clone(),
                internal_note: "  Keep <this>\n note  ".into(),
            };
            let error = validate(&values)
                .expect_err("invalid description must fail")
                .description
                .expect("description message");
            let html = crate::testing::render(move || {
                rsx! {
                    LinkEditPage {
                        signed_in_login: Some("admin".to_string()),
                        account_login: "acme",
                        link_id: InvitationLinkId::new(),
                        values: values.clone(),
                        description_error: Some(error.clone()),
                        form_error: None,
                    }
                }
            });

            assert!(html.contains(&format!("value=\"{description}\"")));
            assert!(html.contains(">  Keep &#60;this&#62;\n note  </textarea>"));
            assert!(html.contains("aria-invalid=\"true\""));
            assert!(html.contains("aria-describedby=\"description-help description-error\""));
            assert!(html.contains(&format!(
                "<p id=\"description-error\" class=\"text-sm font-medium text-error\">{message}</p>"
            )));
            assert!(html.contains("id=\"link-edit-errors\""));
            assert!(html.contains("role=\"alert\" aria-live=\"polite\""));
            assert!(html.contains(&format!("href=\"#description\">{message}</a>")));
        }
    }

    #[test]
    fn internal_note_over_16384_bytes_is_a_field_error_with_raw_values_preserved() {
        let at_limit = LinkEditValues {
            description: "Workshop".into(),
            internal_note: format!("  {}  ", "x".repeat(16_384)),
        };
        assert!(validate(&at_limit).is_ok());

        let values = LinkEditValues {
            description: "   ".into(),
            internal_note: "x".repeat(16_385),
        };
        let errors = validate(&values).expect_err("oversized note must fail");
        assert_eq!(
            errors.internal_note.as_deref(),
            Some("Internal note must be 16384 UTF-8 bytes or fewer.")
        );
        assert!(errors.description.is_some(), "every field reports at once");
        let html = crate::testing::render(move || {
            rsx! {
                LinkEditPage {
                    signed_in_login: Some("admin".to_string()),
                    account_login: "acme",
                    link_id: InvitationLinkId::new(),
                    values: values.clone(),
                    description_error: errors.description.clone(),
                    internal_note_error: errors.internal_note.clone(),
                    form_error: None,
                }
            }
        });

        assert!(html.contains(&format!(">{}</textarea>", "x".repeat(16_385))));
        assert!(html.contains("aria-describedby=\"internal_note-help internal_note-error\""));
        assert!(html.contains(
            "<p id=\"internal_note-error\" class=\"text-sm font-medium text-error\">Internal note must be 16384 UTF-8 bytes or fewer.</p>"
        ));
        assert!(html.contains(
            "href=\"#internal_note\">Internal note must be 16384 UTF-8 bytes or fewer.</a>"
        ));
    }

    #[test]
    fn edit_page_prefills_only_metadata_in_a_native_form_with_cancel_and_current_links() {
        let link_id = InvitationLinkId::new();
        let html = crate::testing::render(move || {
            rsx! {
                LinkEditPage {
                    signed_in_login: Some("admin".to_string()),
                    account_login: "acme",
                    link_id,
                    values: LinkEditValues {
                        description: "AI coding workshop".into(),
                        internal_note: "Contractor onboarding\nSecond cohort".into(),
                    },
                    description_error: None,
                    form_error: None,
                }
            }
        });

        assert!(html.contains(&format!(
            "<form method=\"post\" action=\"/console/accounts/acme/links/{link_id}/edit\""
        )));
        assert_eq!(
            html.matches(&format!(
                "action=\"/console/accounts/acme/links/{link_id}/edit\""
            ))
            .count(),
            1
        );
        assert!(html.contains("for=\"description\""));
        assert!(html.contains("<input id=\"description\" type=\"text\" name=\"description\" value=\"AI coding workshop\""));
        assert!(html.contains("required=true"));
        assert!(!html.contains("maxlength="));
        assert!(html.contains("aria-describedby=\"description-help\""));
        assert!(html.contains("for=\"internal_note\""));
        assert!(html.contains("<textarea id=\"internal_note\" name=\"internal_note\""));
        assert!(html.contains(">Contractor onboarding\nSecond cohort</textarea>"));
        assert!(html.contains("aria-describedby=\"internal_note-help\""));
        assert!(
            html.contains(
                "<button type=\"submit\" class=\"btn btn-primary\">Save changes</button>"
            )
        );
        assert!(html.contains(&format!(
            "href=\"/console/accounts/acme/links/{link_id}\">Cancel</a>"
        )));
        assert_eq!(
            html.matches("href=\"/console/accounts/acme/links\" aria-current=\"page\">Links</a>")
                .count(),
            2
        );
        assert!(html.contains("Guardrails are fixed"));
        assert!(html.contains("create a new invitation link"));
        assert!(!html.contains("aria-invalid"));
        assert!(!html.contains("link-edit-errors"));
        assert!(!html.contains("onsubmit"));
        assert!(!html.contains("ghinvite-island"));
        for field in [
            "permission",
            "repo_ids",
            "approval_required",
            "max_uses",
            "expires_in_days",
            "slug",
            "uses_count",
            "revoked_at",
        ] {
            assert!(!html.contains(&format!("name=\"{field}\"")));
        }
    }
}
