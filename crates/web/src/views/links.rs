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

    rsx! {
        DashboardLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "{slug} · {props.account_login}".to_string(),
            account_login: Some(props.account_login.clone()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex items-center gap-3",
                    h1 { class: "text-2xl font-bold", "{slug}" }
                    span { class: "{badge_class}", "{badge_label}" }
                }
                section { class: "card bg-base-100 shadow mb-6",
                    div { class: "card-body",
                        h2 { class: "card-title", "Share URL" }
                        p { class: "font-mono break-all", "{props.share_url}" }
                    }
                }
                section { class: "grid grid-cols-2 gap-4 mb-6",
                    div { class: "card bg-base-100 shadow", div { class: "card-body",
                        h3 { class: "font-semibold", "Permission" } p { "{perm}" }
                    }}
                    div { class: "card bg-base-100 shadow", div { class: "card-body",
                        h3 { class: "font-semibold", "Uses" }
                        p { "{props.link.uses_count} / {props.link.max_uses.map(|n| n.to_string()).unwrap_or_else(|| \"∞\".into())}" }
                    }}
                }
                {if active {
                    rsx! {
                        form {
                            method: "post",
                            action: "/accounts/{login}/links/{id_str}/revoke",
                            class: "mt-4",
                            button {
                                r#type: "submit",
                                class: "btn btn-error",
                                "Revoke this link"
                            }
                        }
                    }
                } else {
                    rsx! {
                        p { class: "opacity-70", "This link is no longer active." }
                    }
                }}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_form_defaults_are_safe() {
        let form = LinkFormValues::default();

        assert_eq!(form.permission, "pull");
        assert_eq!(form.expires_in_days, "30");
        assert_eq!(form.max_uses, "");
        assert!(!form.approval_required);
    }
}
