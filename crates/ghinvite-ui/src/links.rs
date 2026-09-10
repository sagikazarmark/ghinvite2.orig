//! Invitation-link views: create form + detail page.
//!
//! The new invitation link form is split so the browser island (ADR 0001) can
//! render exactly what the server rendered by calling the same components:
//! [`LinkCreateFormPage`] is the Console page, [`LinkCreateForm`] is just the
//! `<form>` inside it, and [`PermissionSelect`] / [`RepositoryScopeGroup`] are
//! the two controls that [`crate::field::Field`] does not cover. All of them
//! are props-in, markup-out; the optional event-handler props
//! ([`LinkFormHandlers`]) are for the island and change nothing in
//! server-rendered HTML.
//!
//! The page also emits the island's mount points: the form is wrapped in
//! `<div id="link-form-island">`, followed by a `<script type="application/json"
//! id="link-form-props">` data block (a serialised
//! [`crate::link_form::LinkFormIslandProps`]) and one `<script type="module" src>`
//! tag. Neither script is inline executable code, so both are clean under the
//! `script-src 'self'` Content-Security-Policy; if the module is missing (no
//! bundle built), the browser 404s it and the plain form keeps working.

use crate::components::alert::{Alert, AlertColor};
use crate::components::button::{Button, ButtonColor};
use crate::field::{
    ControlHandlers, Field, FieldKind, described_by, error_text, help_text, listeners,
};
use crate::flash::Flash;
use crate::layouts::ConsoleLayout;
use crate::link_form::{
    CreateLinkForm, LINK_FORM_ISLAND_MODULE_SRC, LINK_FORM_ISLAND_PROPS_ID,
    LINK_FORM_ISLAND_ROOT_ID, LinkFormIslandProps, RepositoryChoice,
};
use chrono::{DateTime, Utc};
use dioform_core::Form;
use dioxus::prelude::*;
use ghinvite_core::{InvitationLink, Permission};

pub use crate::link_form::{LinkFormErrors, LinkFormValues};

#[derive(Clone, PartialEq, Props)]
pub struct LinkCreateFormPageProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    /// Available repositories, in the order the account makes them available.
    pub repos: Vec<RepositoryChoice>,
    pub form: LinkFormValues,
    /// The server's instant: what the route validated (or will validate)
    /// against. Handed to the island through the props blob so browser-side
    /// validation judges expiration from the same anchor.
    pub now: DateTime<Utc>,
}

/// The Console page for creating an invitation link: header, the form inside
/// the island root, the island's props blob and module script.
#[component]
pub fn LinkCreateFormPage(props: LinkCreateFormPageProps) -> Element {
    let login = props.account_login.clone();
    let action = format!("/console/accounts/{login}/links");
    let island_props = LinkFormIslandProps {
        action: action.clone(),
        values: props.form.clone(),
        repos: props.repos.clone(),
        now: props.now,
    }
    .script_json();

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "New invitation link · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("links".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-2",
                    p { class: "text-sm font-medium text-primary", "Invitation links" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "New invitation link" }
                    p { class: "max-w-2xl text-sm leading-6 text-base-content/70",
                        "Create a controlled invitation link that lets GitHub users request repository access to selected repositories."
                    }
                }
                // The island root: `dioxus-web` mounts here and re-renders
                // `LinkCreateForm` from the props blob below.
                div { id: LINK_FORM_ISLAND_ROOT_ID,
                    LinkCreateForm {
                        action: action.clone(),
                        form: props.form.clone(),
                        repos: props.repos.clone(),
                    }
                }
                // Data, not code: a JSON script block is inert for the browser
                // and for the CSP. `dangerous_inner_html` writes it verbatim,
                // which is why `script_json` escapes `<`.
                script { r#type: "application/json", id: LINK_FORM_ISLAND_PROPS_ID, dangerous_inner_html: "{island_props}" }
                // Module scripts are deferred wherever they sit, so this can
                // live next to the island it drives instead of in the shared
                // layout head; it runs after the whole document is parsed.
                script { r#type: "module", src: LINK_FORM_ISLAND_MODULE_SRC }
            },
        }
    }
}

/// The five collaborator permission levels, in the order the select lists
/// them. These are the exact strings the `<select>` submits and
/// [`ghinvite_core::Permission`] parses.
pub const PERMISSION_LEVELS: [&str; 5] = ["pull", "triage", "push", "maintain", "admin"];

/// The listeners the browser island attaches to [`LinkCreateForm`], one
/// bundle per control plus the form's `onsubmit`. All optional: the server
/// renders with [`LinkFormHandlers::default`] and, because SSR never emits
/// listener attributes, the markup is byte-identical with or without them
/// (tested). This is what lets the island render the very same component the
/// server rendered instead of a copy of its layout.
#[derive(Clone, Default, PartialEq)]
pub struct LinkFormHandlers {
    /// Fired when the form is submitted; the island's progressive submit
    /// preflight, which cancels the native POST only on a known blocker.
    pub onsubmit: Option<EventHandler<FormEvent>>,
    pub description: Option<dioxus_field::Binding<String>>,
    pub internal_note: ControlHandlers,
    pub permission: ControlHandlers,
    pub approval_required: ControlHandlers,
    pub max_uses: ControlHandlers,
    pub expires_in_days: ControlHandlers,
    pub repo_scope: RepositoryScopeHandlers,
}

/// Listeners for the repository checkbox group ([`RepositoryScopeGroup`]).
#[derive(Clone, Default, PartialEq)]
pub struct RepositoryScopeHandlers {
    /// Fired with `(repository id, checked)` when a checkbox changes.
    pub onchange: Option<EventHandler<(u64, bool)>>,
    /// Fired when focus leaves any checkbox.
    pub onblur: Option<EventHandler<FocusEvent>>,
}

/// The `<form>` of the new invitation link page: summary alert, the four
/// sections, and the submit button. Rendered by [`LinkCreateFormPage`] on the
/// server and again by the island in the browser, from the same inputs.
///
/// Rendered `name`s come from the shared model so the POST keys the server
/// parses and the controls the browser submits cannot drift apart.
#[component]
pub fn LinkCreateForm(
    /// `action` of the form; the route that handles the POST.
    action: String,
    form: LinkFormValues,
    /// Available repositories, in the order the account makes them available.
    repos: Vec<RepositoryChoice>,
    /// Listeners for the island; the server passes none.
    #[props(default)]
    handlers: LinkFormHandlers,
) -> Element {
    let fields = CreateLinkForm::fields();
    let mut form_listeners: Vec<Attribute> = Vec::new();
    if let Some(handler) = handlers.onsubmit {
        form_listeners.push(dioxus_elements::events::onsubmit(handler));
    }
    let approval_listeners = handlers.approval_required.attributes();

    rsx! {
        form { method: "post", action: "{action}", class: "max-w-3xl space-y-5", ..form_listeners,
            {if form.errors.summary.is_empty() {
                rsx! {}
            } else {
                rsx! {
                    Alert { id: "link-form-errors", color: AlertColor::Error, class: "items-start", role: "alert", aria_live: "polite",
                        div {
                            h2 { class: "font-semibold", dangerous_inner_html: "We couldn't create this invitation link" }
                            ul { class: "mt-1 list-disc space-y-1 pl-5 text-sm",
                                {form.errors.summary.iter().map(|message| rsx! { li { "{message}" } })}
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
                    Field {
                        id: "description",
                        name: fields.description().field_name().to_string(),
                        label: "Description",
                        kind: FieldKind::RegistryText { maxlength: Some(120) },
                        value: form.description.clone(),
                        required: true,
                        placeholder: "AI coding workshop",
                        help: "Visible only to admins. Use a short purpose or audience for this invitation link.",
                        error: form.errors.description.clone(),
                        binding: handlers.description,
                    }
                    Field {
                        id: "internal_note",
                        name: fields.internal_note().field_name().to_string(),
                        label: "Internal note",
                        kind: FieldKind::Textarea { rows: None },
                        value: form.internal_note.clone(),
                        placeholder: "Why this link exists",
                        help: "Optional admin-only notes. Not visible in the invitation request flow.",
                        oninput: handlers.internal_note.oninput,
                        onchange: handlers.internal_note.onchange,
                        onblur: handlers.internal_note.onblur,
                    }
                }
            }
            section { class: "mac-panel",
                div { class: "space-y-4 p-4",
                    div {
                        h2 { class: "text-base font-semibold", "Access configuration" }
                        p { class: "mt-1 text-sm text-base-content/65", "Choose the GitHub permission level and repositories included in this invitation link." }
                    }
                    PermissionSelect {
                        id: "permission",
                        name: fields.permission().field_name().to_string(),
                        value: form.permission.clone(),
                        help: "Use pull for read-only access. Maintain and admin can change repository settings.",
                        error: form.errors.permission.clone(),
                        onchange: handlers.permission.onchange,
                        onblur: handlers.permission.onblur,
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
                        label { class: "label cursor-pointer justify-start gap-3 whitespace-normal",
                            input { r#type: "checkbox", name: fields.approval_required().field_name().to_string(), value: "true", checked: form.approval_required, class: "checkbox", ..approval_listeners }
                            span { class: "label-text", "Require account admin approval before GitHub invitations are sent" }
                        }
                        p { class: "text-sm text-base-content/65", "Leave unchecked to auto-approve invitation requests that use this invitation link." }
                    }
                    div { class: "grid grid-cols-1 gap-4 md:grid-cols-2",
                        Field {
                            id: "max_uses",
                            name: fields.max_uses().field_name().to_string(),
                            label: "Max use",
                            kind: FieldKind::Number { min: Some(1), max: None },
                            value: form.max_uses.clone(),
                            placeholder: "Unlimited",
                            help: "Blank means unlimited invitation requests.",
                            error: form.errors.max_uses.clone(),
                            oninput: handlers.max_uses.oninput,
                            onchange: handlers.max_uses.onchange,
                            onblur: handlers.max_uses.onblur,
                        }
                        Field {
                            id: "expires_in_days",
                            name: fields.expires_in_days().field_name().to_string(),
                            label: "Expires in days",
                            kind: FieldKind::Number { min: Some(1), max: None },
                            value: form.expires_in_days.clone(),
                            help: "Default is 30 days. Blank creates an invitation link with no expiration.",
                            error: form.errors.expires_in_days.clone(),
                            oninput: handlers.expires_in_days.oninput,
                            onchange: handlers.expires_in_days.onchange,
                            onblur: handlers.expires_in_days.onblur,
                        }
                    }
                }
            }
            section { class: "mac-panel",
                div { class: "space-y-4 p-4",
                    RepositoryScopeGroup {
                        name: fields.repo_ids().field_name().to_string(),
                        repos: repos.clone(),
                        selected: form.selected_repo_ids.clone(),
                        help: "Select every repository this invitation link may grant access to.",
                        error: form.errors.repo_scope.clone(),
                        onchange: handlers.repo_scope.onchange,
                        onblur: handlers.repo_scope.onblur,
                    }
                }
            }
            div { class: "flex justify-end",
                Button { r#type: "submit", color: ButtonColor::Primary, "Create invitation link" }
            }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct PermissionSelectProps {
    /// DOM id of the select; prefixes the `{id}-help` / `{id}-error` ids.
    pub id: String,
    /// Form field name submitted with the POST.
    pub name: String,
    /// The level to pre-select. A value that is not one of
    /// [`PERMISSION_LEVELS`] (a tampered POST) selects nothing and is never
    /// echoed into the markup; the browser falls back to the first option.
    pub value: String,
    pub help: Option<String>,
    /// Field-level error; its presence sets `aria-invalid` and error styling.
    pub error: Option<String>,
    /// Fired when the selection changes (island only; ignored by SSR).
    pub onchange: Option<EventHandler<FormEvent>>,
    /// Fired when focus leaves the select (island only; ignored by SSR).
    pub onblur: Option<EventHandler<FocusEvent>>,
}

/// The permission-level `<select>`: a labelled control listing exactly the
/// five supported levels, with help and error wired like [`Field`].
#[component]
pub fn PermissionSelect(props: PermissionSelectProps) -> Element {
    let has_error = props.error.is_some();
    let help_id = format!("{}-help", props.id);
    let error_id = format!("{}-error", props.id);
    let described_by = described_by(
        props.help.as_ref().map(|_| help_id.as_str()),
        props.error.as_ref().map(|_| error_id.as_str()),
    );
    let listeners = listeners(None, props.onchange, props.onblur);
    let select_class = if has_error {
        "select select-bordered select-error w-full"
    } else {
        "select select-bordered w-full"
    };

    rsx! {
        div { class: "form-control gap-2",
            label { class: "label", r#for: "{props.id}", span { class: "label-text font-medium", "Permission level" } }
            select {
                id: "{props.id}",
                name: "{props.name}",
                class: "{select_class}",
                aria_describedby: described_by,
                aria_invalid: if has_error { "true" },
                ..listeners,
                {PERMISSION_LEVELS.iter().map(|level| {
                    let selected = props.value == *level;
                    rsx! { option { value: "{level}", selected: selected, "{level}" } }
                })}
            }
            {help_text(&help_id, props.help.as_deref())}
            {error_text(&error_id, props.error.as_deref())}
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct RepositoryScopeGroupProps {
    /// Form field name of every checkbox (repeated per repository).
    pub name: String,
    /// Available repositories, in the order the account makes them available.
    pub repos: Vec<RepositoryChoice>,
    /// Repository ids to render checked. Ids not in `repos` are ignored and
    /// never surface in the markup.
    pub selected: Vec<u64>,
    pub help: Option<String>,
    /// Section-level error rendered under the list; its presence sets the
    /// error border and extends `aria-describedby`.
    pub error: Option<String>,
    /// Fired with `(repository id, checked)` when a checkbox changes (island
    /// only; ignored by SSR).
    pub onchange: Option<EventHandler<(u64, bool)>>,
    /// Fired when focus leaves any checkbox (island only; ignored by SSR).
    pub onblur: Option<EventHandler<FocusEvent>>,
}

/// The repository-scope section body: heading, help, the checkbox group (or
/// the "no repositories" notice), and the error under it.
///
/// The group is the "control" for the repository scope: labelled by the
/// heading, described by the help text and, on failure, the error under the
/// list — the same `repo_ids` / `repo_ids-help` / `repo_ids-error` id scheme
/// as [`Field`]. ARIA does not permit `aria-invalid` on `role="group"`, so the
/// error is associated through `aria-describedby` and the visible border only.
#[component]
pub fn RepositoryScopeGroup(props: RepositoryScopeGroupProps) -> Element {
    let has_error = props.error.is_some();
    let described_by = described_by(
        props.help.as_ref().map(|_| "repo_ids-help"),
        props.error.as_ref().map(|_| "repo_ids-error"),
    );
    let group_class = if has_error {
        "max-h-80 space-y-1 overflow-y-auto rounded-box border border-error bg-base-200 p-3"
    } else {
        "max-h-80 space-y-1 overflow-y-auto rounded-box border border-base-300 bg-base-200 p-3"
    };

    rsx! {
        div {
            h2 { id: "repo_ids-label", class: "text-base font-semibold", "Repository scope" }
            {props.help.as_ref().map(|help| rsx! {
                p { id: "repo_ids-help", class: "mt-1 text-sm text-base-content/65", "{help}" }
            })}
        }
        {if props.repos.is_empty() {
            rsx! { div { class: "alert shadow-sm", span { "No repositories are available for this installation." } } }
        } else {
            rsx! {
                div {
                    id: "repo_ids",
                    role: "group",
                    class: "{group_class}",
                    aria_labelledby: "repo_ids-label",
                    aria_describedby: described_by,
                    {props.repos.iter().map(|repo| {
                        let id = repo.id;
                        let checked = props.selected.contains(&id);
                        let full_name = repo.full_name.clone();
                        // Built from plain closures (`ListenerCallback`, an
                        // `Rc`) rather than wrapped in a new `EventHandler`:
                        // `EventHandler::new` allocates in the component's
                        // scope and would not be freed until unmount, so a
                        // reactive caller would leak one per option per render.
                        let mut listeners: Vec<Attribute> = Vec::new();
                        if let Some(handler) = props.onchange {
                            listeners.push(dioxus_elements::events::onchange(
                                move |event: FormEvent| handler.call((id, event.checked())),
                            ));
                        }
                        if let Some(handler) = props.onblur {
                            listeners.push(dioxus_elements::events::onblur(handler));
                        }
                        rsx! {
                            label { class: "repo-choice-row flex cursor-pointer items-center gap-3 rounded-lg px-2 py-1.5 text-sm hover:bg-base-100",
                                input { r#type: "checkbox", name: "{props.name}", value: "{id}", checked: checked, class: "checkbox checkbox-sm", ..listeners }
                                span { class: "text-sm", "{full_name}" }
                            }
                        }
                    })}
                }
            }
        }}
        {error_text("repo_ids-error", props.error.as_deref())}
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
            active_nav: Some("links".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6 flex flex-col gap-3 md:flex-row md:items-center md:justify-between",
                    div {
                        h1 { class: "text-2xl font-bold", "{description}" }
                        p { class: "text-sm text-base-content/70", "Invitation code: ", span { class: "font-mono", "{slug}" } }
                    }
                    div { class: "flex items-center gap-3",
                        span { class: "{badge_class}", "{badge_label}" }
                        a { class: "btn btn-outline btn-sm", href: "/console/accounts/{login}/links/{id_str}/edit", "Edit details" }
                    }
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
    fn link_detail_offers_edit_details_for_active_expired_exhausted_and_revoked_links() {
        let now = dt("2026-05-04T12:00:00Z");
        for state in ["active", "expired", "exhausted", "revoked"] {
            let mut link = sample_link();
            match state {
                "expired" => link.expires_at = Some(now),
                "exhausted" => link.uses_count = 5,
                "revoked" => {
                    link.revoked_at = Some(now);
                    link.revoked_by = Some(701);
                }
                _ => {}
            }
            let id = link.id;
            let html = crate::testing::render(move || {
                rsx! {
                    LinkDetailPage {
                        signed_in_login: Some("admin".to_string()),
                        flash: None,
                        account_login: "acme",
                        link: link.clone(),
                        now,
                        invitation_url: "https://ghinvite.test/i/abcdEFGH01234567",
                    }
                }
            });

            assert!(
                html.contains(&format!(
                    "href=\"/console/accounts/acme/links/{id}/edit\">Edit details</a>"
                )),
                "{state} link must remain editable"
            );
            assert_eq!(html.matches(">Edit details</a>").count(), 1);
            if state == "active" {
                assert!(html.contains(&format!(
                    "<form method=\"post\" action=\"/console/accounts/acme/links/{id}/revoke\">"
                )));
                assert!(html.contains("href=\"#stop-link-modal\">Stop accepting new requests</a>"));
                assert!(html.contains(
                    "<button type=\"submit\" class=\"btn btn-error\">Confirm stop</button>"
                ));
            } else {
                assert!(!html.contains("/revoke"));
                assert!(
                    html.contains(
                        "This invitation link is no longer accepting invitation requests."
                    )
                );
            }
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

    // --- extracted form pieces (the island renders these directly) --------

    /// The whole page, and just the `<form>` it embeds, for the same inputs.
    fn render_page_and_form(
        form: LinkFormValues,
        repos: Vec<RepositoryChoice>,
    ) -> (String, String) {
        let (page_form, page_repos) = (form.clone(), repos.clone());
        let page = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: page_repos.clone(),
                    form: page_form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });
        let standalone = crate::testing::render(move || {
            rsx! {
                LinkCreateForm {
                    action: "/console/accounts/acme/links".to_string(),
                    form: form.clone(),
                    repos: repos.clone(),
                }
            }
        });
        (page, standalone)
    }

    /// The `<form>…</form>` slice of a rendered page: what the admin sees and
    /// submits, as opposed to the props blob that follows it.
    fn form_markup(page: &str) -> &str {
        let start = page.find("<form").expect("page renders a form");
        let end = page.find("</form>").expect("page closes the form") + "</form>".len();
        &page[start..end]
    }

    fn assert_create_submit_button(html: &str) {
        let form = form_markup(html);
        let button = form
            .split("<button ")
            .skip(1)
            .find(|button| {
                button.split_once('>').is_some_and(|(_, content)| {
                    content.starts_with("Create invitation link</button>")
                })
            })
            .expect("create form renders its labelled submit button");
        let attributes = button.split_once('>').unwrap().0;
        assert!(attributes.contains("type=\"submit\""));
        let classes = attributes
            .split_once("class=\"")
            .expect("submit button has classes")
            .1
            .split('"')
            .next()
            .unwrap()
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(classes, ["btn", "btn-primary"]);
        assert!(!attributes.contains("disabled"));
    }

    // --- island mount points ------------------------------------------------

    const APP_SCRIPT: &str = "<script src=\"/static/app.js\"></script>";
    const PROPS_SCRIPT_OPEN: &str = "<script type=\"application/json\" id=\"link-form-props\">";
    const MODULE_SCRIPT: &str =
        "<script type=\"module\" src=\"/assets/ghinvite-island.js\"></script>";

    /// Every `<script` on the page is one of: the shared app script, the JSON
    /// data block, the island module. Nothing inline and executable.
    fn assert_no_inline_executable_script(html: &str) {
        let app = html.matches(APP_SCRIPT).count();
        let data = html.matches(PROPS_SCRIPT_OPEN).count();
        let module = html.matches(MODULE_SCRIPT).count();
        assert_eq!(app, 1);
        assert_eq!(data, 1);
        assert_eq!(module, 1);
        assert_eq!(
            html.matches("<script").count(),
            app + data + module,
            "page contains an inline executable <script>"
        );
    }

    #[test]
    fn link_create_form_page_wraps_the_form_in_the_island_root() {
        let (page, form) = render_page_and_form(LinkFormValues::default(), acme_repos());

        let wrapped = format!("<div id=\"link-form-island\">{form}</div>");
        assert!(
            page.contains(&wrapped),
            "island root does not wrap exactly the form"
        );
        assert_eq!(page.matches("id=\"link-form-island\"").count(), 1);
    }

    #[test]
    fn link_create_form_page_emits_props_blob_then_module_script_after_the_island() {
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            permission: "push".to_string(),
            approval_required: true,
            max_uses: "7".to_string(),
            expires_in_days: "45".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![11],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                description: Some("d".to_string()),
                ..LinkFormErrors::default()
            },
        };
        let (page, _) = render_page_and_form(form.clone(), acme_repos());

        assert_no_inline_executable_script(&page);

        let blob_start = page.find(PROPS_SCRIPT_OPEN).unwrap();
        let blob = &page[blob_start + PROPS_SCRIPT_OPEN.len()..];
        let blob = &blob[..blob.find("</script>").unwrap()];
        let props: LinkFormIslandProps = serde_json::from_str(blob).unwrap();
        assert_eq!(
            props,
            LinkFormIslandProps {
                action: "/console/accounts/acme/links".to_string(),
                values: form,
                repos: acme_repos(),
                now: dt("2026-05-04T12:00:00Z"),
            }
        );
        assert!(blob.contains("\"now\":\"2026-05-04T12:00:00Z\""));

        let island_at = page.find("id=\"link-form-island\"").unwrap();
        let module_at = page.find(MODULE_SCRIPT).unwrap();
        assert!(
            island_at < blob_start,
            "props blob must follow the island root"
        );
        assert!(
            blob_start < module_at,
            "module script must follow the props blob"
        );
        assert!(module_at < page.find("</main>").unwrap());
    }

    #[test]
    fn link_create_form_page_props_blob_cannot_be_closed_by_a_description() {
        let form = LinkFormValues {
            description: "</script><script>alert(1)</script>".to_string(),
            ..LinkFormValues::default()
        };
        let (page, _) = render_page_and_form(form, acme_repos());

        assert_no_inline_executable_script(&page);
        // Three script elements in total: app, data block, module.
        assert_eq!(page.matches("</script>").count(), 3);
        assert!(page.contains("\\u003c/script>\\u003cscript>alert(1)\\u003c/script>"));
        // The form itself still escapes it as HTML.
        assert!(
            page.contains("value=\"&#60;/script&#62;&#60;script&#62;alert(1)&#60;/script&#62;\"")
        );
    }

    #[test]
    fn link_create_form_is_only_the_form_element() {
        let (_, html) = render_page_and_form(LinkFormValues::default(), acme_repos());

        assert!(html.starts_with(
            "<form method=\"post\" action=\"/console/accounts/acme/links\" class=\"max-w-3xl space-y-5\">"
        ));
        assert!(html.ends_with("</form>"));
        assert!(!html.contains("<html"));
        assert!(!html.contains("<title>"));
        assert!(!html.contains("<h1"));
        assert!(html.contains("Create invitation link"));
    }

    #[test]
    fn link_create_form_page_embeds_the_standalone_form_byte_for_byte() {
        // Fresh form and a re-render with every kind of error: the island
        // re-renders `LinkCreateForm` from the props blob, so what the page
        // emits and what the component emits on its own must not drift.
        let with_errors = LinkFormValues {
            description: "   ".to_string(),
            permission: "owner".to_string(),
            approval_required: true,
            max_uses: "abc".to_string(),
            expires_in_days: "0".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![11],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                description: Some("d".to_string()),
                permission: Some("p".to_string()),
                max_uses: Some("m".to_string()),
                expires_in_days: Some("e".to_string()),
                repo_scope: Some("r".to_string()),
            },
        };

        for (form, repos) in [
            (LinkFormValues::default(), acme_repos()),
            (with_errors.clone(), acme_repos()),
            (with_errors, vec![]),
        ] {
            let (page, standalone) = render_page_and_form(form, repos);
            assert!(
                page.contains(&standalone),
                "page does not embed the standalone form verbatim"
            );
            assert_eq!(page.matches("<form").count(), 1);
        }
    }

    #[test]
    fn link_create_form_with_every_handler_wired_renders_the_same_markup_as_without() {
        // The island renders `LinkCreateForm` with `LinkFormHandlers` filled
        // in; the server renders it with none. SSR emits no listener
        // attributes, so the two must be byte-identical — this is what makes
        // "the island renders the same component" hold literally.
        let with_errors = LinkFormValues {
            description: "   ".to_string(),
            permission: "owner".to_string(),
            approval_required: true,
            max_uses: "abc".to_string(),
            expires_in_days: "0".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![11, 999],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                description: Some("d".to_string()),
                permission: Some("p".to_string()),
                max_uses: Some("m".to_string()),
                expires_in_days: Some("e".to_string()),
                repo_scope: Some("r".to_string()),
            },
        };

        for (form, repos) in [
            (LinkFormValues::default(), acme_repos()),
            (with_errors.clone(), acme_repos()),
            (with_errors, vec![]),
        ] {
            let (plain_form, plain_repos) = (form.clone(), repos.clone());
            let plain = crate::testing::render(move || {
                rsx! {
                    LinkCreateForm {
                        action: "/console/accounts/acme/links".to_string(),
                        form: plain_form.clone(),
                        repos: plain_repos.clone(),
                    }
                }
            });
            let wired = crate::testing::render(move || {
                let description = use_signal(|| form.description.clone());
                let control = || ControlHandlers {
                    oninput: Some(EventHandler::new(|_event: FormEvent| {})),
                    onchange: Some(EventHandler::new(|_event: FormEvent| {})),
                    onblur: Some(EventHandler::new(|_event: FocusEvent| {})),
                };
                let handlers = LinkFormHandlers {
                    onsubmit: Some(EventHandler::new(|_event: FormEvent| {})),
                    description: Some(description.into()),
                    internal_note: control(),
                    permission: control(),
                    approval_required: control(),
                    max_uses: control(),
                    expires_in_days: control(),
                    repo_scope: RepositoryScopeHandlers {
                        onchange: Some(EventHandler::new(|_change: (u64, bool)| {})),
                        onblur: Some(EventHandler::new(|_event: FocusEvent| {})),
                    },
                };
                rsx! {
                    LinkCreateForm {
                        action: "/console/accounts/acme/links".to_string(),
                        form: form.clone(),
                        repos: repos.clone(),
                        handlers: handlers,
                    }
                }
            });

            assert_eq!(wired, plain);
            for listener in ["onsubmit", "oninput", "onchange", "onblur"] {
                assert!(
                    !wired.contains(listener),
                    "SSR emitted a {listener} listener"
                );
            }
        }
    }

    #[test]
    fn permission_select_renders_five_options_help_and_no_error() {
        let html = crate::testing::render(|| {
            rsx! {
                PermissionSelect {
                    id: "permission".to_string(),
                    name: "permission".to_string(),
                    value: "push".to_string(),
                    help: "Use pull for read-only access.".to_string(),
                }
            }
        });

        assert!(html.starts_with("<div class=\"form-control gap-2\">"));
        assert!(html.contains("<label class=\"label\" for=\"permission\">"));
        assert!(html.contains("<span class=\"label-text font-medium\">Permission level</span>"));
        assert!(html.contains(
            "<select id=\"permission\" name=\"permission\" class=\"select select-bordered w-full\" aria-describedby=\"permission-help\">"
        ));
        assert_eq!(html.matches("<option").count(), 5);
        assert!(html.contains("<option value=\"pull\">pull</option>"));
        // Dioxus SSR writes boolean attributes as `name=true`.
        assert!(html.contains("<option value=\"push\" selected=true>push</option>"));
        assert!(html.contains(
            "<p id=\"permission-help\" class=\"text-sm text-base-content/65\">Use pull for read-only access.</p>"
        ));
        assert!(!html.contains("permission-error"));
        assert!(!html.contains("aria-invalid"));
    }

    #[test]
    fn permission_select_with_error_and_unknown_value_selects_nothing() {
        let html = crate::testing::render(|| {
            rsx! {
                PermissionSelect {
                    id: "permission".to_string(),
                    name: "permission".to_string(),
                    value: "owner".to_string(),
                    help: "Use pull for read-only access.".to_string(),
                    error: "Choose a supported permission level.".to_string(),
                    onchange: move |_event: FormEvent| {},
                    onblur: move |_event: FocusEvent| {},
                }
            }
        });

        assert!(html.contains(
            "<select id=\"permission\" name=\"permission\" class=\"select select-bordered select-error w-full\" aria-describedby=\"permission-help permission-error\" aria-invalid=\"true\">"
        ));
        assert!(!html.contains("owner"));
        assert!(!html.contains(" selected"));
        assert!(html.contains(
            "<p id=\"permission-error\" class=\"text-sm font-medium text-error\">Choose a supported permission level.</p>"
        ));
        assert!(!html.contains("onchange"));
        assert!(!html.contains("onblur"));
    }

    #[test]
    fn repository_scope_group_renders_checkboxes_with_selection_and_no_error() {
        let html = crate::testing::render(|| {
            rsx! {
                RepositoryScopeGroup {
                    name: "repo_ids".to_string(),
                    repos: acme_repos(),
                    selected: vec![11],
                    help: "Select every repository.".to_string(),
                }
            }
        });

        assert!(html.contains(
            "<h2 id=\"repo_ids-label\" class=\"text-base font-semibold\">Repository scope</h2>"
        ));
        assert!(html.contains(
            "<p id=\"repo_ids-help\" class=\"mt-1 text-sm text-base-content/65\">Select every repository.</p>"
        ));
        assert!(html.contains(
            "<div id=\"repo_ids\" role=\"group\" class=\"max-h-80 space-y-1 overflow-y-auto rounded-box border border-base-300 bg-base-200 p-3\" aria-labelledby=\"repo_ids-label\" aria-describedby=\"repo_ids-help\">"
        ));
        assert_eq!(html.matches("name=\"repo_ids\"").count(), 2);
        assert!(html.contains(
            "<input type=\"checkbox\" name=\"repo_ids\" value=\"10\" class=\"checkbox checkbox-sm\"/>"
        ));
        assert!(html.contains(
            "<input type=\"checkbox\" name=\"repo_ids\" value=\"11\" checked=true class=\"checkbox checkbox-sm\"/>"
        ));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(!html.contains("repo_ids-error"));
        assert!(!html.contains("No repositories are available"));
    }

    #[test]
    fn repository_scope_group_with_error_and_handlers_renders_error_under_list() {
        let html = crate::testing::render(|| {
            rsx! {
                RepositoryScopeGroup {
                    name: "repo_ids".to_string(),
                    repos: acme_repos(),
                    selected: vec![],
                    help: "Select every repository.".to_string(),
                    error: "Repository scope is required.".to_string(),
                    onchange: move |_change: (u64, bool)| {},
                    onblur: move |_event: FocusEvent| {},
                }
            }
        });

        assert!(html.contains("border-error"));
        assert!(html.contains("aria-describedby=\"repo_ids-help repo_ids-error\""));
        assert!(html.contains(
            "<p id=\"repo_ids-error\" class=\"text-sm font-medium text-error\">Repository scope is required.</p>"
        ));
        assert!(
            html.find("name=\"repo_ids\"").unwrap() < html.find("id=\"repo_ids-error\"").unwrap()
        );
        assert!(!html.contains("aria-invalid"));
        assert!(!html.contains("onchange"));
        assert!(!html.contains("onblur"));
    }

    #[test]
    fn repository_scope_group_without_repositories_renders_notice_and_error() {
        let html = crate::testing::render(|| {
            rsx! {
                RepositoryScopeGroup {
                    name: "repo_ids".to_string(),
                    repos: vec![],
                    selected: vec![],
                    help: "Select every repository.".to_string(),
                    error: "Repository scope is required.".to_string(),
                }
            }
        });

        assert!(html.contains("No repositories are available for this installation."));
        assert!(html.contains("id=\"repo_ids-error\""));
        assert!(!html.contains("name=\"repo_ids\""));
        assert!(!html.contains("role=\"group\""));
    }

    #[test]
    fn link_form_values_round_trip_through_json() {
        // The island deserialises these from the props blob.
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            permission: "push".to_string(),
            approval_required: true,
            max_uses: "7".to_string(),
            expires_in_days: "45".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![10, 11],
            errors: LinkFormErrors {
                summary: vec!["summary".to_string()],
                description: Some("d".to_string()),
                ..LinkFormErrors::default()
            },
        };

        let json = serde_json::to_string(&form).unwrap();
        let back: LinkFormValues = serde_json::from_str(&json).unwrap();
        assert_eq!(back, form);

        let repos = acme_repos();
        let json = serde_json::to_string(&repos).unwrap();
        let back: Vec<RepositoryChoice> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, repos);
    }

    #[test]
    fn link_create_form_renders_sectioned_console_form() {
        let html = crate::testing::render(|| {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![RepositoryChoice {
                        id: 10,
                        full_name: "acme/api".to_string(),
                    }],
                    form: LinkFormValues::default(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("Access configuration"));
        assert!(html.contains("Link details"));
        assert!(html.contains("name=\"description\""));
        assert_eq!(html.matches("name=\"description\"").count(), 1);
        assert!(html.contains("required"));
        assert!(html.contains("maxlength=\"120\""));
        assert!(html.contains("AI coding workshop"));
        assert!(html.contains("aria-describedby=\"description-help\""));
        assert!(html.contains("id=\"description-help\""));
        assert!(!html.contains("aria-invalid"));
        assert!(html.contains("aria-describedby=\"internal_note-help\""));
        assert!(html.contains("id=\"internal_note-help\""));
        assert!(html.contains("aria-describedby=\"max_uses-help\""));
        assert!(html.contains("aria-describedby=\"expires_in_days-help\""));
        assert!(html.contains("name=\"max_uses\" value=\"\""));
        assert!(html.contains("name=\"expires_in_days\" value=\"30\""));
        assert!(html.contains("min=\"1\""));
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
                ..LinkFormErrors::default()
            },
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![
                        RepositoryChoice { id: 10, full_name: "acme/api".to_string() },
                        RepositoryChoice { id: 11, full_name: "acme/web".to_string() },
                    ],
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
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
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 1);
        assert!(html.contains("aria-describedby=\"description-help description-error\""));
        assert!(html.contains("input-error"));
        assert_eq!(html.matches("name=\"description\"").count(), 1);
        assert!(html.contains("value=\"push\" selected"));
        assert!(html.contains("name=\"approval_required\" value=\"true\" checked"));
        assert!(html.contains("name=\"max_uses\" value=\"7\""));
        assert!(html.contains("name=\"expires_in_days\" value=\"45\""));
        assert!(html.contains("Keep this note"));
        assert!(html.contains("value=\"10\" checked"));
        assert!(html.contains("value=\"11\""));
    }

    #[test]
    fn link_create_form_renders_numeric_guardrail_errors_and_preserved_raw_values() {
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            max_uses: "abc".to_string(),
            expires_in_days: "0".to_string(),
            selected_repo_ids: vec![10],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                max_uses: Some("Max use must be a whole number of 1 or more.".to_string()),
                expires_in_days: Some(
                    "Expiration must be a whole number of days, 1 or more.".to_string(),
                ),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![
                        RepositoryChoice { id: 10, full_name: "acme/api".to_string() },
                    ],
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"link-form-errors\""));
        assert!(html.contains("Fix the highlighted fields before creating this invitation link."));
        assert!(html.contains(
            "<p id=\"max_uses-error\" class=\"text-sm font-medium text-error\">Max use must be a whole number of 1 or more.</p>"
        ));
        assert!(html.contains(
            "<p id=\"expires_in_days-error\" class=\"text-sm font-medium text-error\">Expiration must be a whole number of days, 1 or more.</p>"
        ));
        assert!(html.contains("aria-describedby=\"max_uses-help max_uses-error\""));
        assert!(html.contains("aria-describedby=\"expires_in_days-help expires_in_days-error\""));
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 2);
        assert_eq!(html.matches("input-error").count(), 2);
        assert!(html.contains("name=\"max_uses\" value=\"abc\""));
        assert!(html.contains("name=\"expires_in_days\" value=\"0\""));
        assert!(html.contains("value=\"AI coding workshop\""));
        assert!(!html.contains("description-error"));
        assert!(html.contains("value=\"10\" checked"));
        assert_create_submit_button(&html);
        assert!(!html.contains("disabled"));
    }

    #[test]
    fn link_create_form_renders_permission_error_under_select_with_preserved_values() {
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            // A tampered value: not one of the rendered options, so no option
            // can be marked selected and the browser falls back to the first.
            permission: "owner".to_string(),
            approval_required: true,
            max_uses: "7".to_string(),
            expires_in_days: "45".to_string(),
            internal_note: "Keep this note".to_string(),
            selected_repo_ids: vec![11],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                permission: Some(
                    "Choose a supported permission level: pull, triage, push, maintain, or admin."
                        .to_string(),
                ),
                ..LinkFormErrors::default()
            },
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: acme_repos(),
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"link-form-errors\""));
        assert!(html.contains("Fix the highlighted fields before creating this invitation link."));
        assert!(html.contains(
            "<p id=\"permission-error\" class=\"text-sm font-medium text-error\">Choose a supported permission level: pull, triage, push, maintain, or admin.</p>"
        ));
        assert!(html.contains("aria-describedby=\"permission-help permission-error\""));
        assert!(html.contains("id=\"permission-help\""));
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 1);
        assert!(html.contains("select-error"));
        assert!(
            html.find("name=\"permission\"").unwrap()
                < html.find("id=\"permission-error\"").unwrap(),
            "error is rendered under the permission select"
        );
        // The tampered value is never echoed into the form markup (the props
        // blob carries the submitted values verbatim, JSON-escaped, so the
        // island starts from the same state); the select still lists exactly
        // the five supported levels, none pre-selected.
        assert!(!form_markup(&html).contains("owner"));
        assert_eq!(html.matches("<option").count(), 5);
        assert!(!html.contains("\" selected"));
        assert!(html.contains("value=\"AI coding workshop\""));
        assert!(html.contains("name=\"approval_required\" value=\"true\" checked"));
        assert!(html.contains("name=\"max_uses\" value=\"7\""));
        assert!(html.contains("name=\"expires_in_days\" value=\"45\""));
        assert!(html.contains("Keep this note"));
        assert!(html.contains("value=\"11\" checked"));
        assert!(!html.contains("value=\"10\" checked"));
        assert!(!html.contains("description-error"));
        assert!(!html.contains("repo_ids-error"));
        assert!(!html.contains("input-error"));
        assert_create_submit_button(&html);
        assert!(!html.contains("disabled"));
    }

    #[test]
    fn link_create_form_permission_select_is_described_by_help_without_error() {
        let html = crate::testing::render(|| {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: acme_repos(),
                    form: LinkFormValues::default(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"permission\""));
        assert!(html.contains("name=\"permission\""));
        assert!(html.contains("id=\"permission-help\""));
        assert!(html.contains("aria-describedby=\"permission-help\""));
        assert!(!html.contains("permission-error"));
        assert!(!html.contains("select-error"));
        assert!(!html.contains("aria-invalid"));
        assert!(html.contains("value=\"pull\" selected"));
        assert_eq!(html.matches("<option").count(), 5);
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
        ]
    }

    #[test]
    fn link_create_form_repository_group_is_labelled_and_described_without_error() {
        let html = crate::testing::render(|| {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: acme_repos(),
                    form: LinkFormValues::default(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"repo_ids-label\""));
        assert!(html.contains("id=\"repo_ids-help\""));
        assert!(html.contains("id=\"repo_ids\""));
        assert!(html.contains("role=\"group\""));
        assert!(html.contains("aria-labelledby=\"repo_ids-label\""));
        assert!(html.contains("aria-describedby=\"repo_ids-help\""));
        assert!(!html.contains("repo_ids-error"));
        assert!(!html.contains("aria-invalid"));
        assert!(!html.contains("border-error"));
        assert!(!html.contains("value=\"10\" checked"));
        assert!(!html.contains("value=\"11\" checked"));
    }

    #[test]
    fn link_create_form_renders_repository_scope_error_under_group_with_preserved_selection() {
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            // 11 is available and stays checked; 999 is unknown and must not
            // surface anywhere in the markup.
            selected_repo_ids: vec![999, 11],
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                repo_scope: Some(
                    "Repository scope is required. Select at least one available repository for this invitation link."
                        .to_string(),
                ),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: acme_repos(),
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("id=\"link-form-errors\""));
        assert!(html.contains(
            "<p id=\"repo_ids-error\" class=\"text-sm font-medium text-error\">Repository scope is required. Select at least one available repository for this invitation link.</p>"
        ));
        assert!(html.contains("role=\"group\""));
        assert!(html.contains("aria-labelledby=\"repo_ids-label\""));
        assert!(html.contains("aria-describedby=\"repo_ids-help repo_ids-error\""));
        // ARIA does not permit aria-invalid on role="group"; the error is
        // associated through aria-describedby and the visible border only.
        assert_eq!(html.matches("aria-invalid=\"true\"").count(), 0);
        assert!(html.contains("border-error"));
        assert!(
            html.find("name=\"repo_ids\"").unwrap() < html.find("id=\"repo_ids-error\"").unwrap(),
            "error is rendered under the repository list"
        );
        assert!(html.contains("value=\"11\" checked"));
        assert!(html.contains("value=\"10\""));
        assert!(!html.contains("value=\"10\" checked"));
        // The unknown id never reaches the form markup (the props blob carries
        // the submitted selection verbatim).
        assert!(!form_markup(&html).contains("999"));
        assert_eq!(html.matches("name=\"repo_ids\"").count(), 2);
        assert!(!html.contains("description-error"));
        assert!(!html.contains("input-error"));
        assert_create_submit_button(&html);
        assert!(!html.contains("disabled"));
    }

    #[test]
    fn link_create_form_renders_repository_scope_error_when_no_repositories_are_available() {
        let form = LinkFormValues {
            description: "AI coding workshop".to_string(),
            errors: LinkFormErrors {
                summary: vec![
                    "Fix the highlighted fields before creating this invitation link.".to_string(),
                ],
                repo_scope: Some("Repository scope is required. Select at least one available repository for this invitation link.".to_string()),
                ..LinkFormErrors::default()
            },
            ..LinkFormValues::default()
        };

        let html = crate::testing::render(move || {
            rsx! {
                LinkCreateFormPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    repos: vec![],
                    form: form.clone(),
                    now: dt("2026-05-04T12:00:00Z"),
                }
            }
        });

        assert!(html.contains("No repositories are available for this installation."));
        assert!(html.contains("id=\"repo_ids-error\""));
        assert!(html.contains("Repository scope is required."));
        assert!(!html.contains("name=\"repo_ids\""));
    }

    #[test]
    fn link_detail_renders_operational_context() {
        let link = sample_link();
        let html = crate::testing::render(move || {
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
