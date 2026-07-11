//! Invitation-link views: create form + detail page.

use crate::session::Flash;
use crate::views::layouts::ConsoleLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use ghinvite_core::{InvitationLink, Permission};
use ghinvite_github::payloads::GhRepo;

#[derive(Clone, PartialEq, Props)]
pub struct LinkCreateFormProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    pub repos: Vec<GhRepo>,
    pub form: LinkFormValues,
}

#[derive(Clone, PartialEq)]
pub struct LinkFormValues {
    pub description: String,
    pub permission: String,
    pub approval_required: bool,
    pub max_uses: String,
    pub expires_in_days: String,
    pub internal_note: String,
    pub selected_repo_ids: Vec<u64>,
    pub errors: LinkFormErrors,
}

#[derive(Clone, Default, PartialEq)]
pub struct LinkFormErrors {
    pub summary: Vec<String>,
    pub description: Option<String>,
}

impl Default for LinkFormValues {
    fn default() -> Self {
        Self {
            description: String::new(),
            permission: "pull".into(),
            approval_required: false,
            max_uses: String::new(),
            expires_in_days: "30".into(),
            internal_note: String::new(),
            selected_repo_ids: Vec::new(),
            errors: LinkFormErrors::default(),
        }
    }
}

#[component]
pub fn LinkCreateFormPage(props: LinkCreateFormProps) -> Element {
    let login = props.account_login.clone();
    let perms = ["pull", "triage", "push", "maintain", "admin"];
    let has_description_error = props.form.errors.description.is_some();
    let description_class = if has_description_error {
        "input input-bordered input-error w-full"
    } else {
        "input input-bordered w-full"
    };
    let description_described_by = if has_description_error {
        "description-help description-error"
    } else {
        "description-help"
    };

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "New invitation link · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("new-link".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-2",
                    p { class: "text-sm font-medium text-primary", "Invitation links" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "New invitation link" }
                    p { class: "max-w-2xl text-sm leading-6 text-base-content/70",
                        "Create a controlled invitation link that lets GitHub users request repository access to selected repositories."
                    }
                }
                form { method: "post", action: "/console/accounts/{login}/links", class: "max-w-3xl space-y-5",
                    {if props.form.errors.summary.is_empty() {
                        rsx! {}
                    } else {
                        rsx! {
                            div { id: "link-form-errors", class: "alert alert-error items-start", role: "alert", aria_live: "polite",
                                div {
                                    h2 { class: "font-semibold", dangerous_inner_html: "We couldn't create this invitation link" }
                                    ul { class: "mt-1 list-disc space-y-1 pl-5 text-sm",
                                        {props.form.errors.summary.iter().map(|message| rsx! { li { "{message}" } })}
                                    }
                                }
                            }
                        }
                    }}
                    section { class: "mac-panel",
                        div { class: "space-y-4 p-4",
                            div {
                                h2 { class: "text-base font-semibold", "Link details" }
                                p { class: "mt-1 text-sm text-base-content/65", "Name the admin purpose for this invitation link and keep optional notes separate." }
                            }
                            div { class: "form-control gap-2",
                                label { class: "label", r#for: "description", span { class: "label-text font-medium", "Description" } }
                                {if has_description_error {
                                    rsx! {
                                        input { id: "description", r#type: "text", name: "description", value: "{props.form.description}", class: "{description_class}", required: true, maxlength: "120", placeholder: "AI coding workshop", aria_invalid: "true", aria_describedby: "{description_described_by}" }
                                    }
                                } else {
                                    rsx! {
                                        input { id: "description", r#type: "text", name: "description", value: "{props.form.description}", class: "{description_class}", required: true, maxlength: "120", placeholder: "AI coding workshop", aria_describedby: "{description_described_by}" }
                                    }
                                }}
                                p { id: "description-help", class: "text-sm text-base-content/65", "Visible only to admins. Use a short purpose or audience for this invitation link." }
                                {if let Some(message) = props.form.errors.description.as_ref() {
                                    rsx! { p { id: "description-error", class: "text-sm font-medium text-error", "{message}" } }
                                } else {
                                    rsx! {}
                                }}
                            }
                            div { class: "form-control gap-2",
                                label { class: "label", r#for: "internal_note", span { class: "label-text font-medium", "Internal note" } }
                                textarea { id: "internal_note", name: "internal_note", class: "textarea textarea-bordered w-full", placeholder: "Why this link exists", "{props.form.internal_note}" }
                                p { class: "text-sm text-base-content/65", "Optional admin-only notes. Not visible in the invitation request flow." }
                            }
                        }
                    }
                    section { class: "mac-panel",
                        div { class: "space-y-4 p-4",
                            div {
                                h2 { class: "text-base font-semibold", "Access configuration" }
                                p { class: "mt-1 text-sm text-base-content/65", "Choose the GitHub permission level and repositories included in this invitation link." }
                            }
                            div { class: "form-control gap-2",
                                label { class: "label", r#for: "permission", span { class: "label-text font-medium", "Permission level" } }
                                select { id: "permission", name: "permission", class: "select select-bordered w-full",
                                    {perms.iter().map(|p| {
                                        let selected = props.form.permission == *p;
                                        rsx! { option { value: "{p}", selected: selected, "{p}" } }
                                    })}
                                }
                                p { class: "text-sm text-base-content/65", "Use pull for read-only access. Maintain and admin can change repository settings." }
                            }
                            div { class: "alert alert-warning shadow-sm",
                                span { "Review elevated permissions before sharing. Approved invitation requests send GitHub invitations." }
                            }
                        }
                    }
                    section { class: "mac-panel",
                        div { class: "space-y-4 p-4",
                            div {
                                h2 { class: "text-base font-semibold", "Request handling" }
                                p { class: "mt-1 text-sm text-base-content/65", "Set approval, usage, and expiration guardrails." }
                            }
                            div { class: "form-control",
                                label { class: "label cursor-pointer justify-start gap-3",
                                    input { r#type: "checkbox", name: "approval_required", value: "true", checked: props.form.approval_required, class: "checkbox" }
                                    span { class: "label-text", "Require account admin approval before GitHub invitations are sent" }
                                }
                                p { class: "text-sm text-base-content/65", "Leave unchecked to auto-approve invitation requests that use this invitation link." }
                            }
                            div { class: "grid grid-cols-1 gap-4 md:grid-cols-2",
                                div { class: "form-control gap-2",
                                    label { class: "label", r#for: "max_uses", span { class: "label-text font-medium", "Max use" } }
                                    input { id: "max_uses", r#type: "number", name: "max_uses", value: "{props.form.max_uses}", class: "input input-bordered w-full", min: "1", placeholder: "Unlimited" }
                                    p { class: "text-sm text-base-content/65", "Blank means unlimited invitation requests." }
                                }
                                div { class: "form-control gap-2",
                                    label { class: "label", r#for: "expires_in_days", span { class: "label-text font-medium", "Expires in days" } }
                                    input { id: "expires_in_days", r#type: "number", name: "expires_in_days", value: "{props.form.expires_in_days}", class: "input input-bordered w-full", min: "1" }
                                    p { class: "text-sm text-base-content/65", "Default is 30 days. Blank creates an invitation link with no expiration." }
                                }
                            }
                        }
                    }
                    section { class: "mac-panel",
                        div { class: "space-y-4 p-4",
                            div {
                                h2 { class: "text-base font-semibold", "Repository scope" }
                                p { class: "mt-1 text-sm text-base-content/65", "Select every repository this invitation link may grant access to." }
                            }
                            {if props.repos.is_empty() {
                                rsx! { div { class: "alert shadow-sm", span { "No repositories are available for this installation." } } }
                            } else {
                                rsx! {
                                    div { class: "max-h-80 space-y-1 overflow-y-auto rounded-box border border-base-300 bg-base-200 p-3",
                                        {props.repos.iter().map(|repo| {
                                            let id = repo.id;
                                            let checked = props.form.selected_repo_ids.contains(&id);
                                            let full_name = repo.full_name.clone();
                                            rsx! {
                                                label { class: "repo-choice-row flex cursor-pointer items-center gap-3 rounded-lg px-2 py-1.5 text-sm hover:bg-base-100",
                                                    input { r#type: "checkbox", name: "repo_ids", value: "{id}", checked: checked, class: "checkbox checkbox-sm" }
                                                    span { class: "text-sm", "{full_name}" }
                                                }
                                            }
                                        })}
                                    }
                                }
                            }}
                        }
                    }
                    div { class: "flex justify-end",
                        button { r#type: "submit", class: "btn btn-primary", "Create invitation link" }
                    }
                }
            },
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct LinkDetailProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    pub link: InvitationLink,
    pub now: DateTime<Utc>,
    pub invitation_url: String,
}

#[component]
pub fn LinkDetailPage(props: LinkDetailProps) -> Element {
    let login = props.account_login.clone();
    let id_str = props.link.id.to_string();
    let slug = props.link.slug.as_str().to_string();
    let description = props.link.description.clone();
    let active = props.link.is_active(props.now);
    let badge_class = if active {
        "badge badge-success"
    } else {
        "badge badge-ghost"
    };
    let badge_label = if active { "active" } else { "inactive" };
    let perm = match props.link.permission {
        Permission::Pull => "pull",
        Permission::Triage => "triage",
        Permission::Push => "push",
        Permission::Maintain => "maintain",
        Permission::Admin => "admin",
    };
    let uses = props
        .link
        .max_uses
        .map(|n| format!("{} / {}", props.link.uses_count, n))
        .unwrap_or_else(|| format!("{} / unlimited", props.link.uses_count));
    let expires = props
        .link
        .expires_at
        .map(|when| when.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "No expiration".into());
    let approval = if props.link.approval_required {
        "Account admin approval required"
    } else {
        "Requests are auto-approved"
    };
    let preview_href = props.invitation_url.clone();
    let repos = props.link.repos.iter().map(|repo| {
        let full_name = repo.repo_full_name.clone();
        rsx! { li { "{full_name}" } }
    });

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "{description} · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("overview".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-3 md:flex-row md:items-center md:justify-between",
                    div {
                        h1 { class: "text-2xl font-bold", "{description}" }
                        p { class: "text-sm text-base-content/70", "Invitation code: ", span { class: "font-mono", "{slug}" } }
                    }
                    span { class: "{badge_class}", "{badge_label}" }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Invitation link" }
                        p { class: "mt-0.5 text-xs text-base-content/60", "Share this invitation link with GitHub users who should request access." }
                    }
                    div { class: "p-4",
                        input {
                            class: "input input-bordered input-sm w-full font-mono text-xs",
                            readonly: true,
                            value: "{props.invitation_url}",
                            aria_label: "Invitation link",
                        }
                        div { class: "mt-3",
                            a { class: "btn btn-outline btn-sm h-8 min-h-0", href: "{preview_href}", "Open invitation request flow" }
                        }
                    }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    dl { class: "property-list",
                        div { class: "property-row",
                            dt { class: "property-label", "Permission" }
                            dd { class: "text-sm", "{perm}" }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Uses" }
                            dd { class: "text-sm tabular-nums", "{uses}" }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Expiration" }
                            dd { class: "text-sm", "{expires}" }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Approval" }
                            dd { class: "text-sm", "{approval}" }
                        }
                    }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Repositories" }
                    }
                    div { class: "p-4",
                        ul { class: "space-y-1 text-sm", {repos} }
                    }
                }
                {match props.link.internal_note.as_deref() {
                    Some(note) => rsx! {
                        section { class: "mac-panel mb-4 overflow-hidden",
                            div { class: "border-b border-base-300 px-4 py-3",
                                h2 { class: "text-sm font-semibold", "Internal note" }
                            }
                            div { class: "p-4",
                                p { class: "text-sm text-base-content/80", "{note}" }
                            }
                        }
                    },
                    None => rsx! {},
                }}
                {if active {
                    rsx! {
                        section { class: "mac-panel border-error/30 bg-base-100",
                            div { class: "card-body gap-4",
                                div {
                                    h2 { class: "text-base font-semibold text-error", "Stop accepting new invitation requests" }
                                    p { class: "mt-1 text-sm text-base-content/70",
                                        "This stops new invitation requests through this invitation link. It does not cancel existing invitation requests or GitHub invitations."
                                    }
                                }
                                a { class: "btn btn-error w-fit", href: "#stop-link-modal", "Stop accepting new requests" }
                            }
                        }
                        div { class: "modal", role: "dialog", id: "stop-link-modal", aria_labelledby: "stop-link-modal-title", aria_modal: "true",
                            div { class: "modal-box",
                                h3 { id: "stop-link-modal-title", class: "text-lg font-semibold", "Confirm stop" }
                                p { class: "mt-2 text-sm text-base-content/70",
                                    "GitHub users will no longer be able to create invitation requests from this invitation link. Existing invitation requests and GitHub invitations continue."
                                }
                                div { class: "modal-action",
                                    a { class: "btn btn-ghost", href: "#", "Cancel" }
                                    form { method: "post", action: "/console/accounts/{login}/links/{id_str}/revoke",
                                        button { r#type: "submit", class: "btn btn-error", "Confirm stop" }
                                    }
                                }
                            }
                            a { class: "modal-backdrop", href: "#", "Close" }
                        }
                    }
                } else {
                    rsx! { p { class: "text-base-content/70", "This invitation link is no longer accepting invitation requests." } }
                }}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use ghinvite_core::{InvitationLink, InvitationLinkId, InvitationLinkRepo, Permission, Slug};

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_link() -> InvitationLink {
        InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::from_string("abcdEFGH01234567".to_string()).unwrap(),
            installation_id: 1,
            account_id: 9001,
            created_by: 701,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: Some(dt("2026-06-03T12:00:00Z")),
            max_uses: Some(5),
            uses_count: 2,
            permission: Permission::Push,
            approval_required: true,
            description: "AI coding workshop".into(),
            internal_note: Some("Contractor onboarding".into()),
            revoked_at: None,
            revoked_by: None,
            repos: vec![
                InvitationLinkRepo {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                },
                InvitationLinkRepo {
                    repo_id: 11,
                    repo_full_name: "acme/web".into(),
                },
            ],
        }
    }

    #[test]
    fn link_form_defaults_are_safe() {
        let form = LinkFormValues::default();

        assert_eq!(form.permission, "pull");
        assert_eq!(form.expires_in_days, "30");
        assert_eq!(form.max_uses, "");
        assert!(!form.approval_required);
    }

    #[test]
    fn link_create_form_renders_sectioned_console_form() {
        let html = crate::views::render::render(|| {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![ghinvite_github::payloads::GhRepo {
                        id: 10,
                        full_name: "acme/api".to_string(),
                        private: true,
                    }],
                    form: LinkFormValues::default(),
                }
            }
        });

        assert!(html.contains("Access configuration"));
        assert!(html.contains("Link details"));
        assert!(html.contains("name=\"description\""));
        assert!(html.contains("required"));
        assert!(html.contains("maxlength=\"120\""));
        assert!(html.contains("AI coding workshop"));
        assert!(html.find("Link details").unwrap() < html.find("Access configuration").unwrap());
        assert!(html.contains("<title>New invitation link · acme</title>"));
        assert!(!html.contains("{props.account_login}"));
        assert!(html.contains("Create a controlled invitation link"));
        assert!(html.contains("Approved invitation requests send GitHub invitations"));
        assert!(html.contains("Require account admin approval before GitHub invitations are sent"));
        assert!(html.contains("Blank means unlimited invitation requests"));
        assert!(html.contains("Create invitation link"));
        assert!(!html.contains("collaborator access"));
        assert!(!html.contains("GitHub collaborator invitations"));
        assert!(!html.contains("before invitations are sent"));
        assert!(html.contains("Max use"));
        assert!(!html.contains("Max uses"));
        assert!(html.contains("Request handling"));
        assert!(html.contains("Repository scope"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("repo-choice-row"));
    }

    #[test]
    fn link_create_form_renders_description_validation_errors_and_preserved_values() {
        let form = LinkFormValues {
            description: "   ".to_string(),
            permission: "push".to_string(),
            approval_required: true,
            max_uses: "7".to_string(),
            expires_in_days: "45".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![10],
            errors: LinkFormErrors {
                summary: vec!["Fix the highlighted fields before creating this invitation link."
                    .to_string()],
                description: Some("Description is required. Use short, single-line admin-only context for this invitation link.".to_string()),
            },
        };

        let html = crate::views::render::render(move || {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![
                        ghinvite_github::payloads::GhRepo { id: 10, full_name: "acme/api".to_string(), private: true },
                        ghinvite_github::payloads::GhRepo { id: 11, full_name: "acme/web".to_string(), private: true },
                    ],
                    form: form.clone(),
                }
            }
        });

        assert!(html.contains("Fix the highlighted fields before creating this invitation link."));
        assert!(html.contains("Description is required. Use short, single-line admin-only context for this invitation link."));
        assert!(html.contains("id=\"link-form-errors\""));
        assert!(html.contains("role=\"alert\""));
        assert!(html.contains("aria-live=\"polite\""));
        assert!(html.contains("id=\"description-error\""));
        assert!(html.contains("aria-invalid=\"true\""));
        assert!(html.contains("aria-describedby=\"description-help description-error\""));
        assert!(html.contains("value=\"push\" selected"));
        assert!(html.contains("name=\"approval_required\" value=\"true\" checked"));
        assert!(html.contains("name=\"max_uses\" value=\"7\""));
        assert!(html.contains("name=\"expires_in_days\" value=\"45\""));
        assert!(html.contains("Keep this note"));
        assert!(html.contains("value=\"10\" checked"));
        assert!(html.contains("value=\"11\""));
    }

    #[test]
    fn link_detail_renders_operational_context() {
        let link = sample_link();
        let html = crate::views::render::render(move || {
            rsx! {
                LinkDetailPage {
                    signed_in_login: Some("admin".into()),
                    flash: None,
                    account_login: String::from("acme"),
                    link: link.clone(),
                    now: dt("2026-05-05T12:00:00Z"),
                    invitation_url: String::from("http://127.0.0.1:8787/i/abcdEFGH01234567"),
                }
            }
        });

        assert!(html.contains("Open invitation request flow"));
        assert!(html.contains("Share this invitation link with GitHub users"));
        assert!(!html.contains("Invitation URL"));
        assert!(!html.contains("recipient preview"));
        assert!(html.contains("<title>AI coding workshop · acme</title>"));
        assert!(html.contains("<h1 class=\"text-2xl font-bold\">AI coding workshop</h1>"));
        assert!(html.contains("Invitation code"));
        assert!(html.contains("abcdEFGH01234567"));
        assert!(!html.contains("{props.account_login}"));
        assert!(html.contains("Stop accepting new invitation requests"));
        assert!(html.contains("Existing invitation requests and GitHub invitations continue"));
        assert!(!html.contains("Existing requests and invitations continue"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(html.contains("push"));
        assert!(html.contains("2 / 5"));
        assert!(html.contains("Contractor onboarding"));
        assert!(html.contains("id=\"stop-link-modal\""));
        assert!(html.contains("role=\"dialog\""));
        assert!(html.contains("aria-labelledby=\"stop-link-modal-title\""));
        assert!(html.contains("aria-modal=\"true\""));
        assert!(html.contains("id=\"stop-link-modal-title\""));
        assert!(html.contains("Confirm stop"));
        assert!(html.contains("property-list"));
        assert!(html.contains("property-row"));
        assert!(html.contains("mac-panel"));
        assert!(!html.contains("md:grid-cols-2 gap-4"));
        assert!(!html.contains(concat!("Revoke this ", "link")));
    }
}
