//! Share-link views: create form + detail page.

use crate::session::Flash;
use crate::views::layouts::ConsoleLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use domain::{Permission, ShareLink};
use github::payloads::GhRepo;

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
    pub permission: String,
    pub approval_required: bool,
    pub max_uses: String,
    pub expires_in_days: String,
    pub internal_note: String,
    pub selected_repo_ids: Vec<u64>,
}

impl Default for LinkFormValues {
    fn default() -> Self {
        Self {
            permission: "pull".into(),
            approval_required: false,
            max_uses: String::new(),
            expires_in_days: "30".into(),
            internal_note: String::new(),
            selected_repo_ids: Vec::new(),
        }
    }
}

#[component]
pub fn LinkCreateFormPage(props: LinkCreateFormProps) -> Element {
    let login = props.account_login.clone();
    let perms = ["pull", "triage", "push", "maintain", "admin"];

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "New share link · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("new-link".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-2",
                    p { class: "text-sm font-medium text-primary", "Share links" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "New share link" }
                    p { class: "max-w-2xl text-sm leading-6 text-base-content/70",
                        "Create a controlled URL that lets GitHub users request collaborator access to selected repositories."
                    }
                }
                form { method: "post", action: "/accounts/{login}/links", class: "max-w-3xl space-y-5",
                    section { class: "mac-panel",
                        div { class: "space-y-4 p-4",
                            div {
                                h2 { class: "text-base font-semibold", "Access configuration" }
                                p { class: "mt-1 text-sm text-base-content/65", "Choose the GitHub permission level and the repositories this link can request." }
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
                                span { "Review elevated permissions before sharing. Approved requests send GitHub collaborator invitations." }
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
                                    span { class: "label-text", "Require admin approval before invitations are sent" }
                                }
                                p { class: "text-sm text-base-content/65", "Leave unchecked to auto-approve requests that use this link." }
                            }
                            div { class: "grid grid-cols-1 gap-4 md:grid-cols-2",
                                div { class: "form-control gap-2",
                                    label { class: "label", r#for: "max_uses", span { class: "label-text font-medium", "Max uses" } }
                                    input { id: "max_uses", r#type: "number", name: "max_uses", value: "{props.form.max_uses}", class: "input input-bordered w-full", min: "1", placeholder: "Unlimited" }
                                    p { class: "text-sm text-base-content/65", "Blank means unlimited requests." }
                                }
                                div { class: "form-control gap-2",
                                    label { class: "label", r#for: "expires_in_days", span { class: "label-text font-medium", "Expires in days" } }
                                    input { id: "expires_in_days", r#type: "number", name: "expires_in_days", value: "{props.form.expires_in_days}", class: "input input-bordered w-full", min: "1" }
                                    p { class: "text-sm text-base-content/65", "Default is 30 days. Blank creates a link with no expiration." }
                                }
                            }
                            div { class: "form-control gap-2",
                                label { class: "label", r#for: "internal_note", span { class: "label-text font-medium", "Internal note" } }
                                textarea { id: "internal_note", name: "internal_note", class: "textarea textarea-bordered w-full", placeholder: "Why this link exists", "{props.form.internal_note}" }
                                p { class: "text-sm text-base-content/65", "Visible only to admins." }
                            }
                        }
                    }
                    section { class: "mac-panel",
                        div { class: "space-y-4 p-4",
                            div {
                                h2 { class: "text-base font-semibold", "Repository scope" }
                                p { class: "mt-1 text-sm text-base-content/65", "Select every repository this link may grant access to." }
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
                        button { r#type: "submit", class: "btn btn-primary", "Create link" }
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
    pub link: ShareLink,
    pub now: DateTime<Utc>,
    pub share_url: String,
}

#[component]
pub fn LinkDetailPage(props: LinkDetailProps) -> Element {
    let login = props.account_login.clone();
    let id_str = props.link.id.to_string();
    let slug = props.link.slug.as_str().to_string();
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
        "Admin approval required"
    } else {
        "Requests are auto-approved"
    };
    let preview_href = props.share_url.clone();
    let repos = props.link.repos.iter().map(|repo| {
        let full_name = repo.repo_full_name.clone();
        rsx! { li { "{full_name}" } }
    });

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "{slug} · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("overview".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-3 md:flex-row md:items-center md:justify-between",
                    div {
                        h1 { class: "text-2xl font-bold", "{slug}" }
                        p { class: "text-sm text-base-content/70", "Share link details and controls" }
                    }
                    span { class: "{badge_class}", "{badge_label}" }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Share URL" }
                        p { class: "mt-0.5 text-xs text-base-content/60", "Send this URL to recipients who should request access." }
                    }
                    div { class: "p-4",
                        input {
                            class: "input input-bordered input-sm w-full font-mono text-xs",
                            readonly: true,
                            value: "{props.share_url}",
                            aria_label: "Share URL",
                        }
                        div { class: "mt-3",
                            a { class: "btn btn-outline btn-sm h-8 min-h-0", href: "{preview_href}", "Open recipient preview" }
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
                                    h2 { class: "text-base font-semibold text-error", "Stop accepting new requests" }
                                    p { class: "mt-1 text-sm text-base-content/70",
                                        "This prevents new requests through this link. It does not cancel requests or GitHub invitations already in progress."
                                    }
                                }
                                a { class: "btn btn-error w-fit", href: "#stop-link-modal", "Stop accepting new requests" }
                            }
                        }
                        div { class: "modal", role: "dialog", id: "stop-link-modal", aria_labelledby: "stop-link-modal-title", aria_modal: "true",
                            div { class: "modal-box",
                                h3 { id: "stop-link-modal-title", class: "text-lg font-semibold", "Confirm stop" }
                                p { class: "mt-2 text-sm text-base-content/70",
                                    "Recipients will no longer be able to create new requests from this share link. Existing requests and invitations continue."
                                }
                                div { class: "modal-action",
                                    a { class: "btn btn-ghost", href: "#", "Cancel" }
                                    form { method: "post", action: "/accounts/{login}/links/{id_str}/revoke",
                                        button { r#type: "submit", class: "btn btn-error", "Confirm stop" }
                                    }
                                }
                            }
                            a { class: "modal-backdrop", href: "#", "Close" }
                        }
                    }
                } else {
                    rsx! { p { class: "text-base-content/70", "This link is no longer accepting requests." } }
                }}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use domain::{Permission, ShareLink, ShareLinkId, ShareLinkRepo, Slug};

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_link() -> ShareLink {
        ShareLink {
            id: ShareLinkId::new(),
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
            internal_note: Some("Contractor onboarding".into()),
            revoked_at: None,
            revoked_by: None,
            repos: vec![
                ShareLinkRepo {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                },
                ShareLinkRepo {
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
                    repos: vec![github::payloads::GhRepo {
                        id: 10,
                        full_name: "acme/api".to_string(),
                        private: true,
                    }],
                    form: LinkFormValues::default(),
                }
            }
        });

        assert!(html.contains("Access configuration"));
        assert!(html.contains("<title>New share link · acme</title>"));
        assert!(!html.contains("{props.account_login}"));
        assert!(html.contains("Request handling"));
        assert!(html.contains("Repository scope"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("repo-choice-row"));
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
                    share_url: String::from("http://127.0.0.1:8787/i/abcdEFGH01234567"),
                }
            }
        });

        assert!(html.contains("Open recipient preview"));
        assert!(html.contains("<title>abcdEFGH01234567 · acme</title>"));
        assert!(!html.contains("{props.account_login}"));
        assert!(html.contains("Stop accepting new requests"));
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
