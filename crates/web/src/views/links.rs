//! Share-link views: create form + detail page.

use crate::session::Flash;
use crate::views::layouts::DashboardLayout;
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
        DashboardLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "New share link · {props.account_login}".to_string(),
            account_login: Some(props.account_login.clone()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6",
                    h1 { class: "text-2xl font-bold", "New share link" }
                    p { class: "mt-2 text-sm text-base-content/70 max-w-2xl",
                        "Create a URL that lets a GitHub user request collaborator access to the repositories you choose."
                    }
                }
                form {
                    method: "post",
                    action: "/accounts/{login}/links",
                    class: "space-y-6 max-w-2xl",
                    div { class: "form-control gap-2",
                        label { class: "label", r#for: "permission", span { class: "label-text font-medium", "Permission level" } }
                        select {
                            id: "permission",
                            name: "permission",
                            class: "select select-bordered w-full",
                            {perms.iter().map(|p| {
                                let selected = props.form.permission == *p;
                                rsx! { option { value: "{p}", selected: selected, "{p}" } }
                            })}
                        }
                        p { class: "text-sm text-base-content/70", "Use pull for read-only access. Maintain and admin can change repository settings." }
                    }
                    div { class: "alert alert-warning",
                        span { "Review elevated permissions before sharing. A link can send GitHub collaborator invitations when a request is approved." }
                    }
                    div { class: "form-control",
                        label { class: "label cursor-pointer justify-start gap-3",
                            input {
                                r#type: "checkbox",
                                name: "approval_required",
                                value: "true",
                                checked: props.form.approval_required,
                                class: "checkbox",
                            }
                            span { class: "label-text", "Require admin approval before invitations are sent" }
                        }
                        p { class: "text-sm text-base-content/70", "Leave unchecked to auto-approve requests that use this link." }
                    }
                    div { class: "grid grid-cols-1 md:grid-cols-2 gap-4",
                        div { class: "form-control gap-2",
                            label { class: "label", r#for: "max_uses", span { class: "label-text font-medium", "Max uses" } }
                            input {
                                id: "max_uses",
                                r#type: "number",
                                name: "max_uses",
                                value: "{props.form.max_uses}",
                                class: "input input-bordered w-full",
                                min: "1",
                                placeholder: "Unlimited",
                            }
                            p { class: "text-sm text-base-content/70", "Blank means unlimited requests." }
                        }
                        div { class: "form-control gap-2",
                            label { class: "label", r#for: "expires_in_days", span { class: "label-text font-medium", "Expires in days" } }
                            input {
                                id: "expires_in_days",
                                r#type: "number",
                                name: "expires_in_days",
                                value: "{props.form.expires_in_days}",
                                class: "input input-bordered w-full",
                                min: "1",
                            }
                            p { class: "text-sm text-base-content/70", "Default is 30 days. Blank creates a link with no expiration." }
                        }
                    }
                    div { class: "form-control gap-2",
                        label { class: "label", r#for: "internal_note", span { class: "label-text font-medium", "Internal note" } }
                        textarea {
                            id: "internal_note",
                            name: "internal_note",
                            class: "textarea textarea-bordered w-full",
                            placeholder: "Why this link exists, visible only to admins",
                            "{props.form.internal_note}"
                        }
                        p { class: "text-sm text-base-content/70", "Use notes to explain the audience, project, or expiry reason." }
                    }
                    fieldset { class: "form-control gap-2",
                        legend { class: "label", span { class: "label-text font-medium", "Repositories" } }
                        p { class: "text-sm text-base-content/70", "Select every repository this link may grant access to." }
                        {if props.repos.is_empty() {
                            rsx! { div { class: "alert", span { "No repositories are available for this installation." } } }
                        } else {
                            rsx! {
                                div { class: "space-y-1 max-h-80 overflow-y-auto rounded-box border border-base-300 bg-base-100 p-3",
                                    {props.repos.iter().map(|repo| {
                                        let id = repo.id;
                                        let checked = props.form.selected_repo_ids.contains(&id);
                                        let full_name = repo.full_name.clone();
                                        rsx! {
                                            label { class: "label cursor-pointer justify-start gap-2",
                                                input {
                                                    r#type: "checkbox",
                                                    name: "repo_ids",
                                                    value: "{id}",
                                                    checked: checked,
                                                    class: "checkbox checkbox-sm",
                                                }
                                                span { "{full_name}" }
                                            }
                                        }
                                    })}
                                }
                            }
                        }}
                    }
                    div { class: "form-control pt-2",
                        button {
                            r#type: "submit",
                            class: "btn btn-primary",
                            "Create link"
                        }
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
        DashboardLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "{slug} · {props.account_login}".to_string(),
            account_login: Some(props.account_login.clone()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-3 md:flex-row md:items-center md:justify-between",
                    div {
                        h1 { class: "text-2xl font-bold", "{slug}" }
                        p { class: "text-sm text-base-content/70", "Share link details and controls" }
                    }
                    span { class: "{badge_class}", "{badge_label}" }
                }
                section { class: "card bg-base-100 shadow mb-6",
                    div { class: "card-body gap-4",
                        div {
                            h2 { class: "card-title", "Share URL" }
                            p { class: "text-sm text-base-content/70", "Send this URL to recipients who should request access." }
                        }
                        input {
                            class: "input input-bordered font-mono text-sm w-full",
                            readonly: true,
                            value: "{props.share_url}",
                            aria_label: "Share URL",
                        }
                        div { class: "card-actions justify-start",
                            a { class: "btn btn-outline", href: "{preview_href}", "Open recipient preview" }
                        }
                    }
                }
                section { class: "grid grid-cols-1 md:grid-cols-2 gap-4 mb-6",
                    div { class: "card bg-base-100 shadow", div { class: "card-body",
                        h3 { class: "font-semibold", "Permission" }
                        p { "{perm}" }
                    }}
                    div { class: "card bg-base-100 shadow", div { class: "card-body",
                        h3 { class: "font-semibold", "Uses" }
                        p { "{uses}" }
                    }}
                    div { class: "card bg-base-100 shadow", div { class: "card-body",
                        h3 { class: "font-semibold", "Expiration" }
                        p { "{expires}" }
                    }}
                    div { class: "card bg-base-100 shadow", div { class: "card-body",
                        h3 { class: "font-semibold", "Approval" }
                        p { "{approval}" }
                    }}
                }
                section { class: "card bg-base-100 shadow mb-6",
                    div { class: "card-body",
                        h2 { class: "card-title", "Repositories" }
                        ul { class: "list-disc list-inside space-y-1", {repos} }
                    }
                }
                {match props.link.internal_note.as_deref() {
                    Some(note) => rsx! {
                        section { class: "card bg-base-100 shadow mb-6",
                            div { class: "card-body",
                                h2 { class: "card-title", "Internal note" }
                                p { "{note}" }
                            }
                        }
                    },
                    None => rsx! {},
                }}
                {if active {
                    rsx! {
                        section { class: "card bg-base-100 border border-error/30",
                            div { class: "card-body",
                                h2 { class: "card-title text-error", "Stop accepting new requests" }
                                p { class: "text-sm text-base-content/70",
                                    "This prevents new requests through this link. It does not cancel requests or GitHub invitations already in progress."
                                }
                                form {
                                    method: "post",
                                    action: "/accounts/{login}/links/{id_str}/revoke",
                                    class: "mt-2",
                                    button {
                                        r#type: "submit",
                                        class: "btn btn-error",
                                        "Stop accepting new requests"
                                    }
                                }
                            }
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
        assert!(html.contains("Stop accepting new requests"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(html.contains("push"));
        assert!(html.contains("2 / 5"));
        assert!(html.contains("Contractor onboarding"));
        assert!(!html.contains("Revoke this link"));
    }
}
