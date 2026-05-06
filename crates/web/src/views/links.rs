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

#[derive(Clone, Default, PartialEq)]
pub struct LinkFormValues {
    pub permission: String,
    pub approval_required: bool,
    pub max_uses: String,
    pub expires_in_days: String,
    pub internal_note: String,
    pub selected_repo_ids: Vec<u64>,
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
                header { class: "mb-6", h1 { class: "text-2xl font-bold", "New share link" } }
                form {
                    method: "post",
                    action: "/accounts/{login}/links",
                    class: "space-y-4 max-w-2xl",
                    div {
                        class: "form-control",
                        label { class: "label", "Permission level" }
                        select {
                            name: "permission",
                            class: "select select-bordered",
                            {perms.iter().map(|p| {
                                let selected = props.form.permission == *p;
                                rsx! { option { value: "{p}", selected: selected, "{p}" } }
                            })}
                        }
                    }
                    div {
                        class: "form-control",
                        label { class: "label cursor-pointer",
                            span { class: "label-text", "Require admin approval" }
                            input {
                                r#type: "checkbox",
                                name: "approval_required",
                                value: "true",
                                checked: props.form.approval_required,
                                class: "checkbox",
                            }
                        }
                    }
                    div {
                        class: "form-control",
                        label { class: "label", "Max uses (blank = unlimited)" }
                        input {
                            r#type: "number",
                            name: "max_uses",
                            value: "{props.form.max_uses}",
                            class: "input input-bordered",
                            min: "1",
                        }
                    }
                    div {
                        class: "form-control",
                        label { class: "label", "Expires in (days, blank = no expiration)" }
                        input {
                            r#type: "number",
                            name: "expires_in_days",
                            value: "{props.form.expires_in_days}",
                            class: "input input-bordered",
                            min: "1",
                        }
                    }
                    div {
                        class: "form-control",
                        label { class: "label", "Internal note (optional, admin-only)" }
                        textarea {
                            name: "internal_note",
                            class: "textarea textarea-bordered",
                            "{props.form.internal_note}"
                        }
                    }
                    fieldset {
                        class: "form-control",
                        legend { class: "label", "Repositories" }
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
                                        class: "checkbox",
                                    }
                                    span { "{full_name}" }
                                }
                            }
                        })}
                    }
                    div {
                        class: "form-control",
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
